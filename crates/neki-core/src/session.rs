//! Turning package names (`pkgs.nodejs`, `self.default`) into derivations:
//! fetching inputs, evaluating package sets, and compiling `neki.toml`
//! packages. One session runs on one thread (evaluation isn't `Send`).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use neki_derivation::{Derivation, DerivationOutput};
use neki_eval::{ContextElem, EvalStore, Evaluator, FetchRequest, Thunk, Value};
use neki_fetch::FetchSpec;
use neki_hash::{StoreDir, StorePath};
use neki_project::{LOCK_FILE, LOCK_VERSION, LockFile, Project};
use neki_store::{EntryKind, LocalStore, Store};

use crate::inputs::Input;
use crate::{Error, EventSink, Result};

/// A derivation output: what a package name resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub drv: StorePath,
    pub output: String,
}

/// Where a package name's input names come from.
#[derive(Clone)]
pub enum Scope<'a> {
    Project(&'a Project),
    /// The registry in the settings (outside projects, and for profiles).
    Registry,
}

pub struct Session<'a> {
    pub store: Arc<LocalStore>,
    registry: &'a BTreeMap<String, String>,
    state_dir: PathBuf,
    ev: Evaluator,
    events: &'a dyn EventSink,
    /// Inputs to fetch fresh even if locked (`neki lock --update`).
    pub update: BTreeSet<String>,
    /// Fetched inputs: (lock file, input name) → directory.
    inputs: HashMap<(PathBuf, String), PathBuf>,
    /// Lock files read, and whether they changed.
    locks: HashMap<PathBuf, (LockFile, bool)>,
    /// Package sets evaluated, by directory.
    sets: HashMap<PathBuf, Value>,
    /// Projects used as inputs, by directory.
    projects: HashMap<PathBuf, Project>,
}

impl<'a> Session<'a> {
    pub fn new(
        store: Arc<LocalStore>,
        registry: &'a BTreeMap<String, String>,
        system: &str,
        events: &'a dyn EventSink,
    ) -> Self {
        let state_dir = store.state_dir().to_path_buf();
        let mut ev = Evaluator::with_store(Rc::new(EvalBackend {
            store: store.clone(),
        }));
        ev.system = system.to_string();
        Session {
            store,
            registry,
            state_dir,
            ev,
            events,
            update: BTreeSet::new(),
            inputs: HashMap::new(),
            locks: HashMap::new(),
            sets: HashMap::new(),
            projects: HashMap::new(),
        }
    }

    pub fn evaluator(&mut self) -> &mut Evaluator {
        &mut self.ev
    }

    fn store_dir(&self) -> &StoreDir {
        self.store.store_dir()
    }

    /// The inputs of `scope`, where its lock file is, and its directory
    /// (for `path:` inputs).
    fn scope_info(&self, scope: &Scope) -> (BTreeMap<String, String>, PathBuf, PathBuf) {
        match scope {
            Scope::Project(p) => (
                p.manifest
                    .inputs
                    .iter()
                    .map(|(k, v)| (k.clone(), v.url.clone()))
                    .collect(),
                p.root.join(LOCK_FILE),
                p.root.clone(),
            ),
            Scope::Registry => (
                self.registry.clone(),
                self.state_dir.join("registry.lock"),
                PathBuf::from("/"),
            ),
        }
    }

    fn lock_file(&mut self, file: &Path) -> Result<&mut (LockFile, bool)> {
        if !self.locks.contains_key(file) {
            let lock = if file.is_file() {
                LockFile::from_file(file)?
            } else {
                LockFile {
                    version: LOCK_VERSION,
                    inputs: BTreeMap::new(),
                }
            };
            self.locks.insert(file.to_path_buf(), (lock, false));
        }
        Ok(self.locks.get_mut(file).expect("inserted above"))
    }

    /// Fetch input `name` of `scope` (locked if possible) and return its
    /// directory.
    pub fn input(&mut self, scope: &Scope, name: &str) -> Result<PathBuf> {
        let (inputs, lock_path, base) = self.scope_info(scope);
        let key = (lock_path.clone(), name.to_string());
        if let Some(dir) = self.inputs.get(&key) {
            return Ok(dir.clone());
        }
        let url = inputs.get(name).ok_or_else(|| {
            Error::Input(match scope {
                Scope::Project(p) => format!(
                    "{} has no input named `{name}` (add it under [inputs])",
                    p.root.join(neki_project::MANIFEST_FILE).display()
                ),
                Scope::Registry => format!(
                    "no registry entry named `{name}` (add one under [registry] in config.toml)"
                ),
            })
        })?;
        let input = Input::parse(url)?;
        let refresh = matches!(scope, Scope::Project(_)) && self.update.contains(name);
        let locked = if refresh {
            None
        } else {
            self.lock_file(&lock_path)?.0.inputs.get(name).cloned()
        };
        let (_, dir, entry) = input.fetch(url, &self.store, &base, locked.as_ref(), self.events)?;
        let lock = self.lock_file(&lock_path)?;
        if lock.0.inputs.get(name) != Some(&entry) {
            lock.0.inputs.insert(name.to_string(), entry);
            lock.1 = true;
        }
        self.inputs.insert(key, dir.clone());
        Ok(dir)
    }

    /// Fetch every input of `scope`, e.g. for `neki lock`.
    pub fn lock_all(&mut self, scope: &Scope) -> Result<()> {
        let (inputs, lock_path, _) = self.scope_info(scope);
        for name in inputs.keys() {
            self.input(scope, name)?;
        }
        // Drop entries for inputs that are no longer declared.
        let lock = self.lock_file(&lock_path)?;
        let before = lock.0.inputs.len();
        lock.0.inputs.retain(|name, _| inputs.contains_key(name));
        if lock.0.inputs.len() != before {
            lock.1 = true;
        }
        Ok(())
    }

    /// Write every lock file that changed.
    pub fn save_locks(&mut self) -> Result<()> {
        for (path, (lock, dirty)) in &mut self.locks {
            if *dirty {
                std::fs::write(path.as_path(), lock.to_json()).map_err(|source| Error::File {
                    path: path.clone(),
                    source,
                })?;
                *dirty = false;
            }
        }
        Ok(())
    }

    /// The lock file of `scope` as it is now.
    pub fn lock_of(&mut self, scope: &Scope) -> Result<LockFile> {
        let (_, lock_path, _) = self.scope_info(scope);
        Ok(self.lock_file(&lock_path)?.0.clone())
    }

    /// Resolve `spec`: `<input>.<attr path>`, or `self.<package>` for a
    /// package of the project itself.
    pub fn resolve(&mut self, scope: &Scope, spec: &str) -> Result<Resolved> {
        let (input, attr) = spec.split_once('.').ok_or_else(|| {
            Error::Input(format!(
                "`{spec}`: packages are named <input>.<package>, e.g. pkgs.nodejs"
            ))
        })?;
        if input == "self" {
            let Scope::Project(project) = scope else {
                return Err(Error::Input(format!(
                    "`{spec}`: `self` only works inside a project"
                )));
            };
            let project = (*project).clone();
            return self.compile(&project, attr);
        }
        let dir = self.input(scope, input)?;
        self.resolve_in(&dir, spec, attr)
    }

    /// `attr` (a dotted path) of the package source at `dir`: a package set
    /// (`default.neki`) or a project (`neki.toml`).
    fn resolve_in(&mut self, dir: &Path, spec: &str, attr: &str) -> Result<Resolved> {
        if dir.join("default.neki").is_file() {
            let set = self.package_set(dir)?;
            let path: Vec<&str> = attr.split('.').collect();
            let value = self
                .ev
                .select(&set, &path)
                .map_err(|e| Error::Input(format!("`{spec}`: {e}")))?;
            let (drv, output) = self
                .ev
                .derivation_info(&value)?
                .ok_or_else(|| Error::Input(format!("`{spec}` is not a package")))?;
            return Ok(Resolved { drv, output });
        }
        if dir.join(neki_project::MANIFEST_FILE).is_file() {
            if !self.projects.contains_key(dir) {
                self.projects.insert(dir.to_path_buf(), Project::load(dir)?);
            }
            let project = self.projects[dir].clone();
            return self.compile(&project, attr);
        }
        Err(Error::Input(format!(
            "`{spec}`: {} has neither default.neki nor neki.toml",
            dir.display()
        )))
    }

    /// The package set at `dir` (its `default.neki`), evaluated once.
    fn package_set(&mut self, dir: &Path) -> Result<Value> {
        if !self.sets.contains_key(dir) {
            let mut set = self.ev.eval_file(&dir.join("default.neki"))?;
            if matches!(set, Value::Lambda(_) | Value::PrimOp(_)) {
                // A package set may take arguments; pass none.
                set = self.ev.call(&set, Value::attrs(Default::default()))?;
            }
            self.sets.insert(dir.to_path_buf(), set);
        }
        Ok(self.sets[dir].clone())
    }

    /// A package defined by a `.neki` function (`file` in `neki.toml`).
    fn compile_file(
        &mut self,
        project: &Project,
        file: &str,
        name: &str,
        src: &StorePath,
    ) -> Result<Resolved> {
        let scope = Scope::Project(project);
        let mut inputs = BTreeMap::new();
        for input in project.manifest.inputs.keys() {
            let dir = self.input(&scope, input)?;
            let value = if dir.join("default.neki").is_file() {
                self.package_set(&dir)?
            } else {
                Value::Path(dir)
            };
            inputs.insert(input.clone(), Thunk::value(value));
        }
        let src_value = Value::String {
            s: self.store_dir().to_path(src).display().to_string(),
            context: [ContextElem::Path(src.clone())].into(),
        };
        let arg = Value::attrs(BTreeMap::from([
            ("inputs".to_string(), Thunk::value(Value::attrs(inputs))),
            ("src".to_string(), Thunk::value(src_value)),
            ("name".to_string(), Thunk::value(Value::string(name))),
            (
                "system".to_string(),
                Thunk::value(Value::string(self.ev.system.clone())),
            ),
        ]));
        let path = project.root.join(file);
        let f = self.ev.eval_file(&path)?;
        let value = self.ev.call(&f, arg)?;
        let (drv, output) = self
            .ev
            .derivation_info(&value)?
            .ok_or_else(|| Error::Input(format!("{} does not return a package", path.display())))?;
        Ok(Resolved { drv, output })
    }

    /// Compile package `name` of `project`'s `neki.toml` into a derivation.
    pub fn compile(&mut self, project: &Project, name: &str) -> Result<Resolved> {
        let decl = project.manifest.packages.get(name).ok_or_else(|| {
            Error::Input(format!(
                "{} has no package `{name}`",
                project.root.join(neki_project::MANIFEST_FILE).display()
            ))
        })?;
        let pkg_name = store_name(&if name == "default" {
            project.manifest.project.name.clone()
        } else {
            format!("{}-{name}", project.manifest.project.name)
        });

        let scope = Scope::Project(project);
        let mut input_drvs: BTreeMap<StorePath, BTreeSet<String>> = BTreeMap::new();
        let mut inputs = Vec::new();
        for spec in &decl.build_inputs {
            let resolved = self.resolve(&scope, spec)?;
            let drv = self.store.read_derivation(&resolved.drv)?;
            let out = drv.output_paths(self.store_dir())?;
            let path = out.get(&resolved.output).ok_or_else(|| {
                Error::Input(format!("`{spec}` has no output `{}`", resolved.output))
            })?;
            inputs.push(self.store_dir().to_path(path).display().to_string());
            input_drvs
                .entry(resolved.drv)
                .or_default()
                .insert(resolved.output);
        }

        // A `file` package can use any input; lock them all now.
        if decl.file.is_some() {
            for input in project.manifest.inputs.keys() {
                self.input(&scope, input)?;
            }
        }

        // The source, without version control data, build results or
        // excluded entries. Copied after the inputs are locked, so a
        // `neki.lock` written now is part of it from the first build on.
        self.save_locks()?;
        let src_root = project.root.join(&decl.src);
        let exclude: Vec<PathBuf> = decl.exclude.iter().map(|e| src_root.join(e)).collect();
        let src =
            self.store
                .add_path_filtered("source", &src_root, &mut |path, _: EntryKind| {
                    let top = path.parent() == Some(src_root.as_path());
                    let file_name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    !(file_name == ".git"
                        || (top && (file_name == "result" || file_name.starts_with("result-")))
                        || exclude.iter().any(|e| e == path))
                })?;

        if let Some(file) = &decl.file {
            return self.compile_file(project, file, &pkg_name, &src);
        }

        let mut env = decl.env.clone();
        env.insert("name".into(), pkg_name.clone());
        env.insert("system".into(), self.ev.system.clone());
        env.insert("builder".into(), "builtin:run".into());
        env.insert(
            "src".into(),
            self.store_dir().to_path(&src).display().to_string(),
        );
        env.insert("buildInputs".into(), inputs.join(" "));
        env.insert(
            "commands".into(),
            serde_json::to_string(&decl.build).expect("argument lists serialize"),
        );
        let mut drv = Derivation {
            outputs: BTreeMap::from([("out".to_string(), DerivationOutput::Deferred)]),
            input_drvs,
            input_srcs: BTreeSet::from([src]),
            system: self.ev.system.clone(),
            builder: "builtin:run".into(),
            args: Vec::new(),
            env,
        };
        let store = self.store.clone();
        drv.finalize(self.store_dir(), &|p| {
            store
                .read_derivation(p)
                .map_err(|e| neki_derivation::Error::Invalid(e.to_string()))
        })?;
        let drv = self.store.add_derivation(&drv)?;
        Ok(Resolved {
            drv,
            output: "out".into(),
        })
    }

    /// Evaluate a `.neki` file and resolve `attr` in it (or the file's value
    /// itself if `attr` is empty) to derivations: one for a package, one per
    /// package for a set of packages.
    pub fn eval_packages(&mut self, file: &Path, attr: Option<&str>) -> Result<Vec<Resolved>> {
        let mut value = self.ev.eval_file(file)?;
        if matches!(value, Value::Lambda(_) | Value::PrimOp(_)) {
            value = self.ev.call(&value, Value::attrs(Default::default()))?;
        }
        if let Some(attr) = attr.filter(|a| !a.is_empty()) {
            let path: Vec<&str> = attr.split('.').collect();
            value = self.ev.select(&value, &path)?;
        }
        if let Some((drv, output)) = self.ev.derivation_info(&value)? {
            return Ok(vec![Resolved { drv, output }]);
        }
        let Value::Attrs(attrs) = &value else {
            return Err(Error::Input(format!(
                "{} does not evaluate to a package",
                file.display()
            )));
        };
        let mut out = Vec::new();
        for thunk in attrs.values() {
            let v = self.ev.force(thunk)?;
            if let Some((drv, output)) = self.ev.derivation_info(&v)? {
                out.push(Resolved { drv, output });
            }
        }
        Ok(out)
    }
}

/// `name` with every character a store path name can't have replaced.
pub fn store_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "+-._=".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').to_string();
    if cleaned.is_empty() {
        "package".into()
    } else {
        cleaned
    }
}

/// The store, as evaluation sees it.
struct EvalBackend {
    store: Arc<LocalStore>,
}

impl EvalStore for EvalBackend {
    fn store_dir(&self) -> &StoreDir {
        self.store.store_dir()
    }

    fn add_path(
        &self,
        name: &str,
        path: &Path,
        filter: Option<neki_eval::PathFilter>,
    ) -> Result<StorePath, String> {
        let name = store_name(name);
        match filter {
            None => self.store.add_path(&name, path).map_err(|e| e.to_string()),
            Some(filter) => {
                let failed = RefCell::new(None);
                let added = self
                    .store
                    .add_path_filtered(&name, path, &mut |p, kind| match filter(p, kind.name()) {
                        Ok(keep) => keep,
                        Err(e) => {
                            failed.borrow_mut().get_or_insert(e.to_string());
                            false
                        }
                    });
                if let Some(e) = failed.into_inner() {
                    return Err(e);
                }
                added.map_err(|e| e.to_string())
            }
        }
    }

    fn add_text(
        &self,
        name: &str,
        text: &str,
        refs: &BTreeSet<StorePath>,
    ) -> Result<StorePath, String> {
        self.store
            .add_text(name, text, refs)
            .map_err(|e| e.to_string())
    }

    fn add_derivation(&self, drv: &Derivation) -> Result<StorePath, String> {
        self.store.add_derivation(drv).map_err(|e| e.to_string())
    }

    fn read_derivation(&self, path: &StorePath) -> Result<Derivation, String> {
        self.store.read_derivation(path).map_err(|e| e.to_string())
    }

    fn fetch(&self, name: &str, request: &FetchRequest) -> Result<StorePath, String> {
        let spec = match request {
            FetchRequest::Url {
                url,
                hash: Some(hash),
            } => FetchSpec::Url {
                url: url.clone(),
                hash: hash.clone(),
            },
            FetchRequest::Url { url, hash: None } => {
                return Err(format!(
                    "fetchurl {url}: give `hash` (or `sha256`) so the result is reproducible"
                ));
            }
            FetchRequest::Tarball { url, hash } => FetchSpec::Archive {
                url: url.clone(),
                hash: hash.clone(),
                strip_root: true,
            },
            FetchRequest::Git {
                url,
                rev,
                reference,
            } => {
                let rev = match rev {
                    Some(rev) => rev.clone(),
                    None => neki_fetch::git_resolve(url, reference.as_deref())
                        .map_err(|e| e.to_string())?,
                };
                FetchSpec::Git {
                    url: url.clone(),
                    rev,
                    hash: None,
                }
            }
        };
        neki_fetch::fetch(&self.store, &store_name(name), &spec, &|_, _| {})
            .map_err(|e| e.to_string())
    }
}
