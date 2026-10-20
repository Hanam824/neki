use std::fs;
use std::io::Write;

use neki_hash::{HashAlgo, StoreDir};

use super::*;

struct Env {
    store: Option<LocalStore>,
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(
            StoreDir::new(dir.path().join("store")),
            dir.path().join("var"),
        )
        .unwrap();
        Self {
            store: Some(store),
            dir,
        }
    }

    fn store(&self) -> &LocalStore {
        self.store.as_ref().unwrap()
    }

    fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.dir.path().join(name);
        fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        self.store = None;
        neki_platform::fs::remove_tree(self.dir.path()).unwrap();
    }
}

fn no_progress(_: u64, _: Option<u64>) {}

/// A tar.gz with `root/a.txt`, `root/bin/tool` (executable) and `root/sub/`.
fn tar_gz() -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    let mut add = |path: &str, mode: u32, data: &[u8], kind: tar::EntryType| {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(kind);
        header.set_mode(mode);
        header.set_size(data.len() as u64);
        header.set_cksum();
        builder.append_data(&mut header, path, data).unwrap();
    };
    add("root/", 0o755, b"", tar::EntryType::Directory);
    add("root/a.txt", 0o644, b"hello", tar::EntryType::Regular);
    add(
        "root/bin/tool",
        0o755,
        b"#!/bin/sh\n",
        tar::EntryType::Regular,
    );
    add("root/sub/", 0o755, b"", tar::EntryType::Directory);
    builder.into_inner().unwrap().finish().unwrap()
}

fn zip_archive() -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o755);
    zip.add_directory("root/", options).unwrap();
    zip.start_file("root/run", options).unwrap();
    zip.write_all(b"run").unwrap();
    zip.start_file("root/data.txt", options.unix_permissions(0o644))
        .unwrap();
    zip.write_all(b"data").unwrap();
    zip.finish().unwrap().into_inner()
}

#[test]
fn file_urls() {
    assert_eq!(
        file_url_path("file:///tmp/a%20b").unwrap(),
        PathBuf::from("/tmp/a b")
    );
    assert_eq!(
        file_url_path("file:///C:/x/y").unwrap(),
        PathBuf::from("C:/x/y")
    );
    assert!(file_url_path("https://x").is_none());
    assert_eq!(file_url(Path::new("/tmp/x")), "file:///tmp/x");
    assert_eq!(file_url(Path::new(r"C:\x\y")), "file:///C:/x/y");
}

#[test]
fn fetch_url_checks_the_hash() {
    let env = Env::new();
    let file = env.file("data.bin", b"payload");
    let url = file_url(&file);
    let hash = Hash::of_bytes(HashAlgo::Sha256, b"payload");
    let spec = FetchSpec::Url {
        url: url.clone(),
        hash: hash.clone(),
    };
    let path = fetch(env.store(), "data.bin", &spec, &no_progress).unwrap();
    let stored = env.store().store_dir().to_path(&path);
    assert_eq!(fs::read(stored).unwrap(), b"payload");

    // Already valid: nothing is downloaded.
    fs::remove_file(&file).unwrap();
    assert_eq!(
        fetch(env.store(), "data.bin", &spec, &no_progress).unwrap(),
        path
    );

    env.file("data.bin", b"tampered");
    let err = fetch(
        env.store(),
        "other.bin",
        &FetchSpec::Url { url, hash },
        &no_progress,
    )
    .unwrap_err();
    assert!(matches!(err, Error::HashMismatch { .. }), "{err}");
}

#[test]
fn fetch_archive_unpacks_and_strips_the_root() {
    let env = Env::new();
    for (name, bytes) in [("a.tar.gz", tar_gz()), ("a.zip", zip_archive())] {
        let url = file_url(&env.file(name, &bytes));
        let spec = FetchSpec::Archive {
            url: url.clone(),
            hash: None,
            strip_root: true,
        };
        let path = fetch(env.store(), "source", &spec, &no_progress).unwrap();
        let tree = env.store().store_dir().to_path(&path);
        assert!(tree.is_dir(), "{name}");
        let exe = if name.ends_with("zip") {
            tree.join("run")
        } else {
            tree.join("bin/tool")
        };
        if cfg!(unix) {
            assert!(
                neki_platform::fs::is_executable(&fs::metadata(&exe).unwrap()),
                "{name}"
            );
        } else {
            assert!(exe.is_file());
        }

        // With the hash known, the same path is found without downloading.
        let hash = env
            .store()
            .query_path_info(&path)
            .unwrap()
            .unwrap()
            .nar_hash;
        let pinned = FetchSpec::Archive {
            url: "https://invalid.example/never-fetched".into(),
            hash: Some(hash),
            strip_root: true,
        };
        assert_eq!(
            fetch(env.store(), "source", &pinned, &no_progress).unwrap(),
            path
        );
    }
    let tree = env.dir.path().join("plain");
    unpack(&env.file("b.tar.gz", &tar_gz()), &tree, false).unwrap();
    assert_eq!(
        fs::read_to_string(tree.join("root/a.txt")).unwrap(),
        "hello"
    );
    assert!(tree.join("root/sub").is_dir());
}

#[test]
fn unpack_rejects_unsafe_entries() {
    let env = Env::new();
    for (i, path) in ["../evil", "/abs", "a/../../evil", "con.txt", "a:b"]
        .iter()
        .enumerate()
    {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(1);
        header.set_mode(0o644);
        // Bypass the builder's own path checks to craft a hostile archive.
        let name = &mut header.as_old_mut().name;
        name[..path.len()].copy_from_slice(path.as_bytes());
        header.set_cksum();
        let mut builder = tar::Builder::new(Vec::new());
        builder.append(&header, &b"x"[..]).unwrap();
        let archive = env.file(&format!("bad{i}.tar"), &builder.into_inner().unwrap());
        let dest = env.dir.path().join(format!("out{i}"));
        let err = unpack(&archive, &dest, false).unwrap_err();
        assert!(matches!(err, Error::Unpack { .. }), "{path}: {err}");
        assert!(!dest.exists());
    }
    assert!(!env.dir.path().parent().unwrap().join("evil").exists());

    // strip_root needs exactly one top-level directory.
    let mut builder = tar::Builder::new(Vec::new());
    for name in ["a", "b"] {
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_cksum();
        builder.append_data(&mut header, name, &b""[..]).unwrap();
    }
    let archive = env.file("two.tar", &builder.into_inner().unwrap());
    assert!(unpack(&archive, &env.dir.path().join("two"), true).is_err());
}

#[test]
fn unpacks_xz() {
    let env = Env::new();
    let mut tar = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(2);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "x/f", &b"hi"[..]).unwrap();
    let tar = tar.into_inner().unwrap();
    let mut xz = Vec::new();
    lzma_rs::xz_compress(&mut &tar[..], &mut xz).unwrap();
    let dest = env.dir.path().join("xz");
    unpack(&env.file("a.tar.xz", &xz), &dest, true).unwrap();
    assert_eq!(fs::read_to_string(dest.join("f")).unwrap(), "hi");
}

#[test]
fn entries_differing_only_by_case_are_left_out() {
    let env = Env::new();
    let mut tar = tar::Builder::new(Vec::new());
    for name in ["t/2/2621a", "t/2/2621A", "T/x", "t/ok"] {
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, name, &b"x"[..]).unwrap();
    }
    let archive = env.file("case.tar", &tar.into_inner().unwrap());
    let dest = env.dir.path().join("case");
    let skipped = unpack(&archive, &dest, false).unwrap();
    assert_eq!(skipped, ["t/2/2621A", "T/x"]);
    assert!(dest.join("t/2/2621a").is_file() && dest.join("t/ok").is_file());
    // The result is a valid (portable) store tree.
    neki_nar::dump(&dest, &mut Vec::new()).unwrap();
}
