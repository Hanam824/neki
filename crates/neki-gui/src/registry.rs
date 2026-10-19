//! The project folders shown in the app, persisted in
//! `<user config dir>/desktop.toml`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    projects: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct Registry {
    /// Where the list is saved; `None` keeps it in memory only.
    file: Option<PathBuf>,
    projects: Vec<PathBuf>,
}

impl Registry {
    pub fn default_file() -> Option<PathBuf> {
        neki_core::user_config_dir().map(|dir| dir.join("desktop.toml"))
    }

    /// Registry that is never saved.
    pub fn in_memory() -> Self {
        Self {
            file: None,
            projects: Vec::new(),
        }
    }

    /// Load from `file`; a missing file gives an empty registry.
    pub fn load(file: PathBuf) -> Result<Self, String> {
        let projects = if file.exists() {
            let text = std::fs::read_to_string(&file)
                .map_err(|err| format!("cannot read {}: {err}", file.display()))?;
            toml::from_str::<RegistryFile>(&text)
                .map_err(|err| format!("{}: {err}", file.display()))?
                .projects
        } else {
            Vec::new()
        };
        Ok(Self {
            file: Some(file),
            projects,
        })
    }

    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    pub fn projects(&self) -> &[PathBuf] {
        &self.projects
    }

    /// Index of `root`, adding it if it isn't registered yet.
    pub fn add(&mut self, root: PathBuf) -> usize {
        if let Some(index) = self.projects.iter().position(|p| *p == root) {
            return index;
        }
        self.projects.push(root);
        self.projects.len() - 1
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.projects.len() {
            self.projects.remove(index);
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        let text = toml::to_string(&RegistryFile {
            projects: self.projects.clone(),
        })
        .map_err(|err| err.to_string())?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
        }
        std::fs::write(file, text).map_err(|err| format!("cannot write {}: {err}", file.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_dedupes_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("neki-gui-registry-{}", std::process::id()));
        let file = dir.join("desktop.toml");
        let mut registry = Registry::load(file.clone()).unwrap();
        assert_eq!(registry.add("/a".into()), 0);
        assert_eq!(registry.add("/b".into()), 1);
        assert_eq!(registry.add("/a".into()), 0);
        registry.save().unwrap();

        let mut loaded = Registry::load(file).unwrap();
        assert_eq!(
            loaded.projects(),
            [PathBuf::from("/a"), PathBuf::from("/b")]
        );
        loaded.remove(0);
        assert_eq!(loaded.projects(), [PathBuf::from("/b")]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
