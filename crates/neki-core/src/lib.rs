//! High-level operations shared by the CLI and the desktop app. Front-ends
//! depend only on this crate; long operations report through [`EventSink`].

#![allow(unused_variables)] // skeleton: stub bodies

mod config;
mod events;

pub use config::Config;
pub use events::{Event, EventSink, NullSink};
pub use neki_hash::{Hash, HashAlgo, Hasher, StorePath};
pub use neki_platform::paths::user_config_dir;
pub use neki_platform::shell::Shell;
pub use neki_profile::{Generation, ShellEnv};
pub use neki_project::{LOCK_FILE, LockFile, MANIFEST_FILE, Manifest, Project};
pub use neki_store::{GcOptions, GcResult, PathInfo};

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not implemented yet (planned for {0})")]
    NotImplemented(&'static str),
    #[error("{} already exists", .0.display())]
    AlreadyExists(PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Project(#[from] neki_project::Error),
    #[error("{}: {source}", path.display())]
    File {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Hash(#[from] neki_hash::Error),
    #[error(transparent)]
    Nar(#[from] neki_nar::Error),
    #[error(transparent)]
    Store(#[from] neki_store::Error),
    #[error(transparent)]
    Fetch(#[from] neki_fetch::Error),
    #[error(transparent)]
    Build(#[from] neki_build::Error),
    #[error(transparent)]
    Substitute(#[from] neki_substitute::Error),
    #[error(transparent)]
    Eval(#[from] neki_eval::Error),
    #[error(transparent)]
    Profile(#[from] neki_profile::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Error for operations whose implementation is scheduled for `milestone`.
fn planned<T>(milestone: &'static str) -> Result<T> {
    Err(Error::NotImplemented(milestone))
}

pub struct Neki {
    config: Config,
}

impl Neki {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The project at or above `dir`, or `None` if there is none.
    pub fn find_project(&self, dir: &Path) -> Result<Option<Project>> {
        match Project::discover(dir) {
            Ok(project) => Ok(Some(project)),
            Err(neki_project::Error::NotFound(_)) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// Create a project in `dir` (created if missing) with a starter `neki.toml`
    /// named after the directory.
    pub fn init_project(&self, dir: &Path) -> Result<Project> {
        let manifest_path = dir.join(MANIFEST_FILE);
        if manifest_path.exists() {
            return Err(Error::AlreadyExists(manifest_path));
        }
        std::fs::create_dir_all(dir)?;
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project");
        std::fs::write(&manifest_path, neki_project::starter_manifest(name))?;
        Ok(Project::load(dir)?)
    }

    /// Build a package of `project` (`default` if `package` is `None`) and
    /// return its output paths.
    pub fn build(
        &self,
        project: &Project,
        package: Option<&str>,
        events: &dyn EventSink,
    ) -> Result<Vec<StorePath>> {
        planned("M3")
    }

    /// Build a package and run its main program with `args`.
    pub fn run(
        &self,
        project: &Project,
        package: Option<&str>,
        args: &[String],
        events: &dyn EventSink,
    ) -> Result<ExitStatus> {
        planned("M5")
    }

    /// Environment of the project's dev shell, with all its packages built.
    pub fn develop(&self, project: &Project, events: &dyn EventSink) -> Result<ShellEnv> {
        planned("M5")
    }

    /// Environment with `packages` (`<input>.<attr>`) available.
    pub fn shell(&self, packages: &[String], events: &dyn EventSink) -> Result<ShellEnv> {
        planned("M5")
    }

    /// Lock missing inputs, re-resolve the inputs named in `update`, and
    /// write `neki.lock`.
    pub fn lock(
        &self,
        project: &Project,
        update: &[String],
        events: &dyn EventSink,
    ) -> Result<LockFile> {
        planned("M5")
    }

    /// Run the user's interactive shell with `env` applied and wait for it.
    pub fn spawn_shell(&self, env: &ShellEnv) -> Result<ExitStatus> {
        let (_, program) = Shell::detect();
        let mut cmd = Command::new(program);
        env.apply_to(&mut cmd);
        Ok(cmd.status()?)
    }

    pub fn store_add(&self, path: &Path) -> Result<StorePath> {
        planned("M2")
    }

    pub fn path_info(&self, path: &str) -> Result<PathInfo> {
        planned("M2")
    }

    /// Paths whose contents no longer match their recorded hash.
    pub fn verify(&self) -> Result<Vec<StorePath>> {
        planned("M2")
    }

    pub fn gc(&self, opts: &GcOptions) -> Result<GcResult> {
        planned("M6")
    }

    /// Deduplicate store files; returns bytes saved.
    pub fn optimise(&self) -> Result<u64> {
        planned("M6")
    }

    /// SHA-256 of the NAR serialization of `path` (a file, directory or symlink).
    pub fn hash_path(&self, path: &Path) -> Result<Hash> {
        let mut hasher = Hasher::new(HashAlgo::Sha256);
        neki_nar::dump(path, &mut hasher)?;
        Ok(hasher.finish())
    }

    /// SHA-256 of the flat contents of the file at `path`.
    pub fn hash_file(&self, path: &Path) -> Result<Hash> {
        let mut hasher = Hasher::new(HashAlgo::Sha256);
        let mut file = std::fs::File::open(path).map_err(|source| Error::File {
            path: path.to_path_buf(),
            source,
        })?;
        std::io::copy(&mut file, &mut hasher).map_err(|source| Error::File {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(hasher.finish())
    }

    pub fn profile_install(
        &self,
        profile: &str,
        packages: &[String],
        events: &dyn EventSink,
    ) -> Result<Generation> {
        planned("M6")
    }

    pub fn profile_remove(
        &self,
        profile: &str,
        packages: &[String],
        events: &dyn EventSink,
    ) -> Result<Generation> {
        planned("M6")
    }

    pub fn profile_rollback(&self, profile: &str) -> Result<Generation> {
        planned("M6")
    }

    pub fn profile_generations(&self, profile: &str) -> Result<Vec<Generation>> {
        planned("M6")
    }

    /// Upload the closures of `paths` to the cache at `url`.
    pub fn cache_push(&self, url: &str, paths: &[String], events: &dyn EventSink) -> Result<()> {
        planned("M7")
    }
}
