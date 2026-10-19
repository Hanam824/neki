use std::path::{Path, PathBuf};

use neki_hash::StorePath;

use crate::Result;

/// A named, versioned pointer to an environment in the store, kept under
/// `<state dir>/profiles/<name>`. Every switch creates a new generation.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    pub number: u32,
    /// Unix timestamp.
    pub created: i64,
    pub path: StorePath,
}

impl Profile {
    pub fn open(state_dir: &Path, name: &str) -> Self {
        Self {
            name: name.to_string(),
            dir: state_dir.join("profiles").join(name),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn generations(&self) -> Result<Vec<Generation>> {
        todo!("M6: list generations")
    }

    pub fn current(&self) -> Result<Option<Generation>> {
        todo!("M6: current generation")
    }

    /// Create a new generation pointing at `env` and make it current.
    pub fn switch(&self, env: &StorePath) -> Result<Generation> {
        todo!("M6: switch")
    }

    /// Make the previous generation current.
    pub fn rollback(&self) -> Result<Generation> {
        todo!("M6: rollback")
    }
}
