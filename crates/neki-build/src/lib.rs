//! Builds derivations: orders the dependency graph and runs builders.
//!
//! Builders get best-effort isolation, identical on every OS: a scrubbed
//! environment, a private build and temp directory, a `PATH` made only of
//! declared inputs, and the whole process tree is killed when the build ends.
//!
//! Besides external programs, a derivation's builder can be one of:
//!
//! - `builtin:run`: run the argv lists in the JSON `commands` variable in
//!   order, without a shell, in a writable copy of `src` (if set). `${NAME}`
//!   in an argument expands to the build variable `NAME`, `$$` to `$`.
//! - `builtin:fetchurl`: download `url` to `$out`, or with `unpack = "1"`
//!   unpack it there (`stripRoot = "1"` removes a single top-level
//!   directory). Only for fixed-output derivations.
//! - `builtin:unpack`: unpack the archive `src` into `$out`. `stripRoot` is
//!   `1` (remove the single top-level directory), `0` (default) or `auto`
//!   (remove it if there is exactly one). `subdirs` (space-separated) makes
//!   `$out` the union of those directories of the archive. `install =
//!   "<path>"` instead copies the file `src` to `$out/<path>` as a program.
//!   The directories holding programs (`binDirs`, space-separated, or found:
//!   `bin`, else the top level, else a `bin` one level down) are written to
//!   `neki-support/bin-dirs`.
//! - `builtin:assemble`: build `$out` from the JSON list `entries` of
//!   `{"path": …, "src": <store path>}` (copied, merging directories) and
//!   `{"path": …, "text": …, "executable": bool}` (written).
//!
//! Every builder gets `PATH` made of the program directories (see
//! `neki_store::bin_dirs`) of the whitespace-separated store paths in
//! `buildInputs`.

mod builder;
mod builtins;
mod schedule;

pub use builder::build_into;
pub use schedule::realise;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use neki_hash::StorePath;
use neki_store::LocalStore;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{}: {source}", path.display())]
    File {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Store(#[from] neki_store::Error),
    #[error(transparent)]
    Derivation(#[from] neki_derivation::Error),
    #[error(transparent)]
    Fetch(#[from] neki_fetch::Error),
    #[error("build of {drv} failed: {reason}; log: {}", log.display())]
    BuildFailed {
        drv: String,
        reason: String,
        log: PathBuf,
    },
    #[error(transparent)]
    Nar(#[from] neki_nar::Error),
    #[error("{0}")]
    Builder(String),
    #[error("build cancelled")]
    Cancelled,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Downloads prebuilt store paths (a binary cache client).
pub trait Substituter: Send + Sync {
    /// Download `path` and everything it refers to into `store`. Returns
    /// false if the substituter doesn't have it.
    fn substitute(&self, store: &LocalStore, path: &StorePath) -> Result<bool, String>;
}

#[derive(Clone)]
pub struct BuildOptions {
    /// Number of derivations built in parallel.
    pub max_jobs: usize,
    /// Keep the build directory of failed builds for inspection.
    pub keep_failed: bool,
    /// Tried in order for every output before building it.
    pub substituters: Vec<Arc<dyn Substituter>>,
    /// Set to stop: running builders are killed and `realise` returns
    /// [`Error::Cancelled`].
    pub cancel: Arc<AtomicBool>,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            max_jobs: std::thread::available_parallelism().map_or(1, |n| n.get()),
            keep_failed: false,
            substituters: Vec::new(),
            cancel: Arc::default(),
        }
    }
}

impl std::fmt::Debug for BuildOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuildOptions")
            .field("max_jobs", &self.max_jobs)
            .field("keep_failed", &self.keep_failed)
            .field("substituters", &self.substituters.len())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct BuildResult {
    pub drv: StorePath,
    pub outputs: BTreeMap<String, StorePath>,
    /// False if the outputs were already valid or substituted.
    pub built: bool,
}

#[derive(Debug, Clone)]
pub enum BuildEvent {
    Started { drv: StorePath },
    Log { drv: StorePath, line: String },
    Finished { drv: StorePath, success: bool },
    Substituted { path: StorePath },
}

/// Called from worker threads.
pub type Events<'a> = &'a (dyn Fn(BuildEvent) + Sync);

#[cfg(test)]
mod tests;
