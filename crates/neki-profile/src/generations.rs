use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use neki_hash::StorePath;
use neki_store::{LocalStore, Store};
use serde::{Deserialize, Serialize};

use crate::{Error, Result, at};

/// A named, versioned pointer to an environment in the store, kept under
/// `<state dir>/profiles/<name>`:
///
/// - `generations/<n>.json`: every generation;
/// - `current-generation`: the number of the current one;
/// - `current`: a directory link to the current environment, so
///   `<profile>/current/bin` can go on `PATH`.
///
/// Every generation is a GC root named `profile:<name>:<n>`.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generation {
    pub number: u32,
    /// Unix timestamp.
    pub created: i64,
    #[serde(with = "store_path")]
    pub path: StorePath,
    /// The installed packages, as the user named them, in order.
    #[serde(default)]
    pub packages: Vec<String>,
}

mod store_path {
    use neki_hash::StorePath;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(path: &StorePath, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&path.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<StorePath, D::Error> {
        let s = String::deserialize(d)?;
        StorePath::parse(&s).map_err(serde::de::Error::custom)
    }
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

    /// The link to the current environment (`<profile>/current`).
    pub fn current_link(&self) -> PathBuf {
        self.dir.join("current")
    }

    fn generations_dir(&self) -> PathBuf {
        self.dir.join("generations")
    }

    fn corrupt(&self, message: impl Into<String>) -> Error {
        Error::Corrupt {
            profile: self.name.clone(),
            message: message.into(),
        }
    }

    /// All generations, oldest first.
    pub fn generations(&self) -> Result<Vec<Generation>> {
        let dir = self.generations_dir();
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir).map_err(at(&dir))? {
            let path = entry.map_err(at(&dir))?.path();
            if path.extension().is_some_and(|e| e == "json") {
                let text = fs::read_to_string(&path).map_err(at(&path))?;
                out.push(
                    serde_json::from_str::<Generation>(&text)
                        .map_err(|e| self.corrupt(format!("{}: {e}", path.display())))?,
                );
            }
        }
        out.sort_by_key(|g| g.number);
        Ok(out)
    }

    pub fn current(&self) -> Result<Option<Generation>> {
        let file = self.dir.join("current-generation");
        let Ok(text) = fs::read_to_string(&file) else {
            return Ok(None);
        };
        let number: u32 = text
            .trim()
            .parse()
            .map_err(|_| self.corrupt("current-generation is not a number"))?;
        Ok(self.generations()?.into_iter().find(|g| g.number == number))
    }

    /// Create a new generation pointing at `env` (with `packages`) and make
    /// it current.
    pub fn switch(
        &self,
        store: &LocalStore,
        env: &StorePath,
        packages: &[String],
    ) -> Result<Generation> {
        let number = self.generations()?.last().map_or(1, |g| g.number + 1);
        let generation = Generation {
            number,
            created: now(),
            path: env.clone(),
            packages: packages.to_vec(),
        };
        let dir = self.generations_dir();
        fs::create_dir_all(&dir).map_err(at(&dir))?;
        let file = dir.join(format!("{number}.json"));
        let json = serde_json::to_string_pretty(&generation).expect("generations serialize");
        fs::write(&file, json).map_err(at(&file))?;
        store.add_root(&self.root_name(number), env)?;
        self.make_current(store, &generation)?;
        Ok(generation)
    }

    /// Make the generation before the current one current.
    pub fn rollback(&self, store: &LocalStore) -> Result<Generation> {
        let current = self.current()?.map(|g| g.number).unwrap_or(u32::MAX);
        let previous = self
            .generations()?
            .into_iter()
            .rev()
            .find(|g| g.number < current)
            .ok_or_else(|| Error::NoPreviousGeneration(self.name.clone()))?;
        self.make_current(store, &previous)?;
        Ok(previous)
    }

    /// Make `generation` (an existing one) current.
    pub fn switch_to(&self, store: &LocalStore, number: u32) -> Result<Generation> {
        let generation = self
            .generations()?
            .into_iter()
            .find(|g| g.number == number)
            .ok_or_else(|| self.corrupt(format!("there is no generation {number}")))?;
        self.make_current(store, &generation)?;
        Ok(generation)
    }

    fn make_current(&self, store: &LocalStore, generation: &Generation) -> Result<()> {
        let link = self.current_link();
        neki_platform::fs::remove_tree(&link).map_err(at(&link))?;
        let target = store.store_dir().to_path(&generation.path);
        neki_platform::fs::link_dir(&target, &link).map_err(at(&link))?;
        let file = self.dir.join("current-generation");
        fs::write(&file, generation.number.to_string()).map_err(at(&file))
    }

    /// Delete generations created at least `age` ago (never the current
    /// one; `age` zero deletes all others) and their GC roots. Returns the
    /// deleted generation numbers.
    pub fn delete_older_than(&self, store: &LocalStore, age: Duration) -> Result<Vec<u32>> {
        let current = self.current()?.map(|g| g.number);
        let cutoff = now() - i64::try_from(age.as_secs()).unwrap_or(i64::MAX);
        let mut deleted = Vec::new();
        for generation in self.generations()? {
            if Some(generation.number) != current && generation.created <= cutoff {
                let file = self
                    .generations_dir()
                    .join(format!("{}.json", generation.number));
                fs::remove_file(&file).map_err(at(&file))?;
                store.remove_root(&self.root_name(generation.number))?;
                deleted.push(generation.number);
            }
        }
        Ok(deleted)
    }

    fn root_name(&self, number: u32) -> String {
        format!("profile:{}:{number}", self.name)
    }

    /// Every profile under `state_dir`.
    pub fn list(state_dir: &Path) -> Result<Vec<Profile>> {
        let dir = state_dir.join("profiles");
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir).map_err(at(&dir))? {
            let entry = entry.map_err(at(&dir))?;
            if entry.path().is_dir() {
                out.push(Profile::open(
                    state_dir,
                    &entry.file_name().to_string_lossy(),
                ));
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
