//! Profiles (user environments with generations) and shell activation.

mod activation;
mod env;
mod generations;

pub use activation::{ShellEnv, render_activation, shell_command};
pub use env::build_env;
pub use generations::{Generation, Profile};
pub use neki_shim::{Shim, shim_sidecar};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{}: {source}", path.display())]
    File {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Store(#[from] neki_store::Error),
    #[error("profile {0} has no previous generation")]
    NoPreviousGeneration(String),
    #[error("profile {profile}: {message}")]
    Corrupt { profile: String, message: String },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

fn at(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::File {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests;
