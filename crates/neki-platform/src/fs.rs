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
/// contents can be unpacked anywhere (rejects Windows reserved names such as
/// `CON` or `NUL`, the characters `<>:"/\|?*`, and trailing dots or spaces).
pub fn is_portable_file_name(name: &str) -> bool {
    todo!("M1: portable file name rules")
}
