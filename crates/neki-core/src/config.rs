use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Neki's settings: defaults, then `<user config dir>/neki/config.toml`,
/// then environment variables.
///
/// ```toml
/// substituters = ["https://cache.example.org"]
/// trusted-public-keys = ["cache.example.org-1:<base64>"]
/// secret-key-file = "/path/to/key"   # for `neki cache push`
/// max-jobs = 4
/// keep-failed = false
///
/// [registry]                          # inputs usable outside projects
/// pkgs = "github:Hanam824/neki?dir=pkgs"
/// ```
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    pub store_dir: PathBuf,
    pub state_dir: PathBuf,
    /// Binary caches to substitute from, in priority order.
    pub substituters: Vec<String>,
    /// Keys whose signatures make cached paths trusted (`name:base64`).
    pub trusted_public_keys: Vec<String>,
    /// Key used to sign uploads.
    pub secret_key_file: Option<PathBuf>,
    /// Number of derivations built in parallel.
    pub max_jobs: usize,
    /// Keep the build directory of failed builds.
    pub keep_failed: bool,
    /// System that builds target, e.g. `x86_64-windows`.
    pub system: String,
    /// Inputs available by name outside projects (`neki shell pkgs.x`,
    /// profiles): name → input URL.
    pub registry: BTreeMap<String, String>,
    /// The `neki-shim` launcher; found next to the running program if unset.
    pub shim: Option<PathBuf>,
}

/// The package set of this repository, the default `pkgs` registry entry.
pub const DEFAULT_PKGS: &str = "github:Hanam824/neki?dir=pkgs";

impl Default for Config {
    fn default() -> Self {
        Self {
            store_dir: neki_platform::paths::default_store_dir(),
            state_dir: neki_platform::paths::default_state_dir(),
            substituters: Vec::new(),
            trusted_public_keys: Vec::new(),
            secret_key_file: None,
            max_jobs: neki_build::BuildOptions::default().max_jobs,
            keep_failed: false,
            system: neki_derivation::host_system(),
            registry: BTreeMap::from([("pkgs".to_string(), DEFAULT_PKGS.to_string())]),
            shim: None,
        }
    }
}

/// The settings file's contents; every field is optional.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ConfigFile {
    store_dir: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    substituters: Option<Vec<String>>,
    trusted_public_keys: Option<Vec<String>>,
    secret_key_file: Option<PathBuf>,
    max_jobs: Option<usize>,
    keep_failed: Option<bool>,
    #[serde(default)]
    registry: BTreeMap<String, String>,
    shim: Option<PathBuf>,
}

impl Config {
    /// Defaults, the user's `config.toml` (if any), then `NEKI_STORE_DIR`,
    /// `NEKI_STATE_DIR`, `NEKI_SUBSTITUTERS` and `NEKI_TRUSTED_PUBLIC_KEYS`
    /// (space-separated lists).
    pub fn from_env() -> Self {
        let file = neki_platform::paths::user_config_dir().map(|d| d.join("config.toml"));
        let mut config = Self::default();
        if let Some(file) = file.filter(|f| f.is_file()) {
            // A broken settings file shouldn't make every command fail; report it.
            if let Err(err) = config.apply_file(&file) {
                eprintln!("warning: ignoring {}: {err}", file.display());
            }
        }
        config.apply_env(|name| std::env::var_os(name).map(|v| v.to_string_lossy().into_owned()));
        config
    }

    /// Apply the settings file at `path`.
    pub fn apply_file(&mut self, path: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let file: ConfigFile = toml::from_str(&text).map_err(|e| e.to_string())?;
        if let Some(v) = file.store_dir {
            self.store_dir = v;
        }
        if let Some(v) = file.state_dir {
            self.state_dir = v;
        }
        if let Some(v) = file.substituters {
            self.substituters = v;
        }
        if let Some(v) = file.trusted_public_keys {
            self.trusted_public_keys = v;
        }
        if let Some(v) = file.secret_key_file {
            self.secret_key_file = Some(v);
        }
        if let Some(v) = file.max_jobs {
            self.max_jobs = v.max(1);
        }
        if let Some(v) = file.keep_failed {
            self.keep_failed = v;
        }
        self.registry.extend(file.registry);
        if let Some(v) = file.shim {
            self.shim = Some(v);
        }
        Ok(())
    }

    fn apply_env(&mut self, var: impl Fn(&str) -> Option<String>) {
        if let Some(dir) = var("NEKI_STORE_DIR") {
            self.store_dir = dir.into();
        }
        if let Some(dir) = var("NEKI_STATE_DIR") {
            self.state_dir = dir.into();
        }
        if let Some(shim) = var("NEKI_SHIM") {
            self.shim = Some(shim.into());
        }
        let list = |s: String| s.split_whitespace().map(String::from).collect();
        if let Some(v) = var("NEKI_SUBSTITUTERS") {
            self.substituters = list(v);
        }
        if let Some(v) = var("NEKI_TRUSTED_PUBLIC_KEYS") {
            self.trusted_public_keys = list(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_and_environment_override_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(
            &file,
            "substituters = [\"file:///cache\"]\nmax-jobs = 3\n[registry]\nmine = \"path:/x\"\n",
        )
        .unwrap();
        let mut config = Config::default();
        config.apply_file(&file).unwrap();
        assert_eq!(config.substituters, ["file:///cache"]);
        assert_eq!(config.max_jobs, 3);
        assert_eq!(config.registry["pkgs"], DEFAULT_PKGS);
        assert_eq!(config.registry["mine"], "path:/x");
        config.apply_env(|name| match name {
            "NEKI_STORE_DIR" => Some("/tmp/store".into()),
            "NEKI_SUBSTITUTERS" => Some("a b".into()),
            _ => None,
        });
        assert_eq!(config.store_dir, Path::new("/tmp/store"));
        assert_eq!(config.substituters, ["a", "b"]);

        std::fs::write(&file, "bogus = 1\n").unwrap();
        assert!(Config::default().apply_file(&file).is_err());
    }
}
