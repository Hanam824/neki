use std::collections::BTreeSet;
use std::fs;

use neki_hash::StoreDir;
use std::path::{Path, PathBuf};

use super::*;

/// A machine: its own state (DB), but the same store directory as every
/// other machine, as with the fixed default store dir.
struct Machine {
    store: Option<LocalStore>,
    root: PathBuf,
}

impl Machine {
    fn new(root: &Path, name: &str) -> Self {
        let store = LocalStore::open(StoreDir::new(root.join("store")), root.join(name)).unwrap();
        Self {
            store: Some(store),
            root: root.to_path_buf(),
        }
    }

    fn store(&self) -> &LocalStore {
        self.store.as_ref().unwrap()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        self.store = None;
        // Store entries are read-only; the temp dir can only go once they're writable.
        make_writable(&self.root);
    }
}

fn make_writable(p: &Path) {
    if let Ok(meta) = fs::symlink_metadata(p) {
        if meta.file_type().is_symlink() {
            return;
        }
        let mut perms = meta.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = fs::set_permissions(p, perms);
        if meta.is_dir() {
            for e in fs::read_dir(p).into_iter().flatten().flatten() {
                make_writable(&e.path());
            }
        }
    }
}

/// A builds and uploads, B downloads.
fn machines() -> (tempfile::TempDir, Machine, Machine) {
    let root = tempfile::tempdir().unwrap();
    let a = Machine::new(root.path(), "a");
    let b = Machine::new(root.path(), "b");
    (root, a, b)
}

#[test]
fn keys_sign_and_verify() {
    let key = SecretKey::generate("test-1").unwrap();
    let public = key.public();
    let sig = key.sign("message");
    assert!(sig.starts_with("test-1:"));
    assert!(public.verifies("message", &sig));
    assert!(!public.verifies("other message", &sig));
    let other = SecretKey::generate("test-1").unwrap().public();
    assert!(!other.verifies("message", &sig));
    assert_eq!(SecretKey::parse(&key.to_text()).unwrap().public(), public);
    assert_eq!(PublicKey::parse(&public.to_string()).unwrap(), public);
    assert!(PublicKey::parse("no-colon").is_err());
    assert!(SecretKey::parse("x:AAAA").is_err());
}

#[test]
fn narinfo_round_trips() {
    let store_dir = StoreDir::new("/opt/neki/store");
    let hash = Hash::of_bytes(HashAlgo::Sha256, b"nar");
    let path = store_dir.make_path("source", &hash, "hello").unwrap();
    let dep = store_dir.make_path("source", &hash, "dep").unwrap();
    let narinfo = NarInfo {
        info: neki_store::PathInfo {
            path: path.clone(),
            nar_hash: hash.clone(),
            nar_size: 120,
            references: BTreeSet::from([dep, path.clone()]),
            deriver: None,
            registration_time: 0,
            signatures: vec!["k-1:c2ln".into()],
        },
        url: "nar/x.nar.zst".into(),
        compression: "zstd".into(),
        file_hash: Some(hash),
        file_size: Some(80),
    };
    let text = narinfo.to_text(&store_dir);
    assert!(text.starts_with(&format!("StorePath: /opt/neki/store/{path}\n")));
    let parsed = NarInfo::parse(&text, &store_dir).unwrap();
    assert_eq!(parsed.to_text(&store_dir), text);
    assert!(NarInfo::parse("URL: x\n", &store_dir).is_err());
}

fn add_closure(m: &Machine) -> StorePath {
    let src = m.root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.txt"), "library").unwrap();
    let lib = m.store().add_path("lib", &src).unwrap();
    m.store()
        .add_text("app", "uses the lib", &BTreeSet::from([lib]))
        .unwrap()
}

#[test]
fn push_to_a_file_cache_and_substitute_from_it() {
    let (root, a, b) = machines();
    let key = SecretKey::generate("cache-1").unwrap();
    let app = add_closure(&a);
    let cache_url = format!("file://{}", root.path().join("cache").display());
    let cache = BinaryCache::new(&cache_url, vec![]);
    let uploaded = cache
        .push(a.store(), std::slice::from_ref(&app), &key)
        .unwrap();
    assert_eq!(uploaded.len(), 2);
    assert!(
        cache
            .push(a.store(), std::slice::from_ref(&app), &key)
            .unwrap()
            .is_empty()
    );

    // B doesn't trust the key yet.
    let untrusted = BinaryCache::new(&cache_url, vec![]);
    assert!(matches!(
        untrusted.substitute(b.store(), &app),
        Err(Error::Untrusted(_))
    ));

    let trusted = BinaryCache::new(&cache_url, vec![key.public()]);
    assert!(trusted.substitute(b.store(), &app).unwrap());
    let info = b.store().query_path_info(&app).unwrap().unwrap();
    assert_eq!(info.references.len(), 1);
    assert_eq!(info.signatures.len(), 1);
    assert_eq!(
        fs::read_to_string(b.store().store_dir().to_path(&app)).unwrap(),
        "uses the lib"
    );

    // Something the cache doesn't have.
    let missing = b
        .store()
        .store_dir()
        .make_path("source", &Hash::of_bytes(HashAlgo::Sha256, b"x"), "nope")
        .unwrap();
    assert!(!trusted.substitute(b.store(), &missing).unwrap());

    // A tampered narinfo fails the signature check.
    let narinfo = root
        .path()
        .join("cache")
        .join(format!("{}.narinfo", app.hash_part()));
    let text = fs::read_to_string(&narinfo)
        .unwrap()
        .replace("NarSize: ", "NarSize: 1");
    fs::write(&narinfo, text).unwrap();
    let c = Machine::new(root.path(), "c");
    assert!(matches!(
        trusted.substitute(c.store(), &app),
        Err(Error::Untrusted(_))
    ));
}

#[test]
fn serves_a_store_over_http() {
    let (_root, a, b) = machines();
    let key = SecretKey::generate("serve-1").unwrap();
    let app = add_closure(&a);
    let server = CacheServer::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.addr().unwrap());
    std::thread::scope(|scope| {
        scope.spawn(|| server.run(a.store(), Some(&key), None));
        let cache = BinaryCache::new(&url, vec![key.public()]);
        assert!(cache.substitute(b.store(), &app).unwrap());
        assert!(cache.query(b.store().store_dir(), &app).unwrap().is_some());
        server.stop();
    });
    assert!(b.store().is_valid_path(&app).unwrap());
}

#[test]
fn pushes_over_http_to_a_server_that_trusts_the_key() {
    let (_root, a, b) = machines();
    let key = SecretKey::generate("push-1").unwrap();
    let stranger = SecretKey::generate("stranger-1").unwrap();
    let app = add_closure(&a);
    // B serves its store and accepts uploads signed by `key`.
    let trusted = [key.public()];
    let server = CacheServer::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.addr().unwrap());
    let closed = CacheServer::bind("127.0.0.1:0").unwrap();
    let closed_url = format!("http://{}", closed.addr().unwrap());
    std::thread::scope(|scope| {
        scope.spawn(|| server.run(b.store(), None, Some(&trusted)));
        scope.spawn(|| closed.run(b.store(), None, None));
        let cache = BinaryCache::new(&url, vec![]);

        let err = cache
            .push(a.store(), std::slice::from_ref(&app), &stranger)
            .unwrap_err();
        assert!(err.to_string().contains("rejected"), "{err}");
        assert!(!b.store().is_valid_path(&app).unwrap());

        let err = BinaryCache::new(&closed_url, vec![])
            .push(a.store(), std::slice::from_ref(&app), &key)
            .unwrap_err();
        assert!(err.to_string().contains("--allow-push"), "{err}");

        // The reference first, then the app.
        let uploaded = cache
            .push(a.store(), std::slice::from_ref(&app), &key)
            .unwrap();
        assert_eq!(uploaded.len(), 2);
        assert_eq!(uploaded[1], app);
        assert!(
            cache
                .push(a.store(), std::slice::from_ref(&app), &key)
                .unwrap()
                .is_empty()
        );
        server.stop();
        closed.stop();
    });
    let info = b.store().query_path_info(&app).unwrap().unwrap();
    assert_eq!(info.references.len(), 1);
    assert!(!info.signatures.is_empty());
    assert!(b.store().verify().unwrap().is_empty());
}
