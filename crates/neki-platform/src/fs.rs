//! Filesystem operations whose mechanism differs between OSes.

use std::io;
use std::path::Path;

/// Set or clear the read-only flag on a file or directory (store immutability).
pub fn set_readonly(path: &Path, readonly: bool) -> io::Result<()> {
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_readonly(readonly);
    std::fs::set_permissions(path, perms)
}

/// Create a hard link (store optimisation and profile merging).
pub fn hard_link(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::hard_link(src, dst)
}

/// Create `link` pointing at the directory `target`: a symlink on Unix, a
/// directory junction on Windows (needs no admin rights or Developer Mode).
pub fn link_dir(target: &Path, link: &Path) -> io::Result<()> {
    todo!("M2: symlink on Unix, junction on Windows")
}

/// Remove a directory tree, clearing read-only flags and retrying files that
/// are temporarily locked by another process.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    todo!("M6: clear read-only flags, retry locked files")
}

/// Whether `name` is a valid file name on every supported OS, so that store
/// contents can be unpacked anywhere. Rejects `.` and `..`, Windows reserved
/// names (`CON`, `NUL`, `COM1`, … with or without an extension), the
/// characters `<>:"/\|?*` and control characters, trailing dots or spaces,
/// and names longer than 255 bytes.
pub fn is_portable_file_name(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." || name.len() > 255 {
        return false;
    }
    if name
        .chars()
        .any(|c| c.is_control() || r#"<>:"/\|?*"#.contains(c))
    {
        return false;
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let numbered = |prefix: &str| {
        stem.len() == 4 && stem.starts_with(prefix) && matches!(stem.as_bytes()[3], b'1'..=b'9')
    };
    !(["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str()) || numbered("COM") || numbered("LPT"))
}

/// Whether `meta` marks the file executable. Windows has no executable bit,
/// so there this is always false.
pub fn is_executable(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// Make a file executable by everyone who can read it. No-op on Windows.
pub fn set_executable(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(perms.mode() | ((perms.mode() & 0o444) >> 2));
        std::fs::set_permissions(path, perms)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Create a symbolic link at `link` pointing to `target`. Not supported on
/// Windows yet, where symlinks need Developer Mode or admin rights (see
/// `docs/PLAN.md`, Future).
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(not(unix))]
    {
        let _ = (target, link);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symbolic links in store contents aren't supported on Windows yet",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_names() {
        for ok in [
            "a",
            "hello.txt",
            ".hidden",
            "icon.d",
            "CONSOLE",
            "COM0",
            "lpt10",
            "ünïcode",
            "a b",
        ] {
            assert!(is_portable_file_name(ok), "{ok:?} should be portable");
        }
        for bad in [
            "",
            ".",
            "..",
            "CON",
            "con",
            "con.d",
            "Nul.txt",
            "COM1",
            "lpt9.log",
            "aux.tar.gz",
            "a:b",
            "a/b",
            "a\\b",
            "what?",
            "star*",
            "pipe|",
            "quote\"",
            "<x>",
            "tab\t",
            "trailing.",
            "trailing ",
        ] {
            assert!(
                !is_portable_file_name(bad),
                "{bad:?} should not be portable"
            );
        }
        assert!(!is_portable_file_name(&"x".repeat(256)));
    }
}
