use std::fs;
use std::path::Path;

use super::*;

/// A NAR built by hand from its tokens, following the format spec.
fn nar(tokens: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for token in tokens {
        out.extend_from_slice(&(token.len() as u64).to_le_bytes());
        out.extend_from_slice(token);
        out.resize(out.len().next_multiple_of(8), 0);
    }
    out
}

fn dump_to_vec(path: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    dump(path, &mut out).unwrap();
    out
}

fn restore_bytes(bytes: &[u8]) -> (tempfile::TempDir, Result<()>) {
    let dir = tempfile::tempdir().unwrap();
    let result = restore(&mut &bytes[..], &dir.path().join("out"));
    (dir, result)
}

#[test]
fn single_file_matches_spec() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f");
    fs::write(&file, "hello").unwrap();
    let expected = nar(&[
        b"nix-archive-1",
        b"(",
        b"type",
        b"regular",
        b"contents",
        b"hello",
        b")",
    ]);
    assert_eq!(dump_to_vec(&file), expected);
}

#[test]
fn directory_matches_spec() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("b"), "2").unwrap();
    fs::write(dir.path().join("a"), "1").unwrap();
    #[rustfmt::skip]
    let expected = nar(&[
        b"nix-archive-1", b"(", b"type", b"directory",
        b"entry", b"(", b"name", b"a", b"node", b"(", b"type", b"regular", b"contents", b"1", b")", b")",
        b"entry", b"(", b"name", b"b", b"node", b"(", b"type", b"regular", b"contents", b"2", b")", b")",
        b")",
    ]);
    assert_eq!(dump_to_vec(dir.path()), expected);
}

#[test]
fn round_trips_a_tree() {
    let src = tempfile::tempdir().unwrap();
    let root = src.path();
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::create_dir(root.join("empty")).unwrap();
    fs::write(root.join("empty-file"), "").unwrap();
    fs::write(root.join("nine-bytes"), "123456789").unwrap();
    fs::write(root.join("bin/tool"), "#!/bin/sh\necho hi\n").unwrap();
    neki_platform::fs::set_executable(&root.join("bin/tool")).unwrap();
    if cfg!(unix) {
        neki_platform::fs::symlink(Path::new("bin/tool"), &root.join("link")).unwrap();
    }

    let bytes = dump_to_vec(root);
    let (dest, result) = restore_bytes(&bytes);
    result.unwrap();
    let out = dest.path().join("out");
    assert_eq!(dump_to_vec(&out), bytes);
    assert_eq!(
        fs::read_to_string(out.join("nine-bytes")).unwrap(),
        "123456789"
    );
    if cfg!(unix) {
        assert!(neki_platform::fs::is_executable(
            &fs::metadata(out.join("bin/tool")).unwrap()
        ));
        assert!(!neki_platform::fs::is_executable(
            &fs::metadata(out.join("nine-bytes")).unwrap()
        ));
        assert_eq!(
            fs::read_link(out.join("link")).unwrap(),
            Path::new("bin/tool")
        );
    }
}

#[test]
fn executable_flag_is_archived() {
    if !cfg!(unix) {
        return; // Windows has no executable bit to read.
    }
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("run");
    fs::write(&file, "x").unwrap();
    neki_platform::fs::set_executable(&file).unwrap();
    let expected = nar(&[
        b"nix-archive-1",
        b"(",
        b"type",
        b"regular",
        b"executable",
        b"",
        b"contents",
        b"x",
        b")",
    ]);
    assert_eq!(dump_to_vec(&file), expected);
}

#[test]
fn dump_rejects_non_portable_names() {
    if !cfg!(unix) {
        return; // Windows can't create these names in the first place.
    }
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("con.txt"), "").unwrap();
    assert!(matches!(
        dump(dir.path(), &mut Vec::new()),
        Err(Error::NonPortableName(_))
    ));
}

/// A directory NAR with one regular file per name, in the given order.
fn dir_nar(names: &[&str]) -> Vec<u8> {
    let mut tokens: Vec<&[u8]> = vec![b"nix-archive-1", b"(", b"type", b"directory"];
    for name in names {
        tokens.extend([&b"entry"[..], b"(", b"name", name.as_bytes(), b"node"]);
        tokens.extend([&b"("[..], b"type", b"regular", b"contents", b"", b")", b")"]);
    }
    tokens.push(b")");
    nar(&tokens)
}

#[test]
fn restore_accepts_valid_directory() {
    let (dir, result) = restore_bytes(&dir_nar(&["A", "b", "c.txt"]));
    result.unwrap();
    assert!(dir.path().join("out/c.txt").is_file());
}

#[test]
fn restore_rejects_bad_entries() {
    // (entries, why, how many entries are created before the bad one is rejected)
    for (names, why, created) in [
        (&["b", "a"][..], "out of order", 1),
        (&["a", "a"], "duplicate", 1),
        (&["CON"], "reserved name", 0),
        (&["a:b"], "colon", 0),
        (&["a/b"], "slash", 0),
        (&[".."], "parent directory", 0),
        (&["A", "a"], "case collision", 1),
    ] {
        let (dir, result) = restore_bytes(&dir_nar(names));
        assert!(result.is_err(), "{why}: {names:?} should be rejected");
        // The bad entry is rejected before anything is created for it.
        assert_eq!(
            fs::read_dir(dir.path().join("out")).unwrap().count(),
            created,
            "{why}"
        );
    }
}

#[test]
fn restore_rejects_malformed_archives() {
    let file = nar(&[
        b"nix-archive-1",
        b"(",
        b"type",
        b"regular",
        b"contents",
        b"hi",
        b")",
    ]);
    assert!(restore_bytes(&file).1.is_ok());

    let mut trailing = file.clone();
    trailing.push(0);
    let truncated = &file[..file.len() - 4];
    let mut bad_padding = file.clone();
    // The first padding byte after "hi": the tokens before it, then its 8-byte length, then its 2 bytes.
    let contents_padding =
        nar(&[b"nix-archive-1", b"(", b"type", b"regular", b"contents"]).len() + 8 + 2;
    bad_padding[contents_padding] = 1;
    let bad_magic = nar(&[
        b"nix-archive-2",
        b"(",
        b"type",
        b"regular",
        b"contents",
        b"hi",
        b")",
    ]);
    let bad_type = nar(&[b"nix-archive-1", b"(", b"type", b"fifo", b")"]);

    for (bytes, why) in [
        (&trailing[..], "trailing data"),
        (truncated, "truncated"),
        (&bad_padding, "non-zero padding"),
        (&bad_magic, "wrong magic"),
        (&bad_type, "unknown node type"),
    ] {
        assert!(
            matches!(restore_bytes(bytes).1, Err(Error::Invalid(_))),
            "{why}"
        );
    }
}

#[test]
fn restore_refuses_existing_destination() {
    let file = nar(&[
        b"nix-archive-1",
        b"(",
        b"type",
        b"regular",
        b"contents",
        b"hi",
        b")",
    ]);
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("out"), "existing").unwrap();
    assert!(restore(&mut &file[..], &dir.path().join("out")).is_err());
    assert_eq!(
        fs::read_to_string(dir.path().join("out")).unwrap(),
        "existing"
    );
}
