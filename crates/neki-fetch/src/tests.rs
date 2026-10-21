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
    // Made on Unix: on Windows the writer defaults to DOS, which has no
    // executable bit.
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .system(zip::System::Unix)
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
        assert!(
            neki_platform::fs::is_executable(&exe, &fs::metadata(&exe).unwrap()),
            "{name}"
        );

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

/// A hard link is unpacked as a copy of its target, which must be a file
/// from the archive itself, not something a symlink in it points to.
#[test]
fn hard_links_copy_only_files_from_the_archive() {
    let env = Env::new();
    let secret = env.file("secret", b"secret");
    let entry = |kind, name: &str, target: &Path| {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(kind);
        header.set_size(0);
        header.set_mode(0o644);
        header.set_path(name).unwrap();
        header.set_link_name(target).unwrap();
        header.set_cksum();
        header
    };

    let mut builder = tar::Builder::new(Vec::new());
    let mut file = tar::Header::new_gnu();
    file.set_size(2);
    file.set_mode(0o644);
    file.set_cksum();
    builder.append_data(&mut file, "file", &b"ok"[..]).unwrap();
    let link = entry(tar::EntryType::Link, "copy", Path::new("file"));
    builder.append(&link, &b""[..]).unwrap();
    let archive = env.file("good.tar", &builder.into_inner().unwrap());
    let dest = env.dir.path().join("good");
    unpack(&archive, &dest, false).unwrap();
    assert_eq!(fs::read(dest.join("copy")).unwrap(), b"ok");

    let mut builder = tar::Builder::new(Vec::new());
    let symlink = entry(tar::EntryType::Symlink, "out", &secret);
    builder.append(&symlink, &b""[..]).unwrap();
    let link = entry(tar::EntryType::Link, "copy", Path::new("out"));
    builder.append(&link, &b""[..]).unwrap();
    let archive = env.file("bad.tar", &builder.into_inner().unwrap());
    let dest = env.dir.path().join("bad");
    // (On Windows the symlink entry itself is refused.)
    assert!(unpack(&archive, &dest, false).is_err());
    assert!(!dest.exists());
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

/// Run git in `repo` (with a committer set), returning its output.
fn git_in(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A local repository with a script committed as executable (set through
/// the index, so it works where the OS has no executable bit).
#[test]
fn git_checkout_marks_executables() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    fs::write(repo.join("run.sh"), "echo hi\n").unwrap();
    fs::write(repo.join("README"), "hi\n").unwrap();
    let git = |args: &[&str]| git_in(&repo, args);
    git(&["init", "--quiet"]);
    git(&["add", "run.sh", "README"]);
    git(&["update-index", "--chmod=+x", "run.sh"]);
    git(&["commit", "--quiet", "-m", "init"]);
    let rev = git(&["rev-parse", "HEAD"]);

    let dest = dir.path().join("checkout");
    let url = repo.to_str().unwrap();
    git_checkout(url, rev.trim(), &dest).unwrap();
    let is_exec = |name: &str| {
        let path = dest.join(name);
        neki_platform::fs::is_executable(&path, &fs::metadata(&path).unwrap())
    };
    assert!(is_exec("run.sh"));
    assert!(!is_exec("README"));
    assert!(!dest.join(".git").exists());
    neki_platform::fs::remove_tree(dir.path()).unwrap();
}

/// A URL or ref starting with `-` would be read by git as an option (such
/// as `--upload-pack=<command>`), and a rev must be a commit hash.
#[test]
fn git_arguments_are_not_options() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let url = format!("--upload-pack=touch {};false", marker.display());
    let rev = "0".repeat(40);
    assert!(git_checkout(&url, &rev, &dir.path().join("a")).is_err());
    assert!(git_checkout(&url, ".", &dir.path().join("b")).is_err());
    assert!(git_resolve(&url, None).is_err());
    assert!(git_resolve(dir.path().to_str().unwrap(), Some("--upload-pack=x")).is_err());
    assert!(!marker.exists());
    // Not a commit hash: refused before anything is fetched.
    let err = git_checkout(dir.path().to_str().unwrap(), "main", &dir.path().join("c"));
    assert!(err.unwrap_err().to_string().contains("full commit hash"));
    neki_platform::fs::remove_tree(dir.path()).unwrap();
}

/// A committed symlink is checked out as a symlink, or, where the OS has
/// none, fails clearly instead of becoming a plain file.
#[test]
fn git_checkout_keeps_or_refuses_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| git_in(&repo, args);
    git(&["init", "--quiet"]);
    // Through the index, so the test repo can be made on any OS.
    let blob = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["hash-object", "-w", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child.stdin.take().unwrap().write_all(b"target")?;
            child.wait_with_output()
        })
        .unwrap();
    let blob = String::from_utf8(blob.stdout).unwrap();
    git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("120000,{},link", blob.trim()),
    ]);
    git(&["commit", "--quiet", "-m", "init"]);
    let rev = git(&["rev-parse", "HEAD"]);

    let dest = dir.path().join("checkout");
    match git_checkout(repo.to_str().unwrap(), rev.trim(), &dest) {
        Ok(()) => assert!(
            fs::symlink_metadata(dest.join("link"))
                .unwrap()
                .file_type()
                .is_symlink()
        ),
        Err(err) => assert!(err.to_string().contains("symbolic links"), "{err}"),
    }
    neki_platform::fs::remove_tree(dir.path()).unwrap();
}
