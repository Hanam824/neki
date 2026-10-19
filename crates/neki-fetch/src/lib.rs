//! Fetchers: download sources into the store as fixed-output paths.

#![allow(unused_variables)] // skeleton: stub bodies

use neki_hash::{Hash, StorePath};
use neki_store::Store;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Store(#[from] neki_store::Error),
    #[error("hash mismatch for {url}: expected {expected}, got {got}")]
    HashMismatch {
        url: String,
        expected: String,
        got: String,
    },
    #[error("fetching {url} failed: {reason}")]
    Failed { url: String, reason: String },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What to fetch. Everything is pinned: by content hash, or by git revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchSpec {
    /// A single file.
    Url { url: String, hash: Hash },
    /// An archive (zip, tar.gz, tar.xz) unpacked into a directory;
    /// `strip_root` removes a single top-level directory.
    Archive {
        url: String,
        hash: Hash,
        strip_root: bool,
    },
    /// A git checkout at `rev`, without any line-ending conversion.
    Git { url: String, rev: String },
}

pub fn fetch(store: &dyn Store, spec: &FetchSpec) -> Result<StorePath> {
    todo!("M3: fetchers")
}
