//! NAR (Nix ARchive): a deterministic serialization of a file tree. Only
//! contents, the executable bit and symlink targets are kept, never
//! timestamps, owners or permissions, so the same tree always gives the same
//! bytes (and the same hash).
//!
//! The format is Nix's. Every token is a string: a little-endian u64 length,
//! the bytes, then zero padding to a multiple of 8 bytes.
//!
//! ```text
//! nar       = "nix-archive-1" node
//! node      = "(" "type" ( regular | symlink | directory ) ")"
//! regular   = "regular" [ "executable" "" ] "contents" <bytes>
//! symlink   = "symlink" "target" <target>
//! directory = "directory" { "entry" "(" "name" <name> "node" node ")" }
//! ```
//!
//! Directory entries are sorted by name (byte order). Names and symlink
//! targets must be UTF-8, and names must be valid on every supported OS
//! ([`neki_platform::fs::is_portable_file_name`], and no two names in a
//! directory may differ only by case), so any NAR can be unpacked anywhere.

mod read;
mod write;

pub use read::restore;
pub use write::dump;

use std::path::PathBuf;

const MAGIC: &[u8] = b"nix-archive-1";

/// Longest name or symlink target accepted when reading.
const MAX_NAME: u64 = 4096;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("reading or writing the archive: {0}")]
    Stream(#[from] std::io::Error),
    #[error("invalid NAR: {0}")]
    Invalid(String),
    #[error("file name is not valid on every supported OS: {0:?}")]
    NonPortableName(String),
    #[error("{}: name or symlink target is not valid UTF-8", .0.display())]
    NonUtf8(PathBuf),
    #[error("{}: only files, directories and symlinks can be archived", .0.display())]
    UnsupportedFileType(PathBuf),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Attach `path` to an I/O error.
fn at(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Check one directory entry name: it must be portable, and must not differ
/// only by case from an earlier name in `seen` (the lowercased names of the
/// same directory), since the two would collide on Windows and macOS.
fn check_name(name: &str, seen: &mut std::collections::HashSet<String>) -> Result<()> {
    if !neki_platform::fs::is_portable_file_name(name) {
        return Err(Error::NonPortableName(name.to_string()));
    }
    if !seen.insert(name.to_lowercase()) {
        return Err(Error::NonPortableName(format!(
            "{name} (differs from another entry only by case)"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
