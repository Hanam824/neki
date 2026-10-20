use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use neki_hash::{StoreDir, StorePath};
use neki_platform::shell::Shell;
use neki_store::{LocalStore, Store};

use super::*;

struct Env {
    store: Option<LocalStore>,
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(
            StoreDir::new(dir.path().join("store")),
            dir.path().join("var"),
        )
        .unwrap();
        Self {
            store: Some(store),
            dir,
        }
    }

    fn store(&self) -> &LocalStore {
        self.store.as_ref().unwrap()
    }

    /// A package with `files` (paths ending in `*` are executable).
    fn package(&self, name: &str, files: &[(&str, &str)]) -> StorePath {
        let src = self.dir.path().join(format!("src-{name}"));
        for (file, contents) in files {
            let (file, exe) = match file.strip_suffix('*') {
                Some(f) => (f, true),
                None => (*file, false),
            };
            let path = src.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            if exe {
                neki_platform::fs::set_executable(&path).unwrap();
            }
        }
        self.store().add_path(name, &src).unwrap()
    }

    fn location(&self, path: &StorePath) -> PathBuf {
        self.store().store_dir().to_path(path)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        self.store = None;
        neki_platform::fs::remove_tree(self.dir.path()).unwrap();
    }
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("bin/{name}.exe*")
    } else {
        format!("bin/{name}*")
    }
}

#[test]
fn build_env_merges_packages_with_shims() {
    let env = Env::new();
    let shim_src = env.dir.path().join("neki-shim");
    fs::write(&shim_src, "shim binary").unwrap();
    let shim = env.store().add_path("neki-shim", &shim_src).unwrap();
    let a = env.package(
        "a",
        &[
            (&exe("tool"), "a-tool"),
            ("share/a.txt", "a"),
            ("share/both", "from a"),
        ],
    );
    let b = env.package(
        "b",
        &[
            (&exe("tool"), "b-tool"),
            (&exe("other"), "b-other"),
            ("share/both", "from b"),
        ],
    );

    let merged = build_env(env.store(), "env", &[a.clone(), b.clone()], &shim).unwrap();
    let root = env.location(&merged);
    let tool = if cfg!(windows) {
        root.join("bin/tool.exe")
    } else {
        root.join("bin/tool")
    };
    assert_eq!(fs::read_to_string(&tool).unwrap(), "shim binary");
    let sidecar = Shim::parse(&fs::read_to_string(shim_sidecar(&tool)).unwrap()).unwrap();
    // The first package wins.
    assert!(sidecar.target.starts_with(env.location(&a)));
    assert_eq!(sidecar.path, [env.location(&a).join("bin")]);
    assert_eq!(
        fs::read_to_string(root.join("share/both")).unwrap(),
        "from a"
    );
    assert_eq!(fs::read_to_string(root.join("share/a.txt")).unwrap(), "a");

    let info = env.store().query_path_info(&merged).unwrap().unwrap();
    assert!(info.references.contains(&a) && info.references.contains(&b));
    // The same packages give the same environment.
    assert_eq!(
        build_env(env.store(), "env", &[a, b], &shim).unwrap(),
        merged
    );
}

#[test]
fn shim_sidecar_names() {
    assert_eq!(
        shim_sidecar(Path::new("/x/node.exe")),
        Path::new("/x/node.shim")
    );
    assert_eq!(
        shim_sidecar(Path::new("/x/node.EXE")),
        Path::new("/x/node.shim")
    );
    assert_eq!(
        shim_sidecar(Path::new("/x/node")),
        Path::new("/x/node.shim")
    );
    assert_eq!(
        shim_sidecar(Path::new("/x/a.sh")),
        Path::new("/x/a.sh.shim")
    );
    let shim = Shim {
        target: "/s/bin/x".into(),
        path: vec!["/s/bin".into()],
        env: vec![("A".into(), "1 = 2".into())],
    };
    assert_eq!(Shim::parse(&shim.to_text()), Some(shim));
    assert_eq!(Shim::parse("path = /x\n"), None);
}

#[test]
fn generations_switch_and_roll_back() {
    let env = Env::new();
    let one = env.package("one", &[("f", "1")]);
    let two = env.package("two", &[("f", "2")]);
    let profile = Profile::open(env.store().state_dir(), "default");
    assert!(profile.current().unwrap().is_none());
    assert!(matches!(
        profile.rollback(env.store()),
        Err(Error::NoPreviousGeneration(_))
    ));

    let g1 = profile
        .switch(env.store(), &one, &["pkgs.one".into()])
        .unwrap();
    let g2 = profile
        .switch(env.store(), &two, &["pkgs.two".into()])
        .unwrap();
    assert_eq!((g1.number, g2.number), (1, 2));
    assert_eq!(profile.current().unwrap().unwrap(), g2);
    assert_eq!(
        fs::read_to_string(profile.current_link().join("f")).unwrap(),
        "2"
    );

    assert_eq!(profile.rollback(env.store()).unwrap(), g1);
    assert_eq!(
        fs::read_to_string(profile.current_link().join("f")).unwrap(),
        "1"
    );
    assert_eq!(profile.generations().unwrap(), [g1.clone(), g2.clone()]);
    // A new generation after a rollback gets the next number.
    let g3 = profile.switch(env.store(), &two, &[]).unwrap();
    assert_eq!(g3.number, 3);

    // Every generation is a GC root until it's deleted.
    let roots: Vec<String> = env
        .store()
        .roots()
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(
        roots,
        [
            "profile:default:1",
            "profile:default:2",
            "profile:default:3"
        ]
    );
    let day = Duration::from_secs(24 * 60 * 60);
    assert!(
        profile
            .delete_older_than(env.store(), day)
            .unwrap()
            .is_empty()
    );
    // The current generation (3) always stays.
    let deleted = profile
        .delete_older_than(env.store(), Duration::ZERO)
        .unwrap();
    assert_eq!(deleted, [1, 2]);
    assert_eq!(profile.generations().unwrap(), [g3]);
    assert_eq!(Profile::list(env.store().state_dir()).unwrap().len(), 1);
}

#[test]
fn activation_scripts() {
    let env = ShellEnv {
        vars: [("GREETING".to_string(), "it's".to_string())].into(),
        path_prepend: vec!["/env/bin".into()],
        hook: Some("echo ready".into()),
    };
    let posix = render_activation(&env, Shell::Posix);
    assert!(posix.contains("export GREETING='it'\\''s'\n"), "{posix}");
    assert!(posix.contains("export PATH='/env/bin'"), "{posix}");
    assert!(posix.ends_with("echo ready\n"));
    let ps = render_activation(&env, Shell::PowerShell);
    assert!(ps.contains("$env:GREETING = 'it''s'\n"), "{ps}");
    assert!(
        ps.contains("$env:PATH = '/env/bin' + [IO.Path]::PathSeparator + $env:PATH"),
        "{ps}"
    );
}
