use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use neki_derivation::{Derivation, DerivationOutput};
use neki_hash::{Hash, HashAlgo, StoreDir};
use neki_store::Store;

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

    /// Finalize `drv` and store it.
    fn add(&self, mut drv: Derivation) -> StorePath {
        let store = self.store();
        drv.finalize(store.store_dir(), &|p| {
            Ok(store.read_derivation(p).unwrap())
        })
        .unwrap();
        store.add_derivation(&drv).unwrap()
    }

    fn realise(&self, drvs: &[StorePath]) -> Result<Vec<BuildResult>> {
        realise(self.store(), drvs, &BuildOptions::default(), &|_| {})
    }

    fn out(&self, result: &BuildResult) -> std::path::PathBuf {
        self.store().store_dir().to_path(&result.outputs["out"])
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        self.store = None;
        neki_platform::fs::remove_tree(self.dir.path()).unwrap();
    }
}

/// A `builtin:run` derivation that runs `script` with the system shell
/// (`unix` with /bin/sh, `windows` with cmd.exe). The Windows scripts use
/// no double quotes: Rust escapes them as `\"`, which cmd.exe doesn't
/// understand (the test paths have no spaces).
fn script(name: &str, unix: &str, windows: &str) -> Derivation {
    let argv = if cfg!(windows) {
        let cmd =
            std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
        vec![cmd, "/d".into(), "/c".into(), windows.into()]
    } else {
        // Builds start with an empty PATH.
        vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("PATH=/bin:/usr/bin; {unix}"),
        ]
    };
    Derivation {
        outputs: BTreeMap::from([("out".into(), DerivationOutput::Deferred)]),
        input_drvs: BTreeMap::new(),
        input_srcs: BTreeSet::new(),
        system: neki_derivation::host_system(),
        builder: "builtin:run".into(),
        args: Vec::new(),
        env: BTreeMap::from([
            ("name".into(), name.into()),
            ("commands".into(), serde_json::to_string(&[argv]).unwrap()),
        ]),
    }
}

#[test]
fn builds_a_chain_in_order_and_records_references() {
    let env = Env::new();
    let dep = env.add(script(
        "dep",
        "mkdir -p \"$out\" && echo dep-data > \"$out/file\"",
        "mkdir %out% && echo dep-data> %out%\\file",
    ));
    let mut top = script(
        "top",
        "mkdir -p \"$out\" && cat \"$dep/file\" > \"$out/copy\" && echo \"$dep\" > \"$out/ref\"",
        "mkdir %out% && type %dep%\\file > %out%\\copy && echo %dep%> %out%\\ref",
    );
    top.input_drvs = BTreeMap::from([(dep.clone(), BTreeSet::from(["out".into()]))]);
    let dep_out = env
        .store()
        .read_derivation(&dep)
        .unwrap()
        .output_paths(env.store().store_dir())
        .unwrap()["out"]
        .clone();
    top.env.insert(
        "dep".into(),
        env.store()
            .store_dir()
            .to_path(&dep_out)
            .display()
            .to_string(),
    );
    let top = env.add(top);

    let results = env.realise(std::slice::from_ref(&top)).unwrap();
    assert!(results[0].built);
    let out = env.out(&results[0]);
    assert!(
        fs::read_to_string(out.join("copy"))
            .unwrap()
            .starts_with("dep-data")
    );
    let info = env
        .store()
        .query_path_info(&results[0].outputs["out"])
        .unwrap()
        .unwrap();
    assert_eq!(info.references, BTreeSet::from([dep_out]));
    assert_eq!(info.deriver.as_ref(), Some(&top));

    // Everything is valid now: nothing runs again.
    let again = env.realise(&[top]).unwrap();
    assert!(!again[0].built);
}

#[test]
fn failed_builds_leave_nothing_behind() {
    let env = Env::new();
    let drv = env.add(script(
        "fails",
        "mkdir -p \"$out\"; echo something went wrong; exit 3",
        "mkdir %out% & echo something went wrong& exit 3",
    ));
    let err = env.realise(std::slice::from_ref(&drv)).unwrap_err();
    let Error::BuildFailed { log, .. } = err else {
        panic!("{err}");
    };
    assert!(
        fs::read_to_string(log)
            .unwrap()
            .contains("something went wrong")
    );
    let out = &env
        .store()
        .read_derivation(&drv)
        .unwrap()
        .output_paths(env.store().store_dir())
        .unwrap()["out"];
    assert!(!env.store().is_valid_path(out).unwrap());
    assert!(!env.store().store_dir().to_path(out).exists());

    let missing = env.add(script("no-output", "true", "rem"));
    assert!(matches!(
        env.realise(&[missing]),
        Err(Error::BuildFailed { .. })
    ));
}

#[test]
fn environment_is_scrubbed() {
    let env = Env::new();
    let drv = env.add(script(
        "env",
        "mkdir -p \"$out\" && echo \"[$CARGO_PKG_NAME][$PATH]\" > \"$out/env\"",
        "mkdir %out% && echo [%CARGO_PKG_NAME%][%PATH%]> %out%\\env",
    ));
    let results = env.realise(&[drv]).unwrap();
    let text = fs::read_to_string(env.out(&results[0]).join("env")).unwrap();
    // Cargo sets CARGO_PKG_NAME for tests; the builder must not see it.
    assert!(!text.contains("neki-build"), "{text}");
    if cfg!(unix) {
        assert_eq!(text, "[][/bin:/usr/bin]\n");
    }
}

#[test]
fn run_expands_variables_and_copies_src() {
    let env = Env::new();
    let src = env.dir.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("input.txt"), "from src").unwrap();
    let src = env.store().add_path("src", &src).unwrap();

    let mut drv = script("copy", "", "");
    let copy = if cfg!(windows) {
        vec![
            std::env::var("ComSpec").unwrap(),
            "/d".into(),
            "/c".into(),
            "mkdir ${out} && copy input.txt ${out}\\got.txt && echo $$HOME> ${out}\\dollar".into(),
        ]
    } else {
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "PATH=/bin:/usr/bin; mkdir -p '${out}' && cp input.txt '${out}/got.txt' && echo '$$HOME' > '${out}/dollar'"
                .into(),
        ]
    };
    drv.env
        .insert("commands".into(), serde_json::to_string(&[copy]).unwrap());
    drv.env.insert(
        "src".into(),
        env.store().store_dir().to_path(&src).display().to_string(),
    );
    drv.input_srcs.insert(src);
    let results = env.realise(&[env.add(drv)]).unwrap();
    let out = env.out(&results[0]);
    assert_eq!(fs::read_to_string(out.join("got.txt")).unwrap(), "from src");
    assert!(
        fs::read_to_string(out.join("dollar"))
            .unwrap()
            .starts_with("$HOME")
    );
}

#[test]
fn fetchurl_checks_the_fixed_hash() {
    let env = Env::new();
    let file = env.dir.path().join("payload");
    fs::write(&file, "payload").unwrap();
    let fetch = |hash: Hash| {
        let mut drv = script("payload", "", "");
        drv.builder = "builtin:fetchurl".into();
        drv.env.remove("commands");
        drv.env.insert("url".into(), neki_fetch::file_url(&file));
        drv.outputs = BTreeMap::from([(
            "out".into(),
            DerivationOutput::Fixed {
                hash,
                recursive: false,
            },
        )]);
        env.add(drv)
    };
    let good = fetch(Hash::of_bytes(HashAlgo::Sha256, b"payload"));
    let results = env.realise(&[good]).unwrap();
    assert_eq!(fs::read_to_string(env.out(&results[0])).unwrap(), "payload");

    let bad = fetch(Hash::of_bytes(HashAlgo::Sha256, b"something else"));
    let err = env.realise(&[bad]).unwrap_err().to_string();
    assert!(
        err.contains("hash mismatch") && err.contains("got sha256-"),
        "{err}"
    );
}

#[test]
fn cancel_kills_the_builder() {
    let env = Env::new();
    let drv = env.add(script(
        "slow",
        "sleep 30",
        "%SystemRoot%\\System32\\PING.EXE -n 30 127.0.0.1 > nul",
    ));
    let opts = BuildOptions::default();
    let cancel = opts.cancel.clone();
    let start = Instant::now();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            cancel.store(true, Ordering::Relaxed);
        });
        let result = realise(env.store(), &[drv], &opts, &|_| {});
        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    });
    assert!(start.elapsed() < Duration::from_secs(15));
}

#[test]
fn substituters_are_tried_first() {
    struct Cache;
    impl Substituter for Cache {
        fn substitute(&self, store: &LocalStore, path: &StorePath) -> Result<bool, String> {
            // Pretend to download: create the output and register it.
            let location = store.store_dir().to_path(path);
            fs::write(&location, "from cache").map_err(|e| e.to_string())?;
            store
                .register_built(std::slice::from_ref(path), None, &BTreeSet::new(), false)
                .map_err(|e| e.to_string())?;
            Ok(true)
        }
    }
    let env = Env::new();
    // Would fail if it were built.
    let failing_dep = env.add(script("dep", "exit 1", "exit 1"));
    let mut top = script("top", "exit 1", "exit 1");
    top.input_drvs = BTreeMap::from([(failing_dep, BTreeSet::from(["out".into()]))]);
    let top = env.add(top);
    let opts = BuildOptions {
        substituters: vec![std::sync::Arc::new(Cache)],
        ..BuildOptions::default()
    };
    let results = realise(env.store(), &[top], &opts, &|_| {}).unwrap();
    assert!(!results[0].built);
    assert_eq!(
        fs::read_to_string(env.out(&results[0])).unwrap(),
        "from cache"
    );
}

/// A tar.gz with `top/comp-a/bin/tool` (executable), `top/comp-a/lib/a` and
/// `top/comp-b/lib/b`.
fn components_archive(path: &std::path::Path) {
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    for (name, mode, data) in [
        ("top/comp-a/bin/tool", 0o755, &b"#!/bin/sh\n"[..]),
        ("top/comp-a/lib/a", 0o644, b"a"),
        ("top/comp-b/lib/b", 0o644, b"b"),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_mode(mode);
        header.set_size(data.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, name, data).unwrap();
    }
    fs::write(path, tar.into_inner().unwrap().finish().unwrap()).unwrap();
}

fn derivation(name: &str, builder: &str, env: &[(&str, String)]) -> Derivation {
    let mut drv = script(name, "", "");
    drv.builder = builder.into();
    drv.env.remove("commands");
    drv.env
        .extend(env.iter().map(|(k, v)| (k.to_string(), v.clone())));
    drv
}

#[test]
fn unpack_and_assemble() {
    let env = Env::new();
    let archive = env.dir.path().join("components.tar.gz");
    components_archive(&archive);
    let src = env.store().add_path("components.tar.gz", &archive).unwrap();
    let src_path = env.store().store_dir().to_path(&src).display().to_string();
    let unpack = |name: &str, extra: &[(&str, &str)]| {
        let mut vars = vec![("src", src_path.clone())];
        vars.extend(extra.iter().map(|(k, v)| (*k, v.to_string())));
        let mut drv = derivation(name, "builtin:unpack", &vars);
        drv.input_srcs.insert(src.clone());
        let results = env.realise(&[env.add(drv)]).unwrap();
        env.out(&results[0])
    };

    // `auto` strips the single `top` directory.
    let auto = unpack("auto", &[("stripRoot", "auto")]);
    assert!(auto.join("comp-a/bin/tool").is_file());
    assert_eq!(
        fs::read_to_string(auto.join(neki_store::BIN_DIRS_FILE)).unwrap(),
        "comp-a/bin\n"
    );

    // `subdirs` merges components into one tree.
    let merged = unpack(
        "merged",
        &[("stripRoot", "1"), ("subdirs", "comp-a comp-b")],
    );
    assert!(merged.join("lib/a").is_file() && merged.join("lib/b").is_file());
    assert_eq!(neki_store::bin_dirs(&merged), [merged.join("bin")]);
    if cfg!(unix) {
        let meta = fs::metadata(merged.join("bin/tool")).unwrap();
        assert!(neki_platform::fs::is_executable(&meta));
    }

    // `install` puts a single file in place as a program.
    let installed = unpack("installed", &[("install", "bin/the-tool")]);
    assert!(installed.join("bin/the-tool").is_file());

    // `assemble` combines store paths and text.
    let entries = serde_json::json!([
        { "path": "vendor/a", "src": merged.display().to_string() },
        { "path": "vendor/a", "src": auto.display().to_string() },
        { "path": "bin/run", "text": "run", "executable": true },
    ]);
    let refs = [&merged, &auto]
        .iter()
        .map(|p| env.store().store_dir().parse_path(p).unwrap())
        .collect::<Vec<_>>();
    let mut drv = derivation(
        "assembled",
        "builtin:assemble",
        &[("entries", entries.to_string())],
    );
    drv.input_srcs.extend(refs.iter().cloned());
    let results = env.realise(&[env.add(drv)]).unwrap();
    let out = env.out(&results[0]);
    assert!(out.join("vendor/a/lib/a").is_file());
    assert!(
        out.join("vendor/a/comp-a/lib/a").is_file(),
        "merged from the second entry"
    );
    assert_eq!(fs::read_to_string(out.join("bin/run")).unwrap(), "run");
    let info = env
        .store()
        .query_path_info(&results[0].outputs["out"])
        .unwrap()
        .unwrap();
    assert!(info.references.is_empty() || info.references.iter().all(|r| refs.contains(r)));

    let bad = derivation(
        "bad",
        "builtin:assemble",
        &[("entries", r#"[{"path": "../x", "text": "no"}]"#.to_string())],
    );
    assert!(env.realise(&[env.add(bad)]).is_err());
}
