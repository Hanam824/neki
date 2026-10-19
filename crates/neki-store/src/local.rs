use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use neki_hash::{StoreDir, StorePath};

use crate::{GcOptions, GcResult, PathInfo, Result, Store};

/// A store on the local filesystem: `<store dir>/<hash>-<name>`, with the
/// metadata DB, GC roots and locks under the state directory.
pub struct LocalStore {
    dir: StoreDir,
    state_dir: PathBuf,
}

impl LocalStore {
    /// Open the store, creating its directories and DB if needed.
    pub fn open(dir: StoreDir, state_dir: PathBuf) -> Result<Self> {
        todo!("M2: create dirs, open DB")
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Register `path` as a GC root named `name` (kept in the DB, not as links).
    pub fn add_root(&self, name: &str, path: &StorePath) -> Result<()> {
        todo!("M6: GC roots")
    }

    pub fn collect_garbage(&self, opts: &GcOptions) -> Result<GcResult> {
        todo!("M6: garbage collection")
    }

    /// Re-hash every valid path; returns the paths whose contents changed.
    pub fn verify(&self) -> Result<Vec<StorePath>> {
        todo!("M2: verify")
    }

    /// Replace identical files with hard links; returns bytes saved.
    pub fn optimise(&self) -> Result<u64> {
        todo!("M6: optimise")
    }
}

impl Store for LocalStore {
    fn store_dir(&self) -> &StoreDir {
        &self.dir
    }

    fn query_path_info(&self, path: &StorePath) -> Result<Option<PathInfo>> {
        todo!("M2: query DB")
    }

    fn add_path(&self, name: &str, src: &Path) -> Result<StorePath> {
        todo!("M2: add to store")
    }

    fn add_text(
        &self,
        name: &str,
        text: &str,
        references: &BTreeSet<StorePath>,
    ) -> Result<StorePath> {
        todo!("M2: add text")
    }

    fn closure(&self, paths: &[StorePath]) -> Result<BTreeSet<StorePath>> {
        todo!("M2: closure")
    }
}
