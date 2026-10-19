//! Profiles (user environments with generations) and shell activation.

#![allow(unused_variables)] // skeleton: stub bodies

mod activation;
mod generations;

pub use activation::{ShellEnv, render_activation};
pub use generations::{Generation, Profile};

use neki_hash::StorePath;
use neki_store::LocalStore;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] neki_store::Error),
    #[error("profile {0} has no previous generation")]
    NoPreviousGeneration(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Merge `packages` into one store path with a single `bin` directory of
/// hard links and shims, so `PATH` needs only one entry per environment.
pub fn build_env(store: &LocalStore, name: &str, packages: &[StorePath]) -> Result<StorePath> {
    todo!("M5: merged environment")
}
