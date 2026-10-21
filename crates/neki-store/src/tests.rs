use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use neki_hash::{Hash, HashAlgo, Hasher, StoreDir};

use super::*;

/// A store in a temporary directory, removed (read-only entries included)
/// when dropped.
struct TestStore {
    store: Option<LocalStore>,
    dir: tempfile::TempDir,
}

impl TestStore {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        Self {
            store: Some(store),
            dir,
        }
    }

    fn store(&self) -> &LocalStore {
        self.store.as_ref().unwrap()
    }

    /// Close and reopen the store.
    fn reopen(&mut self) {
        self.store = None;
        self.store = Some(open(self.dir.path()));
    }

    /// A source tree to add, outside the store.
    fn source(&self, files: &[(&str, &str)]) -> std::path::PathBuf {
        let src = self.dir.path().join("src");
        let _ = fs::remove_dir_all(&src);
        for (name, contents) in files {
            let file = src.join(name);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, contents).unwrap();
        }
        src
    }
}

impl Drop for TestStore {
    fn drop(&mut self) {
        // Close the DB first: Windows can't delete open files.
        self.store = None;
        neki_platform::fs::remove_tree(self.dir.path()).unwrap();
    }
}

fn open(root: &Path) -> LocalStore {
    LocalStore::open(StoreDir::new(root.join("store")), root.join("var")).unwrap()
}

fn nar_hash(path: &Path) -> Hash {
    let mut hasher = Hasher::new(HashAlgo::Sha256);
    neki_nar::dump(path, &mut hasher).unwrap();
    hasher.finish()
}

#[test]
fn add_path_copies_and_registers() {
    let t = TestStore::new();
    let src = t.source(&[("a.txt", "hello"), ("sub/b.txt", "world")]);
    let path = t.store().add_path("hello-1.0", &src).unwrap();

    assert_eq!(path.name(), "hello-1.0");
    let hash = nar_hash(&src);
    assert_eq!(
        path,
        t.store()
            .store_dir()
            .make_path("source", &hash, "hello-1.0")
            .unwrap()
    );
    let dest = t.store().store_dir().to_path(&path);
    assert_eq!(fs::read_to_string(dest.join("sub/b.txt")).unwrap(), "world");
    assert_eq!(nar_hash(&dest), hash);
    assert!(
        fs::metadata(dest.join("a.txt"))
            .unwrap()
            .permissions()
            .readonly()
    );

    let info = t.store().query_path_info(&path).unwrap().unwrap();
    assert_eq!(info.nar_hash, hash);
    assert!(info.nar_size > 0);
    assert!(info.references.is_empty());
    assert!(info.registration_time > 0);

    // No temporary entries are left behind.
    let entries: Vec<_> = fs::read_dir(t.store().store_dir().path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(entries, [path.to_string()]);
}

#[test]
fn adding_again_is_idempotent() {
    let mut t = TestStore::new();
    let src = t.source(&[("a", "1")]);
    let path = t.store().add_path("a", &src).unwrap();
    let info = t.store().query_path_info(&path).unwrap().unwrap();
    t.reopen();
    assert_eq!(t.store().add_path("a", &src).unwrap(), path);
    let again = t.store().query_path_info(&path).unwrap().unwrap();
    assert_eq!(again.registration_time, info.registration_time);

    // The name and the contents both change the path.
    assert_ne!(t.store().add_path("b", &src).unwrap(), path);
    let other = t.source(&[("a", "2")]);
    assert_ne!(t.store().add_path("a", &other).unwrap(), path);
}

#[test]
fn add_path_accepts_a_single_file() {
    let t = TestStore::new();
    let src = t.source(&[("file.txt", "contents")]);
    let path = t
        .store()
        .add_path("file.txt", &src.join("file.txt"))
        .unwrap();
    let dest = t.store().store_dir().to_path(&path);
    assert_eq!(fs::read_to_string(dest).unwrap(), "contents");
}

#[test]
fn add_path_rejects_bad_names_and_sources() {
    let t = TestStore::new();
    let src = t.source(&[("a", "1")]);
    assert!(matches!(
        t.store().add_path("has space", &src),
        Err(Error::Hash(_))
    ));
    assert!(t.store().add_path("x", &src.join("missing")).is_err());
    if cfg!(unix) {
        // Unix can create a name that isn't valid on Windows; it can't be added.
        fs::write(src.join("aux.c"), "").unwrap();
        assert!(matches!(
            t.store().add_path("x", &src),
            Err(Error::Nar(neki_nar::Error::NonPortableName(_)))
        ));
    }
    assert_eq!(
        fs::read_dir(t.store().store_dir().path()).unwrap().count(),
        0
    );
}

#[test]
fn add_replaces_an_unregistered_leftover() {
    let t = TestStore::new();
    let src = t.source(&[("a", "1")]);
    let hash = nar_hash(&src);
    let path = t
        .store()
        .store_dir()
        .make_path("source", &hash, "a")
        .unwrap();
    // As if an earlier add was interrupted after the rename.
    let dest = t.store().store_dir().to_path(&path);
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("junk"), "").unwrap();

    assert_eq!(t.store().add_path("a", &src).unwrap(), path);
    assert_eq!(nar_hash(&dest), hash);
}

#[test]
fn text_references_and_closure() {
    let t = TestStore::new();
    let lib = t.store().add_path("lib", &t.source(&[("l", "1")])).unwrap();
    let tool = t
        .store()
        .add_path("tool", &t.source(&[("t", "2")]))
        .unwrap();
    let mid = t
        .store()
        .add_text("mid.drv", "uses lib", &BTreeSet::from([lib.clone()]))
        .unwrap();
    let top = t
        .store()
        .add_text(
            "top.drv",
            "uses mid and tool",
            &BTreeSet::from([mid.clone(), tool.clone()]),
        )
        .unwrap();

    let info = t.store().query_path_info(&top).unwrap().unwrap();
    assert_eq!(info.references, BTreeSet::from([mid.clone(), tool.clone()]));
    let dest = t.store().store_dir().to_path(&top);
    assert_eq!(fs::read_to_string(dest).unwrap(), "uses mid and tool");

    assert_eq!(
        t.store().closure(std::slice::from_ref(&top)).unwrap(),
        BTreeSet::from([top.clone(), mid.clone(), lib.clone(), tool])
    );
    assert_eq!(
        t.store().closure(std::slice::from_ref(&mid)).unwrap(),
        BTreeSet::from([mid.clone(), lib.clone()])
    );

    // The references are part of the path.
    let same_text = t
        .store()
        .add_text("mid.drv", "uses lib", &BTreeSet::new())
        .unwrap();
    assert_ne!(same_text, mid);
    assert_eq!(
        t.store()
            .add_text("mid.drv", "uses lib", &BTreeSet::from([lib]))
            .unwrap(),
        mid
    );
}

#[test]
fn unknown_paths_are_rejected() {
    let t = TestStore::new();
    let src = t.source(&[("a", "1")]);
    let unknown = t
        .store()
        .store_dir()
        .make_path("source", &nar_hash(&src), "unknown")
        .unwrap();
    assert!(t.store().query_path_info(&unknown).unwrap().is_none());
    assert!(matches!(
        t.store()
            .add_text("x", "", &BTreeSet::from([unknown.clone()])),
        Err(Error::NotValid(_))
    ));
    assert!(matches!(
        t.store().closure(&[unknown]),
        Err(Error::NotValid(_))
    ));
}

#[test]
fn verify_reports_modified_and_missing_paths() {
    let t = TestStore::new();
    let a = t.store().add_path("a", &t.source(&[("f", "1")])).unwrap();
    let b = t.store().add_path("b", &t.source(&[("f", "2")])).unwrap();
    let c = t.store().add_path("c", &t.source(&[("f", "3")])).unwrap();
    assert!(t.store().verify().unwrap().is_empty());

    let file = t.store().store_dir().to_path(&a).join("f");
    neki_platform::fs::set_readonly(&file, false).unwrap();
    fs::write(&file, "changed").unwrap();
    neki_platform::fs::remove_tree(&t.store().store_dir().to_path(&c)).unwrap();

    let modified: BTreeSet<_> = t.store().verify().unwrap().into_iter().collect();
    assert_eq!(modified, BTreeSet::from([a, c]));
    assert!(t.store().is_valid_path(&b).unwrap());
}

#[test]
fn gc_keeps_what_roots_reach() {
    let t = TestStore::new();
    let s = t.store();
    let lib = s.add_path("lib", &t.source(&[("l", "1")])).unwrap();
    let app = s
        .add_text("app", "uses lib", &BTreeSet::from([lib.clone()]))
        .unwrap();
    let junk = s.add_path("junk", &t.source(&[("j", "2")])).unwrap();
    s.add_root("app", &app).unwrap();
    // Leftover of an interrupted add.
    fs::create_dir(s.store_dir().path().join(".tmp-1-2-3")).unwrap();

    let dry = s
        .collect_garbage(&GcOptions {
            dry_run: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(dry.deleted, std::slice::from_ref(&junk));
    assert!(s.is_valid_path(&junk).unwrap());

    let result = s.collect_garbage(&GcOptions::default()).unwrap();
    assert_eq!(result.deleted, std::slice::from_ref(&junk));
    assert!(result.bytes_freed > 0);
    assert!(!s.is_valid_path(&junk).unwrap());
    assert!(!s.store_dir().to_path(&junk).exists());
    assert!(!s.store_dir().path().join(".tmp-1-2-3").exists());
    assert!(s.is_valid_path(&lib).unwrap() && s.is_valid_path(&app).unwrap());

    s.remove_root("app").unwrap();
    let result = s.collect_garbage(&GcOptions::default()).unwrap();
    assert_eq!(result.deleted.len(), 2);
}

#[test]
fn indirect_roots_last_while_their_link_does() {
    let t = TestStore::new();
    let s = t.store();
    let path = s.add_path("p", &t.source(&[("f", "1")])).unwrap();
    let link = t.dir.path().join("result");
    neki_platform::fs::link_dir(&s.store_dir().to_path(&path), &link).unwrap();
    s.add_indirect_root("result", &path, &link).unwrap();
    assert!(
        s.collect_garbage(&GcOptions::default())
            .unwrap()
            .deleted
            .is_empty()
    );
    neki_platform::fs::remove_tree(&link).unwrap();
    assert_eq!(
        s.collect_garbage(&GcOptions::default()).unwrap().deleted,
        [path]
    );
    assert!(s.roots().unwrap().is_empty());
}

#[test]
fn optimise_links_identical_files() {
    let t = TestStore::new();
    let s = t.store();
    let content = "same content ".repeat(100);
    let a = s
        .add_path("a", &t.source(&[("f", &content), ("other", "a")]))
        .unwrap();
    let b = s
        .add_path("b", &t.source(&[("g", &content), ("other", "b")]))
        .unwrap();
    let saved = s.optimise().unwrap();
    assert_eq!(saved, content.len() as u64);
    let fa = s.store_dir().to_path(&a).join("f");
    let fb = s.store_dir().to_path(&b).join("g");
    let (ia, ib) = (
        neki_platform::fs::file_id(&fa).unwrap(),
        neki_platform::fs::file_id(&fb).unwrap(),
    );
    assert_eq!((ia.device, ia.index), (ib.device, ib.index));
    assert_eq!(s.optimise().unwrap(), 0, "already optimised");
    assert!(s.verify().unwrap().is_empty());
    assert!(fs::metadata(&fa).unwrap().permissions().readonly());

    // Unused .links entries go with GC.
    s.collect_garbage(&GcOptions::default()).unwrap();
    assert_eq!(
        fs::read_dir(s.store_dir().path().join(".links"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn import_nar_checks_the_hash() {
    let t = TestStore::new();
    let src = t.source(&[("f", "data")]);
    let mut nar = Vec::new();
    neki_nar::dump(&src, &mut nar).unwrap();
    let hash = Hash::of_bytes(HashAlgo::Sha256, &nar);
    let path = t
        .store()
        .store_dir()
        .make_path("source", &hash, "imported")
        .unwrap();
    let info = PathInfo {
        path: path.clone(),
        nar_hash: hash,
        nar_size: nar.len() as u64,
        references: BTreeSet::new(),
        deriver: None,
        registration_time: 0,
        signatures: vec!["cache-1:sig".into()],
    };
    let mut bad = info.clone();
    bad.nar_size += 1;
    assert!(matches!(
        t.store().import_nar(&bad, &mut &nar[..]),
        Err(Error::HashMismatch { .. })
    ));
    assert!(!t.store().is_valid_path(&path).unwrap());
    t.store().import_nar(&info, &mut &nar[..]).unwrap();
    let stored = t.store().query_path_info(&path).unwrap().unwrap();
    assert_eq!(stored.signatures, ["cache-1:sig"]);
    assert_eq!(
        fs::read_to_string(t.store().store_dir().to_path(&path).join("f")).unwrap(),
        "data"
    );
}

#[test]
fn filtered_adds_skip_rejected_entries() {
    let t = TestStore::new();
    let src = t.source(&[
        ("keep.txt", "k"),
        (".git/HEAD", "x"),
        ("sub/drop.log", "d"),
        ("sub/ok", "o"),
    ]);
    let path = t
        .store()
        .add_path_filtered("filtered", &src, &mut |p, _| {
            let name = p.file_name().unwrap();
            name != ".git" && name != "drop.log"
        })
        .unwrap();
    let dest = t.store().store_dir().to_path(&path);
    assert!(dest.join("keep.txt").is_file() && dest.join("sub/ok").is_file());
    assert!(!dest.join(".git").exists() && !dest.join("sub/drop.log").exists());
}

#[test]
fn temp_roots_last_while_held() {
    let t = TestStore::new();
    let s = t.store();
    let path = s.add_path("held", &t.source(&[("f", "1")])).unwrap();
    let root = s.add_temp_root(&path).unwrap();
    assert!(
        s.collect_garbage(&GcOptions::default())
            .unwrap()
            .deleted
            .is_empty()
    );
    drop(root);
    assert_eq!(
        s.collect_garbage(&GcOptions::default()).unwrap().deleted,
        std::slice::from_ref(&path)
    );

    // A root file left by a process that ended (nobody holds its lock).
    let other = s.add_path("other", &t.source(&[("f", "2")])).unwrap();
    let dir = s.state_dir().join("temproots");
    fs::write(dir.join(format!("1-0-{other}")), "").unwrap();
    assert_eq!(
        s.collect_garbage(&GcOptions::default()).unwrap().deleted,
        [other]
    );
    assert_eq!(fs::read_dir(dir).unwrap().count(), 0);
}

/// An add waits for another holder of the path's lock (a build of it in
/// another process), so it can't delete what that build is writing.
#[test]
fn adds_wait_for_a_build_of_the_same_path() {
    let t = TestStore::new();
    let store = t.store();
    let path = store
        .store_dir()
        .make_path("text", &Hash::of_bytes(HashAlgo::Sha256, b"hi"), "a.txt")
        .unwrap();
    let held = store.lock_path(&path).unwrap();
    // What the "build" has written so far.
    let location = store.store_dir().to_path(&path);
    fs::write(&location, "partial").unwrap();
    std::thread::scope(|scope| {
        let add = scope.spawn(|| store.add_text("a.txt", "hi", &BTreeSet::new()));
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!add.is_finished());
        assert_eq!(fs::read_to_string(&location).unwrap(), "partial");
        // The build gave up without registering anything.
        fs::remove_file(&location).unwrap();
        drop(held);
        assert_eq!(add.join().unwrap().unwrap(), path);
    });
    assert_eq!(fs::read_to_string(&location).unwrap(), "hi");
}
