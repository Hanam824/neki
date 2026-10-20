//! End-to-end tests through the API the front-ends use, without network:
//! a local package set (`path:` input), a project using it, environments,
//! profiles, garbage collection and binary caches.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

/// A package set with one package, `greeter`, providing a `greet` program.
const PKGS: &str = r#"
let
  isWindows = builtins.match ".*-windows" builtins.currentSystem != null;
  sh = script:
    if isWindows
    then [ "C:\\Windows\\System32\\cmd.exe" "/d" "/c" script.windows ]
    else [ "/bin/sh" "-c" "PATH=/bin:/usr/bin; ${script.unix}" ];
  mk = name: script: derivation {
    inherit name;
    system = builtins.currentSystem;
    builder = "builtin:run";
    commands = builtins.toJSON [ (sh script) ];
  };
in {
  greeter = mk "greeter" {
    unix = "mkdir -p $out/bin && printf '#!/bin/sh\\necho hello from greeter\\n' > $out/bin/greet && chmod +x $out/bin/greet";
    windows = "mkdir %out%\\bin && (echo @echo hello from greeter) > %out%\\bin\\greet.cmd";
  };
  tools.other = mk "other" {
    unix = "mkdir -p $out/share && echo other > $out/share/other";
    windows = "mkdir %out%\\share && echo other> %out%\\share\\other";
  };
}
"#;

fn project_manifest() -> String {
    let build = if cfg!(windows) {
        r#"[["C:\\Windows\\System32\\cmd.exe", "/d", "/c", "mkdir %out% && greet > %out%\\greeting.txt && copy main.txt %out%\\main.txt"]]"#
    } else {
        r#"[["/bin/sh", "-c", "PATH=$PATH:/bin:/usr/bin; mkdir -p $out && greet > $out/greeting.txt && cp main.txt $out/main.txt"]]"#
    };
    format!(
        r#"[project]
name = "app"

[inputs]
pkgs = {{ url = "path:../pkgs" }}

[dev-shell]
packages = ["pkgs.greeter", "pkgs.tools.other"]
env = {{ GREETING = "hi" }}
shell-hook = "echo entering"

[packages.default]
src = "."
build-inputs = ["pkgs.greeter"]
build = {build}
exclude = ["ignored.txt"]
"#
    )
}

struct Env {
    neki: Neki,
    root: PathBuf,
    _dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        Self::with_state(dir, &root, "var")
    }

    /// A second "machine": its own state, but the same store dir.
    fn with_state(dir: tempfile::TempDir, root: &Path, state: &str) -> Self {
        fs::create_dir_all(root.join("pkgs")).unwrap();
        fs::write(root.join("pkgs/default.neki"), PKGS).unwrap();
        fs::create_dir_all(root.join("app")).unwrap();
        fs::write(root.join("app/neki.toml"), project_manifest()).unwrap();
        fs::write(root.join("app/main.txt"), "main").unwrap();
        fs::write(root.join("app/ignored.txt"), "ignored").unwrap();
        let shim = root.join("fake-shim");
        fs::write(&shim, "shim").unwrap();
        let config = Config {
            store_dir: root.join("store"),
            state_dir: root.join(state),
            shim: Some(shim),
            max_jobs: 2,
            registry: BTreeMap::from([(
                "test".to_string(),
                format!("path:{}", root.join("pkgs").display()),
            )]),
            ..Config::default()
        };
        Self {
            neki: Neki::new(config),
            root: root.to_path_buf(),
            _dir: dir,
        }
    }

    fn project(&self) -> Project {
        Project::load(&self.root.join("app")).unwrap()
    }

    fn location(&self, path: &StorePath) -> PathBuf {
        self.neki.config().store_dir.join(path.to_string())
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = neki_platform::fs::remove_tree(&self.root);
    }
}

#[test]
fn builds_a_project_with_locked_inputs() {
    let env = Env::new();
    let out = env.neki.build(&env.project(), None, &NullSink).unwrap();
    let location = env.location(&out[0]);
    let greeting = fs::read_to_string(location.join("greeting.txt")).unwrap();
    assert!(greeting.contains("hello from greeter"), "{greeting}");
    assert_eq!(
        fs::read_to_string(location.join("main.txt")).unwrap(),
        "main"
    );

    // The `result` link and the lock file.
    let result = env.root.join("app/result");
    assert_eq!(
        fs::canonicalize(&result).unwrap(),
        fs::canonicalize(&location).unwrap()
    );
    let lock = env.project().lock.expect("neki.lock was written");
    assert!(lock.inputs["pkgs"].nar_hash.starts_with("sha256-"));

    // Unchanged: the same output; the source leaves out excluded files.
    let again = env.neki.build(&env.project(), None, &NullSink).unwrap();
    assert_eq!(again, out);
    let drv = env
        .neki
        .path_info(&out[0].to_string())
        .unwrap()
        .deriver
        .unwrap();
    let derivation = env.neki.store().unwrap().read_derivation(&drv).unwrap();
    let src = PathBuf::from(&derivation.env["src"]);
    assert!(src.join("main.txt").exists() && !src.join("ignored.txt").exists());
    assert!(!src.join("result").exists());

    // A changed source gives a new output.
    fs::write(env.root.join("app/main.txt"), "changed").unwrap();
    let changed = env.neki.build(&env.project(), None, &NullSink).unwrap();
    assert_ne!(changed, out);

    // The old output is now garbage, the new one is kept by `result`.
    let gc = env.neki.gc(&GcOptions::default()).unwrap();
    assert!(gc.deleted.contains(&out[0]));
    assert!(
        env.neki
            .store()
            .unwrap()
            .is_valid_path(&changed[0])
            .unwrap()
    );
}

#[test]
fn errors_name_the_problem() {
    let env = Env::new();
    let err = env
        .neki
        .build(&env.project(), Some("missing"), &NullSink)
        .unwrap_err();
    assert!(err.to_string().contains("no package `missing`"), "{err}");
    let err = env
        .neki
        .shell(Some(&env.project()), &["nope.x".into()], &NullSink)
        .unwrap_err();
    assert!(err.to_string().contains("no input named `nope`"), "{err}");
    let err = env
        .neki
        .shell(None, &["test.absent".into()], &NullSink)
        .unwrap_err();
    assert!(err.to_string().contains("absent"), "{err}");
    let err = env
        .neki
        .lock(&env.project(), &["nope".into()], &NullSink)
        .unwrap_err();
    assert!(err.to_string().contains("no input named `nope`"), "{err}");
}

#[test]
fn develop_and_shell_environments() {
    let env = Env::new();
    let shell = env.neki.develop(&env.project(), &NullSink).unwrap();
    assert_eq!(shell.vars["GREETING"], "hi");
    assert_eq!(shell.hook.as_deref(), Some("echo entering"));
    let bin = &shell.path_prepend[0];
    let greet = if cfg!(windows) {
        bin.join("greet.exe")
    } else {
        bin.join("greet")
    };
    // The program is a shim (here the fake one) with a sidecar.
    assert_eq!(fs::read_to_string(&greet).unwrap(), "shim");
    let sidecar = fs::read_to_string(neki_profile::shim_sidecar(&greet)).unwrap();
    assert!(sidecar.contains("-greeter"), "{sidecar}");
    assert!(bin.parent().unwrap().join("share/other").is_file());
    // Kept by a GC root while the project exists.
    assert!(
        env.neki
            .roots()
            .unwrap()
            .iter()
            .any(|r| r.name.starts_with("develop:"))
    );

    let adhoc = env
        .neki
        .shell(None, &["test.greeter".into()], &NullSink)
        .unwrap();
    let bin = adhoc.env.path_prepend[0].clone();
    // Kept while the shell lives, collectable afterwards.
    env.neki.gc(&GcOptions::default()).unwrap();
    assert!(bin.exists());
    drop(adhoc);
    env.neki.gc(&GcOptions::default()).unwrap();
    assert!(!bin.exists());
    let script = render_activation(&shell, Shell::PowerShell);
    assert!(script.contains("$env:GREETING = 'hi'"));
}

#[test]
fn profiles_install_remove_and_roll_back() {
    let env = Env::new();
    let g1 = env
        .neki
        .profile_install("default", &["test.greeter".into()], &NullSink)
        .unwrap();
    let g2 = env
        .neki
        .profile_install("default", &["test.tools.other".into()], &NullSink)
        .unwrap();
    assert_eq!(g2.packages, ["test.greeter", "test.tools.other"]);
    let current = env.neki.profile("default").current_link();
    assert!(current.join("share/other").is_file());

    let g3 = env
        .neki
        .profile_remove("default", &["test.greeter".into()], &NullSink)
        .unwrap();
    assert_eq!(g3.packages, ["test.tools.other"]);
    assert!(
        env.neki
            .profile_remove("default", &["test.greeter".into()], &NullSink)
            .is_err()
    );
    assert_eq!(env.neki.profile_rollback("default").unwrap().number, 2);
    assert_eq!(env.neki.profile_switch("default", 1).unwrap(), g1);
    assert_eq!(env.neki.profile_generations("default").unwrap().len(), 3);

    // Deleting old generations (not the current one, 1) frees their environments.
    let gc = env
        .neki
        .gc(&GcOptions {
            dry_run: false,
            delete_older_than: Some(Duration::ZERO),
        })
        .unwrap();
    assert!(!gc.deleted.is_empty());
    assert_eq!(env.neki.profile_generations("default").unwrap(), [g1]);
}

#[test]
fn lock_and_update() {
    let env = Env::new();
    let lock = env.neki.lock(&env.project(), &[], &NullSink).unwrap();
    let before = lock.inputs["pkgs"].nar_hash.clone();
    fs::write(env.root.join("pkgs/extra.neki"), "1").unwrap();
    // `path:` inputs are re-read every time; the lock follows.
    let lock = env
        .neki
        .lock(&env.project(), &["pkgs".into()], &NullSink)
        .unwrap();
    assert_ne!(lock.inputs["pkgs"].nar_hash, before);
}

#[test]
fn evaluates_and_builds_neki_files() {
    let env = Env::new();
    assert_eq!(env.neki.eval("1 + 2", &env.root, false).unwrap(), "3");
    assert_eq!(
        env.neki.eval("{ a = [ 1 ]; }", &env.root, true).unwrap(),
        r#"{"a":[1]}"#
    );
    let file = env.root.join("pkgs/default.neki");
    let drvs = env.neki.instantiate(&file, Some("greeter")).unwrap();
    assert_eq!(drvs.len(), 1);
    assert!(drvs[0].name().ends_with("greeter.drv"));
    // The same file always gives the same derivation.
    assert_eq!(env.neki.instantiate(&file, Some("greeter")).unwrap(), drvs);
    let outs = env
        .neki
        .build_file(&file, Some("greeter"), &NullSink)
        .unwrap();
    assert!(env.location(&outs[0]).join("bin").is_dir());
    // A `.drv` can be built directly too.
    assert_eq!(
        env.neki
            .build_drvs(&[drvs[0].to_string()], &NullSink)
            .unwrap(),
        outs
    );

    let chains = env
        .neki
        .why_depends(&outs[0].to_string(), &outs[0].to_string())
        .unwrap();
    assert!(chains.is_empty());
    assert!(!env.neki.closure(&outs[0].to_string()).unwrap().is_empty());
}

#[test]
fn push_and_fetch_through_a_cache() {
    let a = Env::new();
    let out = a
        .neki
        .build_file(
            &a.root.join("pkgs/default.neki"),
            Some("greeter"),
            &NullSink,
        )
        .unwrap();
    let key = a.neki.cache_keygen("test-cache-1").unwrap();
    let key_file = a.root.join("key");
    fs::write(&key_file, key.to_text()).unwrap();
    let cache = format!(
        "file://{}",
        a.root
            .join("cache")
            .display()
            .to_string()
            .replace('\\', "/")
    );
    let pushed = a
        .neki
        .cache_push(&cache, &[out[0].to_string()], Some(&key_file))
        .unwrap();
    assert!(pushed.contains(&out[0]));

    // Machine B: same store dir, its own state, trusts the key, and gets the
    // package from the cache instead of building it.
    let dir_b = tempfile::tempdir().unwrap();
    let mut b = Env::with_state(dir_b, &a.root, "var-b");
    let mut config = b.neki.config().clone();
    config.substituters = vec![cache.clone()];
    config.trusted_public_keys = vec![key.public().to_string()];
    b.neki = Neki::new(config);
    let events = std::sync::Mutex::new(Vec::new());
    struct Collect<'a>(&'a std::sync::Mutex<Vec<Event>>);
    impl EventSink for Collect<'_> {
        fn event(&self, event: Event) {
            self.0.lock().unwrap().push(event);
        }
    }
    let out_b = b
        .neki
        .build_file(
            &b.root.join("pkgs/default.neki"),
            Some("greeter"),
            &Collect(&events),
        )
        .unwrap();
    assert_eq!(out_b, out);
    let events = events.into_inner().unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Substituted { .. }))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::BuildStarted { .. }))
    );
    // `b` shares `a`'s directory; only `a` removes it.
    std::mem::forget(b);
}

#[test]
fn file_packages_are_stable_from_the_first_build() {
    let env = Env::new();
    let app = env.root.join("app");
    fs::write(
        app.join("package.neki"),
        r#"{ inputs, src, name, system, ... }: derivation {
          inherit name system;
          builder = "builtin:assemble";
          entries = builtins.toJSON [ { path = "src"; inherit src; } ];
        }"#,
    )
    .unwrap();
    let manifest = fs::read_to_string(app.join("neki.toml")).unwrap()
        + "\n[packages.viafile]\nfile = \"package.neki\"\n";
    fs::write(app.join("neki.toml"), manifest).unwrap();
    // No neki.lock yet: the first build writes it, and must not differ
    // from the next one because of that.
    let first = env
        .neki
        .build(&env.project(), Some("viafile"), &NullSink)
        .unwrap();
    let second = env
        .neki
        .build(&env.project(), Some("viafile"), &NullSink)
        .unwrap();
    assert_eq!(first, second);
    assert!(env.location(&first[0]).join("src/neki.lock").is_file());
}
