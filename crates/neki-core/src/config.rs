use std::path::PathBuf;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    pub store_dir: PathBuf,
    pub state_dir: PathBuf,
    /// Binary caches to substitute from, in priority order.
    pub substituters: Vec<String>,
    /// Number of derivations built in parallel.
    pub max_jobs: usize,
    /// System that builds target, e.g. `x86_64-windows`.
    pub system: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            store_dir: neki_platform::paths::default_store_dir(),
            state_dir: neki_platform::paths::default_state_dir(),
            substituters: Vec::new(),
            max_jobs: neki_build::BuildOptions::default().max_jobs,
            system: neki_derivation::host_system(),
        }
    }
}

impl Config {
    /// Defaults, overridden by `NEKI_STORE_DIR` and `NEKI_STATE_DIR`.
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Some(dir) = std::env::var_os("NEKI_STORE_DIR") {
            config.store_dir = dir.into();
        }
        if let Some(dir) = std::env::var_os("NEKI_STATE_DIR") {
            config.state_dir = dir.into();
        }
        config
    }
}
