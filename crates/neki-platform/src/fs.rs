//! Filesystem operations whose mechanism differs between OSes.

use std::io;
use std::path::Path;

/// Set or clear the read-only flag on a file or directory (store
/// immutability). Clearing it makes the entry writable by its owner only.
/// Follows symlinks, so callers skip them.
pub fn set_readonly(path: &Path, readonly: bool) -> io::Result<()> {
    let mut perms = std::fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = perms.mode();
        perms.set_mode(if readonly {
            mode & !0o222
        } else {
            mode | 0o200
        });
    }
    #[cfg(not(unix))]
    perms.set_readonly(readonly);
    std::fs::set_permissions(path, perms)
}

/// Create a hard link (store optimisation and profile merging).
pub fn hard_link(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::hard_link(src, dst)
}

/// Create `link` pointing at the existing directory `target`: a symlink on
/// Unix, a directory junction on Windows (needs no admin rights or Developer
/// Mode). The link always stores the absolute path of `target`, because
/// junctions can't be relative.
pub fn link_dir(target: &Path, link: &Path) -> io::Result<()> {
    let target = std::path::absolute(target)?;
    if !target.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", target.display()),
        ));
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, link)
    }
    #[cfg(windows)]
    {
        junction::create(&target, link)
    }
}

/// Remove a file, directory tree or link, clearing read-only flags first
/// (Windows can't delete read-only files, Unix can't delete entries of
/// read-only directories). Links are removed, never followed. Failures are
/// retried a few times, for files briefly held open by another process
/// such as a virus scanner. A missing `path` is not an error.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    const ATTEMPTS: u32 = 5;
    let mut attempt = 1;
    loop {
        let result = make_tree_writable(path).and_then(|()| {
            if std::fs::symlink_metadata(path)?.is_file() {
                std::fs::remove_file(path)
            } else {
                // Removes a link itself without following it.
                std::fs::remove_dir_all(path)
            }
        });
        match result {
            Err(err) if err.kind() == io::ErrorKind::NotFound && !path_exists(path) => {
                return Ok(());
            }
            Err(_) if attempt < ATTEMPTS => {
                std::thread::sleep(std::time::Duration::from_millis(20 << attempt));
                attempt += 1;
            }
            result => return result,
        }
    }
}

fn path_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Clear the read-only flag on `path` and everything below it, skipping links.
fn make_tree_writable(path: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.permissions().readonly() {
        set_readonly(path, false)?;
    }
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            make_tree_writable(&entry?.path())?;
        }
    }
    Ok(())
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

/// Whether the regular file at `path` (with metadata `meta`) is a program:
/// executable on Unix; a `.exe`, `.cmd`, `.bat` or `.com` file on Windows.
pub fn is_program(path: &Path, meta: &std::fs::Metadata) -> bool {
    if !meta.is_file() {
        return false;
    }
    if cfg!(windows) {
        path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            ["exe", "cmd", "bat", "com"].contains(&e.to_ascii_lowercase().as_str())
        })
    } else {
        is_executable(meta)
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

/// Identity of a file: two paths with the same `device` and `index` are hard
/// links to the same file, which has `links` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileId {
    pub device: u64,
    pub index: u64,
    pub links: u64,
}

/// The [`FileId`] of the regular file at `path`.
pub fn file_id(path: &Path) -> io::Result<FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(path)?;
        Ok(FileId {
            device: meta.dev(),
            index: meta.ino(),
            links: meta.nlink(),
        })
    }
    #[cfg(windows)]
    {
        windows_file_id(path)
    }
}

#[cfg(windows)]
#[allow(unsafe_code)] // one Win32 call; std has no stable API for this on Windows
fn windows_file_id(path: &Path) -> io::Result<FileId> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let file = std::fs::File::open(path)?;
    // SAFETY: all-zero bytes are a valid value of this plain C struct.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle stays valid while `file` lives, and `info` is a
    // valid, writable pointer to the struct the call fills in.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FileId {
        device: u64::from(info.dwVolumeSerialNumber),
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        links: u64::from(info.nNumberOfLinks),
    })
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

    #[test]
    fn remove_tree_clears_read_only_flags() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(tree.join("sub")).unwrap();
        std::fs::write(tree.join("sub/file"), "x").unwrap();
        set_readonly(&tree.join("sub/file"), true).unwrap();
        set_readonly(&tree.join("sub"), true).unwrap();
        remove_tree(&tree).unwrap();
        assert!(!path_exists(&tree));
        remove_tree(&tree).unwrap(); // already gone

        let file = dir.path().join("file");
        std::fs::write(&file, "x").unwrap();
        set_readonly(&file, true).unwrap();
        remove_tree(&file).unwrap();
        assert!(!path_exists(&file));
    }

    #[test]
    fn file_ids_see_hard_links() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        std::fs::write(&a, "x").unwrap();
        std::fs::write(&c, "x").unwrap();
        hard_link(&a, &b).unwrap();
        let (ia, ib, ic) = (
            file_id(&a).unwrap(),
            file_id(&b).unwrap(),
            file_id(&c).unwrap(),
        );
        assert_eq!((ia.device, ia.index), (ib.device, ib.index));
        assert_ne!((ia.device, ia.index), (ic.device, ic.index));
        assert_eq!(ia.links, 2);
        assert_eq!(ic.links, 1);
    }

    #[test]
    fn link_dir_points_at_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("file"), "x").unwrap();
        let link = dir.path().join("link");
        link_dir(&target, &link).unwrap();
        assert_eq!(std::fs::read_to_string(link.join("file")).unwrap(), "x");

        // Removing the link leaves the target alone.
        remove_tree(&link).unwrap();
        assert!(!path_exists(&link));
        assert!(target.join("file").exists());

        assert!(link_dir(&target.join("file"), &dir.path().join("bad")).is_err());
    }
}
