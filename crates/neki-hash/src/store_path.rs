use std::fmt;
use std::path::{Path, PathBuf};

use crate::{Hash, Result};

/// Length in bytes of the truncated hash in a store path name.
pub const HASH_LEN: usize = 20;

/// The store directory, e.g. `/opt/neki/store`. It is part of every store
/// path hash, so stores with different directories never share paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreDir(PathBuf);

impl StoreDir {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Absolute filesystem location of `path`.
    pub fn to_path(&self, path: &StorePath) -> PathBuf {
        self.0.join(path.to_string())
    }

    /// Parse an absolute path inside this store.
    pub fn parse_path(&self, path: &Path) -> Result<StorePath> {
        todo!("M1: strip store dir, parse store path")
    }

    /// Compute a store path from `<kind>:<inner hash>:<store dir>:<name>`,
    /// hashed and truncated to [`HASH_LEN`] bytes (Nix's `makeStorePath`).
    pub fn make_path(&self, kind: &str, inner: &Hash, name: &str) -> Result<StorePath> {
        todo!("M1: make store path")
    }
}

/// A store path relative to its store directory: `<base32 hash>-<name>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StorePath {
    digest: [u8; HASH_LEN],
    name: String,
}

impl StorePath {
    /// Parse `<base32 hash>-<name>`.
    pub fn parse(s: &str) -> Result<Self> {
        todo!("M1: parse store path")
    }

    pub fn digest(&self) -> &[u8; HASH_LEN] {
        &self.digest
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for StorePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", crate::base32::encode(&self.digest), self.name)
    }
}
