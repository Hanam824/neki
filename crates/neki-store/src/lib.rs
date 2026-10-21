//! The store: immutable, content-addressed paths plus their metadata.

mod db;
mod gc;
mod local;
mod references;

pub use gc::{GcOptions, GcResult, Root, TempRoot};
pub use local::{EntryKind, LocalStore, TempEntry, copy_tree, file_hash, nar_hash};
pub use references::scan_references;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use neki_hash::{Hash, StoreDir, StorePath};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{}: {source}", path.display())]
    File {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("store DB: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("store DB is corrupt: {0}")]
    Corrupt(String),
    #[error("{} changed while it was being added to the store", .0.display())]
    Changed(PathBuf),
    #[error("hash mismatch for {what}: expected {expected}, got {got}")]
    HashMismatch {
        what: String,
        expected: String,
        got: String,
    },
    #[error(transparent)]
    Derivation(#[from] neki_derivation::Error),
    #[error(transparent)]
    Hash(#[from] neki_hash::Error),
    #[error(transparent)]
    Nar(#[from] neki_nar::Error),
    #[error("path is not valid in the store: {0}")]
    NotValid(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// File in a package that lists the directories (relative to the package,
/// one per line) holding its programs. Without it, programs are in `bin`.
pub const BIN_DIRS_FILE: &str = "neki-support/bin-dirs";

/// The directories of the package at `root` that hold its programs.
pub fn bin_dirs(root: &Path) -> Vec<PathBuf> {
    match std::fs::read_to_string(root.join(BIN_DIRS_FILE)) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| {
                if l == "." {
                    root.to_path_buf()
                } else {
                    root.join(l)
                }
            })
            .collect(),
        Err(_) => vec![root.join("bin")],
    }
}

/// Attach `path` to an I/O error.
fn at(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::File {
        path: path.to_path_buf(),
        source,
    }
}

/// Metadata of a valid store path (one row of the store DB).
#[derive(Debug, Clone)]
pub struct PathInfo {
    pub path: StorePath,
    pub nar_hash: Hash,
    pub nar_size: u64,
    pub references: BTreeSet<StorePath>,
    pub deriver: Option<StorePath>,
    /// Unix timestamp of registration.
    pub registration_time: i64,
    pub signatures: Vec<String>,
}

/// Operations shared by the local store and remote binary caches.
pub trait Store {
    /// The store's directory, which every path is under.
    fn store_dir(&self) -> &StoreDir;

    /// What's recorded about `path`, or `None` if it isn't valid (not in the
    /// store, or not registered yet).
    fn query_path_info(&self, path: &StorePath) -> Result<Option<PathInfo>>;

    /// Whether `path` is in the store and registered.
    fn is_valid_path(&self, path: &StorePath) -> Result<bool> {
        Ok(self.query_path_info(path)?.is_some())
    }

    /// Copy a file or directory into the store, addressed by its NAR hash.
    fn add_path(&self, name: &str, src: &Path) -> Result<StorePath>;

    /// Add a text file (e.g. a `.drv`) that refers to `references`.
    fn add_text(
        &self,
        name: &str,
        text: &str,
        references: &BTreeSet<StorePath>,
    ) -> Result<StorePath>;

    /// `paths` and everything reachable from them through references.
    fn closure(&self, paths: &[StorePath]) -> Result<BTreeSet<StorePath>>;
}

#[cfg(test)]
mod tests;
