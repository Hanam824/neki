//! Builds derivations: orders the dependency graph and runs builders.
//!
//! Builders get best-effort isolation, identical on every OS: a scrubbed
//! environment, a private build and temp directory, a `PATH` made only of
//! declared inputs, and the whole process tree is killed when the build ends.

#![allow(unused_variables)] // skeleton: stub bodies

use std::collections::BTreeMap;
use std::path::PathBuf;

use neki_hash::StorePath;
use neki_store::LocalStore;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] neki_store::Error),
    #[error(transparent)]
    Derivation(#[from] neki_derivation::Error),
    #[error("build of {drv} failed ({status}); log: {}", log.display())]
    BuildFailed {
        drv: String,
        status: String,
        log: PathBuf,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone)]
pub struct BuildOptions {
    /// Number of derivations built in parallel.
    pub max_jobs: usize,
    /// Keep the build directory of failed builds for inspection.
    pub keep_failed: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            max_jobs: std::thread::available_parallelism().map_or(1, |n| n.get()),
            keep_failed: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BuildResult {
    pub drv: StorePath,
    pub outputs: BTreeMap<String, StorePath>,
    /// False if the outputs were already valid and nothing ran.
    pub built: bool,
}

#[derive(Debug, Clone)]
pub enum BuildEvent {
    Started { drv: StorePath },
    Log { drv: StorePath, line: String },
    Finished { drv: StorePath, success: bool },
}

/// Build `drvs` and every dependency whose outputs aren't valid yet.
/// `events` is called from worker threads.
pub fn realise(
    store: &LocalStore,
    drvs: &[StorePath],
    opts: &BuildOptions,
    events: &(dyn Fn(BuildEvent) + Sync),
) -> Result<Vec<BuildResult>> {
    todo!("M3: scheduler and executor")
}
