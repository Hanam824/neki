//! High-level operations shared by the CLI and the desktop app. Front-ends
//! depend only on this crate; long operations report through [`EventSink`].

mod config;
mod events;
mod inputs;
mod session;
mod source;

pub use config::{Config, DEFAULT_PKGS};
pub use events::{Event, EventSink, NullSink};
pub use inputs::{Input, InputUrl};
pub use neki_hash::{Hash, HashAlgo, Hasher, StoreDir, StorePath};
pub use neki_platform::paths::user_config_dir;
pub use neki_platform::shell::Shell;
pub use neki_profile::{Generation, Profile, ShellEnv, render_activation};
pub use neki_project::{LOCK_FILE, LockFile, MANIFEST_FILE, Manifest, Project};
pub use neki_store::{GcOptions, GcResult, LocalStore, PathInfo, Root, Store};
pub use neki_substitute::{PublicKey, SecretKey};
pub use session::Resolved;

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use neki_substitute::BinaryCache;
use session::{Scope, Session};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{} already exists", .0.display())]
    AlreadyExists(PathBuf),
    #[error("{}: can't name a store path after it; it has no file name", .0.display())]
    NoName(PathBuf),
    #[error("{0} is not in the store")]
    NotInStore(String),
    #[error("{0}")]
    Input(String),
    #[error("{0}")]
    Run(String),
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
    Derivation(#[from] neki_derivation::Error),
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

/// Stack size for evaluation, which recurses deeply on big package sets.
const EVAL_STACK: usize = 256 * 1024 * 1024;

pub struct Neki {
    config: Config,
    cancel: Arc<AtomicBool>,
}

impl Neki {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            cancel: Arc::default(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Stop the build in progress (from another thread): its builders are
    /// killed and it fails with "build cancelled".
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Open the local store, creating it if needed.
    pub fn store(&self) -> Result<Arc<LocalStore>> {
        Ok(Arc::new(LocalStore::open(
            StoreDir::new(&self.config.store_dir),
            self.config.state_dir.clone(),
        )?))
    }

    /// Run `f` with a [`Session`] on a thread with a big stack, then save
    /// the lock files it changed. The garbage collector waits meanwhile:
    /// evaluation fetches and copies into the store.
    fn session<T: Send>(
        &self,
        store: &Arc<LocalStore>,
        events: &dyn EventSink,
        update: &[String],
        f: impl FnOnce(&mut Session) -> Result<T> + Send,
    ) -> Result<T> {
        let _gc = store.gc_guard()?;
        std::thread::scope(|scope| {
            let handle = std::thread::Builder::new()
                .name("neki-eval".into())
                .stack_size(EVAL_STACK)
                .spawn_scoped(scope, || {
                    let mut session = Session::new(
                        store.clone(),
                        &self.config.registry,
                        &self.config.system,
                        events,
                    );
                    session.update = update.iter().cloned().collect();
                    let result = f(&mut session)?;
                    session.save_locks()?;
                    Ok(result)
                })?;
            handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        })
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

    // ----- building -----

    /// The configured binary caches, trusting the configured keys.
    fn caches(&self) -> Result<Vec<BinaryCache>> {
        let keys = self
            .config
            .trusted_public_keys
            .iter()
            .map(|k| PublicKey::parse(k))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self
            .config
            .substituters
            .iter()
            .map(|url| BinaryCache::new(url.clone(), keys.clone()))
            .collect())
    }

    fn build_options(&self) -> Result<neki_build::BuildOptions> {
        self.cancel.store(false, Ordering::Relaxed);
        let substituters = self
            .caches()?
            .into_iter()
            .map(|cache| Arc::new(CacheSubstituter(cache)) as Arc<dyn neki_build::Substituter>)
            .collect();
        Ok(neki_build::BuildOptions {
            max_jobs: self.config.max_jobs,
            keep_failed: self.config.keep_failed,
            substituters,
            cancel: self.cancel.clone(),
        })
    }

    /// Build `targets` and return the path of each one's output.
    pub fn realise(
        &self,
        store: &LocalStore,
        targets: &[Resolved],
        events: &dyn EventSink,
    ) -> Result<Vec<StorePath>> {
        let drvs: Vec<StorePath> = targets.iter().map(|t| t.drv.clone()).collect();
        let results = neki_build::realise(store, &drvs, &self.build_options()?, &|event| {
            forward(events, event)
        })?;
        targets
            .iter()
            .zip(results)
            .map(|(target, result)| {
                result.outputs.get(&target.output).cloned().ok_or_else(|| {
                    Error::Input(format!("{} has no output `{}`", target.drv, target.output))
                })
            })
            .collect()
    }

    /// Build a package of `project` (`default` if `package` is `None`),
    /// point the project's `result` link at it, and return its output.
    pub fn build(
        &self,
        project: &Project,
        package: Option<&str>,
        events: &dyn EventSink,
    ) -> Result<Vec<StorePath>> {
        let store = self.store()?;
        let _gc = store.gc_guard()?;
        let package = package.unwrap_or("default");
        let target = self.session(&store, events, &[], |s| s.compile(project, package))?;
        let out = self.realise(&store, &[target], events)?;
        let link = project.root.join(if package == "default" {
            "result".to_string()
        } else {
            format!("result-{package}")
        });
        self.link_result(&store, &out[0], &link, events)?;
        Ok(out)
    }

    /// Point `link` at `path` (if it's a directory) and keep it from the
    /// garbage collector while the link exists.
    fn link_result(
        &self,
        store: &LocalStore,
        path: &StorePath,
        link: &Path,
        events: &dyn EventSink,
    ) -> Result<()> {
        let target = store.store_dir().to_path(path);
        if !target.is_dir() {
            return Ok(());
        }
        match std::fs::symlink_metadata(link) {
            Ok(meta) if meta.file_type().is_symlink() => {
                neki_platform::fs::remove_tree(link).map_err(|source| Error::File {
                    path: link.to_path_buf(),
                    source,
                })?;
            }
            Ok(_) => {
                events.event(Event::Message(format!(
                    "{} exists and is not a link; not replacing it",
                    link.display()
                )));
                return Ok(());
            }
            Err(_) => {}
        }
        neki_platform::fs::link_dir(&target, link).map_err(|source| Error::File {
            path: link.to_path_buf(),
            source,
        })?;
        let link = std::path::absolute(link)?;
        store.add_indirect_root(&format!("result:{}", link.display()), path, &link)?;
        Ok(())
    }

    /// Build the packages that the `.neki` file at `file` evaluates to (its
    /// attribute `attr`, if given), as `neki build --file` does.
    pub fn build_file(
        &self,
        file: &Path,
        attr: Option<&str>,
        events: &dyn EventSink,
    ) -> Result<Vec<StorePath>> {
        let store = self.store()?;
        let _gc = store.gc_guard()?;
        let targets = self.session(&store, events, &[], |s| s.eval_packages(file, attr))?;
        self.realise(&store, &targets, events)
    }

    /// The derivations (`.drv` paths) the `.neki` file at `file` evaluates to.
    pub fn instantiate(&self, file: &Path, attr: Option<&str>) -> Result<Vec<StorePath>> {
        let store = self.store()?;
        let targets = self.session(&store, &NullSink, &[], |s| s.eval_packages(file, attr))?;
        Ok(targets.into_iter().map(|t| t.drv).collect())
    }

    /// Build store derivations (`.drv` paths) directly.
    pub fn build_drvs(&self, drvs: &[String], events: &dyn EventSink) -> Result<Vec<StorePath>> {
        let store = self.store()?;
        let mut targets = Vec::new();
        for drv in drvs {
            let path = parse_store_path(store.store_dir(), drv)?;
            for output in store.read_derivation(&path)?.outputs.keys() {
                targets.push(Resolved {
                    drv: path.clone(),
                    output: output.clone(),
                });
            }
        }
        self.realise(&store, &targets, events)
    }

    /// Evaluate `expr` (a `.neki` expression) and print the result fully
    /// evaluated, or as JSON.
    pub fn eval(&self, expr: &str, base_dir: &Path, json: bool) -> Result<String> {
        let store = self.store()?;
        let (expr, base_dir) = (expr.to_string(), base_dir.to_path_buf());
        self.session(&store, &NullSink, &[], move |s| {
            let ev = s.evaluator();
            let value = ev.eval_str(&expr, &base_dir)?;
            Ok(if json {
                ev.to_json(&value)?.to_string()
            } else {
                ev.print(&value)?
            })
        })
    }

    /// Build a package and run its main program with `args`: `bin/<package>`
    /// or `bin/<project name>`, or the only program in `bin`.
    pub fn run(
        &self,
        project: &Project,
        package: Option<&str>,
        args: &[String],
        events: &dyn EventSink,
    ) -> Result<ExitStatus> {
        let out = self.build(project, package, events)?;
        let bin = self.config.store_dir.join(out[0].to_string()).join("bin");
        let mut names = vec![project.manifest.project.name.clone()];
        if let Some(p) = package.filter(|p| *p != "default") {
            names.insert(0, p.to_string());
        }
        let program = names
            .iter()
            .flat_map(|n| [bin.join(n), bin.join(format!("{n}.exe"))])
            .find(|p| p.is_file())
            .or_else(|| {
                let entries: Vec<PathBuf> = std::fs::read_dir(&bin)
                    .ok()?
                    .filter_map(|e| Some(e.ok()?.path()))
                    .filter(|p| p.is_file())
                    .collect();
                (entries.len() == 1).then(|| entries[0].clone())
            })
            .ok_or_else(|| {
                Error::Run(format!(
                    "no program to run in {}: expected bin/{}",
                    bin.display(),
                    names.join(" or bin/")
                ))
            })?;
        Ok(Command::new(&program).args(args).status()?)
    }

    // ----- environments -----

    /// The neki-shim launcher, added to the store.
    fn shim(&self, store: &LocalStore) -> Result<StorePath> {
        let exe = if let Some(p) = &self.config.shim {
            p.clone()
        } else {
            let name = if cfg!(windows) {
                "neki-shim.exe"
            } else {
                "neki-shim"
            };
            let me = std::env::current_exe()?;
            let dir = me.parent().map(Path::to_path_buf).unwrap_or_default();
            // Next to `neki`, or one level up when running from Cargo's
            // `target/<profile>/deps`.
            [
                dir.join(name),
                dir.parent().map(|d| d.join(name)).unwrap_or_default(),
            ]
            .into_iter()
            .find(|p| p.is_file())
            .ok_or_else(|| {
                Error::Run(format!(
                    "cannot find {name} next to {}; set NEKI_SHIM to its path",
                    me.display()
                ))
            })?
        };
        Ok(store.add_path("neki-shim", &exe)?)
    }

    /// Build `packages` (resolved in `scope`) and merge them into one
    /// environment.
    fn environment(
        &self,
        store: &Arc<LocalStore>,
        name: &str,
        project: Option<&Project>,
        packages: &[String],
        events: &dyn EventSink,
    ) -> Result<StorePath> {
        let targets = self.session(store, events, &[], |s| {
            let scope = match project {
                Some(p) => Scope::Project(p),
                None => Scope::Registry,
            };
            packages
                .iter()
                .map(|p| s.resolve(&scope, p))
                .collect::<Result<Vec<_>>>()
        })?;
        let outputs = self.realise(store, &targets, events)?;
        let shim = self.shim(store)?;
        Ok(neki_profile::build_env(
            store,
            &session::store_name(name),
            &outputs,
            &shim,
        )?)
    }

    /// Environment of the project's dev shell, with all its packages built.
    pub fn develop(&self, project: &Project, events: &dyn EventSink) -> Result<ShellEnv> {
        let store = self.store()?;
        let _gc = store.gc_guard()?;
        let dev = project.manifest.dev_shell.clone().unwrap_or_default();
        let name = format!("{}-dev-env", project.manifest.project.name);
        let env = self.environment(&store, &name, Some(project), &dev.packages, events)?;
        let root = std::path::absolute(&project.root)?;
        store.add_root(&format!("develop:{}", root.display()), &env)?;
        let mut vars = dev.env.clone();
        vars.insert("NEKI_SHELL".into(), project.manifest.project.name.clone());
        Ok(ShellEnv {
            vars,
            path_prepend: vec![store.store_dir().to_path(&env).join("bin")],
            hook: dev.shell_hook.clone(),
        })
    }

    /// Environment with `packages` (`<input>.<package>`) available, using
    /// `project`'s inputs, or the registry outside projects. The garbage
    /// collector keeps it until the returned [`AdHocShell`] is dropped.
    pub fn shell(
        &self,
        project: Option<&Project>,
        packages: &[String],
        events: &dyn EventSink,
    ) -> Result<AdHocShell> {
        let store = self.store()?;
        let _gc = store.gc_guard()?;
        let env = self.environment(&store, "shell-env", project, packages, events)?;
        // Taken before the GC guard is released, so there's no gap.
        let root = store.add_temp_root(&env)?;
        Ok(AdHocShell {
            env: ShellEnv {
                vars: BTreeMap::from([("NEKI_SHELL".to_string(), packages.join(" "))]),
                path_prepend: vec![store.store_dir().to_path(&env).join("bin")],
                hook: None,
            },
            _root: root,
        })
    }

    /// Lock missing inputs, re-resolve the inputs named in `update`, and
    /// write `neki.lock`.
    pub fn lock(
        &self,
        project: &Project,
        update: &[String],
        events: &dyn EventSink,
    ) -> Result<LockFile> {
        for name in update {
            if !project.manifest.inputs.contains_key(name) {
                return Err(Error::Input(format!("there is no input named `{name}`")));
            }
        }
        let store = self.store()?;
        self.session(&store, events, update, |s| {
            let scope = Scope::Project(project);
            s.lock_all(&scope)?;
            s.lock_of(&scope)
        })
    }

    /// Open the project's dev shell in a new terminal window, in the project
    /// folder (for the desktop app).
    pub fn open_dev_shell(&self, project: &Project, events: &dyn EventSink) -> Result<()> {
        let env = self.develop(project, events)?;
        let root = std::path::absolute(&project.root)?;
        let root = root.display().to_string();
        let posix = format!(
            "cd '{}'\n{}",
            root.replace('\'', r"'\''"),
            render_activation(&env, Shell::Posix)
        );
        let powershell = format!(
            "Set-Location '{}'\n{}",
            root.replace('\'', "''"),
            render_activation(&env, Shell::PowerShell)
        );
        let dir = self.config.state_dir.join("tmp");
        neki_platform::shell::open_terminal(&dir, &posix, &powershell)
            .map_err(|e| Error::Run(format!("cannot open a terminal: {e}")))
    }

    /// Run the user's interactive shell with `env` applied and wait for it.
    pub fn spawn_shell(&self, env: &ShellEnv) -> Result<ExitStatus> {
        let (shell, program) = Shell::detect();
        let mut cmd = neki_profile::shell_command(shell, &program, env.hook.as_deref());
        env.apply_to(&mut cmd);
        Ok(cmd.status()?)
    }

    // ----- the store -----

    /// Copy the file or directory at `path` into the store, named after its
    /// last component.
    pub fn store_add(&self, path: &Path) -> Result<StorePath> {
        // Resolve `.` and `..` so they don't hide the name.
        let full = std::fs::canonicalize(path).map_err(|source| Error::File {
            path: path.to_path_buf(),
            source,
        })?;
        let name = full
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::NoName(path.to_path_buf()))?;
        Ok(self.store()?.add_path(name, path)?)
    }

    /// Metadata of a valid store path, given as `<hash>-<name>` or as its
    /// full location in the store.
    pub fn path_info(&self, path: &str) -> Result<PathInfo> {
        let store = self.store()?;
        let parsed = parse_store_path(store.store_dir(), path)?;
        store
            .query_path_info(&parsed)?
            .ok_or_else(|| Error::NotInStore(path.to_string()))
    }

    /// Every valid store path.
    pub fn store_paths(&self) -> Result<Vec<StorePath>> {
        Ok(self.store()?.valid_paths()?)
    }

    /// `path` and everything it refers to, recursively.
    pub fn closure(&self, path: &str) -> Result<Vec<PathInfo>> {
        let store = self.store()?;
        let parsed = parse_store_path(store.store_dir(), path)?;
        store
            .closure(std::slice::from_ref(&parsed))?
            .into_iter()
            .map(|p| {
                store
                    .query_path_info(&p)?
                    .ok_or_else(|| Error::NotInStore(p.to_string()))
            })
            .collect()
    }

    /// The chains of references by which `from` depends on `to` (shortest
    /// first), like `nix why-depends`.
    pub fn why_depends(&self, from: &str, to: &str) -> Result<Vec<Vec<StorePath>>> {
        let store = self.store()?;
        let (from, to) = (
            parse_store_path(store.store_dir(), from)?,
            parse_store_path(store.store_dir(), to)?,
        );
        // Breadth-first, remembering how each path was reached.
        let mut parent: BTreeMap<StorePath, Option<StorePath>> =
            BTreeMap::from([(from.clone(), None)]);
        let mut queue = std::collections::VecDeque::from([from]);
        let mut found = BTreeSet::new();
        while let Some(p) = queue.pop_front() {
            let info = store
                .query_path_info(&p)?
                .ok_or_else(|| Error::NotInStore(p.to_string()))?;
            for r in info.references {
                if r == p {
                    continue;
                }
                if r == to {
                    found.insert(p.clone());
                }
                if !parent.contains_key(&r) {
                    parent.insert(r.clone(), Some(p.clone()));
                    queue.push_back(r);
                }
            }
        }
        let chain_to = |mut p: StorePath| {
            let mut chain = vec![to.clone(), p.clone()];
            while let Some(Some(prev)) = parent.get(&p) {
                chain.push(prev.clone());
                p = prev.clone();
            }
            chain.reverse();
            chain
        };
        let mut chains: Vec<Vec<StorePath>> = found.into_iter().map(chain_to).collect();
        chains.sort_by_key(Vec::len);
        Ok(chains)
    }

    /// Paths whose contents no longer match their recorded hash.
    pub fn verify(&self) -> Result<Vec<StorePath>> {
        Ok(self.store()?.verify()?)
    }

    /// Repair store paths whose contents were modified (`paths`, or every
    /// path `verify` reports): replace them with a copy from a configured
    /// binary cache, or else by building their derivation again. A copy is
    /// only used if it has exactly the recorded hash.
    pub fn repair(&self, paths: &[String], events: &dyn EventSink) -> Result<RepairReport> {
        let store = self.store()?;
        let _gc = store.gc_guard()?;
        let modified = store.verify()?;
        let mut targets = Vec::new();
        if paths.is_empty() {
            targets = modified;
        } else {
            for p in paths {
                let path = parse_store_path(store.store_dir(), p)?;
                if modified.contains(&path) {
                    targets.push(path);
                } else {
                    events.event(Event::Message(format!("{path} is intact")));
                }
            }
        }
        let caches = self.caches()?;
        let opts = self.build_options()?;
        let mut report = RepairReport::default();
        for path in targets {
            events.event(Event::Message(format!("repairing {path}")));
            match self.repair_one(&store, &path, &caches, &opts, events) {
                Ok(how) => {
                    events.event(Event::Message(format!("repaired {path} from {how}")));
                    report.repaired.push(path);
                }
                Err(why) => report.failed.push((path, why)),
            }
        }
        Ok(report)
    }

    fn repair_one(
        &self,
        store: &LocalStore,
        path: &StorePath,
        caches: &[BinaryCache],
        opts: &neki_build::BuildOptions,
        events: &dyn EventSink,
    ) -> Result<String, String> {
        let mut problems = Vec::new();
        for cache in caches {
            let tmp = store.temp_entry();
            match cache.fetch_into(store, path, tmp.path()) {
                Ok(true) => match store.replace_contents(path, tmp.path()) {
                    Ok(()) => return Ok(format!("cache {}", cache.url)),
                    Err(e) => problems.push(format!("{}: {e}", cache.url)),
                },
                Ok(false) => {}
                Err(e) => problems.push(format!("{}: {e}", cache.url)),
            }
        }
        let deriver = store
            .query_path_info(path)
            .map_err(|e| e.to_string())?
            .and_then(|info| info.deriver)
            .filter(|drv| store.is_valid_path(drv).unwrap_or(false));
        if let Some(drv) = deriver {
            let rebuilt = (|| -> Result<()> {
                let outputs = store
                    .read_derivation(&drv)?
                    .output_paths(store.store_dir())?;
                let output = outputs
                    .iter()
                    .find(|(_, p)| *p == path)
                    .map(|(name, _)| name.clone())
                    .ok_or_else(|| Error::Run(format!("{drv} doesn't build {path}")))?;
                let tmp = store.temp_entry();
                neki_build::build_into(store, &drv, &output, tmp.path(), opts, &|event| {
                    forward(events, event)
                })?;
                store
                    .replace_contents(path, tmp.path())
                    .map_err(|e| match e {
                        neki_store::Error::HashMismatch { .. } => Error::Run(format!(
                            "building {drv} again didn't give the same bytes ({e})"
                        )),
                        e => e.into(),
                    })?;
                Ok(())
            })();
            match rebuilt {
                Ok(()) => return Ok(format!("building {drv} again")),
                Err(e) => problems.push(e.to_string()),
            }
        }
        if problems.is_empty() {
            problems
                .push("no binary cache has it, and there is no derivation to build it from".into());
        }
        Err(problems.join("; "))
    }

    /// Delete unreachable paths (with `delete_older_than`, after deleting
    /// old profile generations).
    pub fn gc(&self, opts: &GcOptions) -> Result<GcResult> {
        let store = self.store()?;
        if let (Some(age), false) = (opts.delete_older_than, opts.dry_run) {
            for profile in Profile::list(&self.config.state_dir)? {
                profile.delete_older_than(&store, age)?;
            }
        }
        Ok(store.collect_garbage(opts)?)
    }

    /// The GC roots.
    pub fn roots(&self) -> Result<Vec<Root>> {
        Ok(self.store()?.roots()?)
    }

    /// Deduplicate store files; returns bytes saved.
    pub fn optimise(&self) -> Result<u64> {
        Ok(self.store()?.optimise()?)
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

    // ----- profiles -----

    pub fn profile(&self, name: &str) -> Profile {
        Profile::open(&self.config.state_dir, name)
    }

    /// Every profile.
    pub fn profiles(&self) -> Result<Vec<Profile>> {
        Ok(Profile::list(&self.config.state_dir)?)
    }

    /// Make a new generation of `profile` with `packages` (registry
    /// `<input>.<package>` names).
    fn profile_set(
        &self,
        profile: &str,
        packages: Vec<String>,
        events: &dyn EventSink,
    ) -> Result<Generation> {
        let store = self.store()?;
        let _gc = store.gc_guard()?;
        let env = self.environment(
            &store,
            &format!("profile-{profile}"),
            None,
            &packages,
            events,
        )?;
        Ok(self.profile(profile).switch(&store, &env, &packages)?)
    }

    pub fn profile_install(
        &self,
        profile: &str,
        packages: &[String],
        events: &dyn EventSink,
    ) -> Result<Generation> {
        let mut list = self
            .profile(profile)
            .current()?
            .map(|g| g.packages)
            .unwrap_or_default();
        for p in packages {
            if !list.contains(p) {
                list.push(p.clone());
            }
        }
        self.profile_set(profile, list, events)
    }

    pub fn profile_remove(
        &self,
        profile: &str,
        packages: &[String],
        events: &dyn EventSink,
    ) -> Result<Generation> {
        let current = self
            .profile(profile)
            .current()?
            .map(|g| g.packages)
            .unwrap_or_default();
        for p in packages {
            if !current.contains(p) {
                return Err(Error::Input(format!(
                    "{p} is not installed in profile {profile}"
                )));
            }
        }
        let list = current
            .into_iter()
            .filter(|p| !packages.contains(p))
            .collect();
        self.profile_set(profile, list, events)
    }

    /// Rebuild every package of the profile from the newest inputs.
    pub fn profile_upgrade(&self, profile: &str, events: &dyn EventSink) -> Result<Generation> {
        let list = self
            .profile(profile)
            .current()?
            .map(|g| g.packages)
            .unwrap_or_default();
        let store = self.store()?;
        // Re-resolve every registry input.
        let registry = store.state_dir().join("registry.lock");
        if registry.exists() {
            std::fs::remove_file(&registry).map_err(|source| Error::File {
                path: registry,
                source,
            })?;
        }
        self.profile_set(profile, list, events)
    }

    pub fn profile_rollback(&self, profile: &str) -> Result<Generation> {
        Ok(self.profile(profile).rollback(&*self.store()?)?)
    }

    pub fn profile_switch(&self, profile: &str, generation: u32) -> Result<Generation> {
        Ok(self
            .profile(profile)
            .switch_to(&*self.store()?, generation)?)
    }

    pub fn profile_generations(&self, profile: &str) -> Result<Vec<Generation>> {
        Ok(self.profile(profile).generations()?)
    }

    // ----- binary caches -----

    fn secret_key(&self, file: Option<&Path>) -> Result<SecretKey> {
        let file = file
            .map(Path::to_path_buf)
            .or_else(|| self.config.secret_key_file.clone())
            .ok_or_else(|| {
                Error::Input(
                    "no signing key: pass --key-file or set secret-key-file in config.toml".into(),
                )
            })?;
        let text =
            std::fs::read_to_string(&file).map_err(|source| Error::File { path: file, source })?;
        Ok(SecretKey::parse(&text)?)
    }

    /// Upload the closures of `paths` to the cache at `url` (`file://`, or
    /// an HTTP `neki store serve --allow-push`), signed with the key in
    /// `key_file` (or `secret-key-file`). Returns the paths uploaded.
    pub fn cache_push(
        &self,
        url: &str,
        paths: &[String],
        key_file: Option<&Path>,
    ) -> Result<Vec<StorePath>> {
        let key = self.secret_key(key_file)?;
        let store = self.store()?;
        let paths = paths
            .iter()
            .map(|p| parse_store_path(store.store_dir(), p))
            .collect::<Result<Vec<_>>>()?;
        Ok(BinaryCache::new(url, Vec::new()).push(&store, &paths, &key)?)
    }

    /// Download `paths` (and what they refer to) from the cache at `url`.
    pub fn cache_fetch(&self, url: &str, paths: &[String]) -> Result<Vec<StorePath>> {
        let keys = self
            .config
            .trusted_public_keys
            .iter()
            .map(|k| PublicKey::parse(k))
            .collect::<Result<Vec<_>, _>>()?;
        let store = self.store()?;
        // Each path is imported after the paths it refers to, which aren't
        // rooted yet.
        let _gc = store.gc_guard()?;
        let cache = BinaryCache::new(url, keys);
        let mut fetched = Vec::new();
        for p in paths {
            let path = parse_store_path(store.store_dir(), p)?;
            if !cache.substitute(&store, &path)? {
                return Err(Error::NotInStore(format!("{p} (in cache {url})")));
            }
            fetched.push(path);
        }
        Ok(fetched)
    }

    /// A new signing key named `name`.
    pub fn cache_keygen(&self, name: &str) -> Result<SecretKey> {
        Ok(SecretKey::generate(name)?)
    }

    /// Serve the store as a binary cache at `addr` until the process ends,
    /// signing with the key in `key_file` if given. With `allow_push`,
    /// uploads signed by one of the `trusted-public-keys` are added to the
    /// store. `ready` gets the bound address.
    pub fn serve(
        &self,
        addr: &str,
        key_file: Option<&Path>,
        allow_push: bool,
        ready: impl FnOnce(SocketAddr),
    ) -> Result<()> {
        let key = key_file.map(|f| self.secret_key(Some(f))).transpose()?;
        let trusted = self
            .config
            .trusted_public_keys
            .iter()
            .map(|k| PublicKey::parse(k))
            .collect::<Result<Vec<_>, _>>()?;
        if allow_push && trusted.is_empty() {
            return Err(Error::Input(
                "--allow-push needs trusted-public-keys in config.toml: only uploads signed by those keys are accepted".into(),
            ));
        }
        let store = self.store()?;
        let server = neki_substitute::CacheServer::bind(addr)?;
        if let Some(a) = server.addr() {
            ready(a);
        }
        server.run(
            &store,
            key.as_ref(),
            allow_push.then_some(trusted.as_slice()),
        );
        Ok(())
    }
}

/// An ad-hoc environment, kept from the garbage collector while this value
/// lives (and never after this process ends).
#[derive(Debug)]
pub struct AdHocShell {
    pub env: ShellEnv,
    _root: neki_store::TempRoot,
}

/// A path given as `<hash>-<name>` or as its full location in the store.
pub fn parse_store_path(store_dir: &StoreDir, s: &str) -> Result<StorePath> {
    Ok(if Path::new(s).is_absolute() {
        store_dir.parse_path(Path::new(s))?
    } else {
        StorePath::parse(s)?
    })
}

/// What [`Neki::repair`] did.
#[derive(Debug, Default)]
pub struct RepairReport {
    pub repaired: Vec<StorePath>,
    /// Paths that couldn't be repaired, and why.
    pub failed: Vec<(StorePath, String)>,
}

/// Pass a builder event on to `events`.
fn forward(events: &dyn EventSink, event: neki_build::BuildEvent) {
    events.event(match event {
        neki_build::BuildEvent::Planned { drv, waits_for } => {
            Event::BuildPlanned { drv, waits_for }
        }
        neki_build::BuildEvent::Started { drv } => Event::BuildStarted { drv },
        neki_build::BuildEvent::Log { drv, line } => Event::BuildLog { drv, line },
        neki_build::BuildEvent::Finished { drv, success } => Event::BuildFinished { drv, success },
        neki_build::BuildEvent::Substituted { path } => Event::Substituted { path },
    })
}

/// A binary cache as the builder's substituter.
struct CacheSubstituter(BinaryCache);

impl neki_build::Substituter for CacheSubstituter {
    fn substitute(&self, store: &LocalStore, path: &StorePath) -> Result<bool, String> {
        self.0.substitute(store, path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests;
