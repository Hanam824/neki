//! Per-project declarations (`neki.toml`) and lock files (`neki.lock`).

mod lock;
mod manifest;

pub use lock::{LOCK_VERSION, LockFile, LockedInput};
pub use manifest::{DevShell, Input, Manifest, PackageDecl, ProjectMeta};

use std::path::{Path, PathBuf};

pub const MANIFEST_FILE: &str = "neki.toml";
pub const LOCK_FILE: &str = "neki.lock";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot access {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: {message}", path.display())]
    Parse { path: PathBuf, message: String },
    #[error("no {MANIFEST_FILE} found in {} or any parent directory", .0.display())]
    NotFound(PathBuf),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A project directory with its manifest and lock file (if any).
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub manifest: Manifest,
    pub lock: Option<LockFile>,
}

impl Project {
    /// Load the nearest project at or above `start`.
    pub fn discover(start: &Path) -> Result<Self> {
        start
            .ancestors()
            .find(|dir| dir.join(MANIFEST_FILE).is_file())
            .map_or_else(|| Err(Error::NotFound(start.to_path_buf())), Self::load)
    }

    pub fn load(root: &Path) -> Result<Self> {
        let manifest = Manifest::from_file(&root.join(MANIFEST_FILE))?;
        let lock_path = root.join(LOCK_FILE);
        let lock = if lock_path.is_file() {
            Some(LockFile::from_file(&lock_path)?)
        } else {
            None
        };
        Ok(Self {
            root: root.to_path_buf(),
            manifest,
            lock,
        })
    }
}

/// Text of a new project's `neki.toml`.
pub fn starter_manifest(name: &str) -> String {
    let name = toml::Value::String(name.to_string());
    format!(
        "[project]\nname = {name}\n\n\
         # Sources pinned by neki.lock, e.g. pkgs = {{ url = \"github:owner/repo\" }}\n\
         [inputs]\n\n\
         [dev-shell]\npackages = []\n\n\
         [packages.default]\nsrc = \".\"\nbuild = []\n"
    )
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}
