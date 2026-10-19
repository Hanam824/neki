//! The store: immutable, content-addressed paths plus their metadata.

#![allow(unused_variables)] // skeleton: stub bodies

mod gc;
mod local;

pub use gc::{GcOptions, GcResult};
pub use local::LocalStore;

use std::collections::BTreeSet;
use std::path::Path;

use neki_hash::{Hash, StoreDir, StorePath};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Hash(#[from] neki_hash::Error),
    #[error(transparent)]
    Nar(#[from] neki_nar::Error),
    #[error("path is not valid in the store: {0}")]
    NotValid(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

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
    fn store_dir(&self) -> &StoreDir;

    fn query_path_info(&self, path: &StorePath) -> Result<Option<PathInfo>>;

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
