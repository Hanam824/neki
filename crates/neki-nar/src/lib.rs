//! NAR (Nix ARchive): a deterministic serialization of a file tree. Only
//! contents, the executable bit and symlink targets are kept, never
//! timestamps, owners or permissions.

#![allow(unused_variables)] // skeleton: stub bodies

use std::io::{Read, Write};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid NAR: {0}")]
    Invalid(String),
    #[error("file name is not valid on every supported OS: {0:?}")]
    NonPortableName(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Serialize the tree at `path` as a NAR into `out`.
pub fn dump(path: &Path, out: &mut impl Write) -> Result<()> {
    todo!("M1: NAR dump")
}

/// Unpack a NAR from `input` into `dest`, which must not exist. Names that
/// are not portable (`neki_platform::fs::is_portable_file_name`) are rejected;
/// the executable bit is applied only on Unix.
pub fn restore(input: &mut impl Read, dest: &Path) -> Result<()> {
    todo!("M1: NAR restore")
}
