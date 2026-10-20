use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use neki_derivation::Derivation;
use neki_hash::{Hash, HashAlgo, Hasher, StoreDir, StorePath};
use rusqlite::Connection;

use crate::{Error, PathInfo, Result, Store, at, db};

/// A store on the local filesystem: `<store dir>/<hash>-<name>`, with the
/// metadata DB, GC roots and locks under the state directory.
///
/// Paths are added by copying into a temporary entry in the store dir, then
/// renaming it into place, making it read-only and registering it in the DB.
/// A path is valid only once registered, so an entry left by an interrupted
/// add is replaced by the next add. Writers take an exclusive lock on
/// `<state dir>/store.lock`, so concurrent processes don't add the same path
/// twice.
pub struct LocalStore {
    pub(crate) dir: StoreDir,
    state_dir: PathBuf,
    db: Mutex<Connection>,
}

impl LocalStore {
    /// Open the store, creating its directories and DB if needed.
    pub fn open(dir: StoreDir, state_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(dir.path()).map_err(at(dir.path()))?;
        let db_dir = state_dir.join("db");
        fs::create_dir_all(&db_dir).map_err(at(&db_dir))?;
        let db = db::open(&db_dir.join("db.sqlite"))?;
        Ok(Self {
            dir,
            state_dir,
            db: Mutex::new(db),
        })
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Re-hash every valid path; returns the paths whose contents changed or
    /// are missing.
    pub fn verify(&self) -> Result<Vec<StorePath>> {
        let paths = db::valid_paths(&self.db())?;
        let mut modified = Vec::new();
        for path in paths {
            let info = self
                .query_path_info(&path)?
                .ok_or_else(|| Error::NotValid(path.to_string()))?;
            let unchanged = match nar_hash(&self.dir.to_path(&path)) {
                Ok((hash, size)) => hash == info.nar_hash && size == info.nar_size,
                // Missing, or now contains something a NAR can't hold.
                Err(err) if !is_unreadable(&err) => false,
                // Unreadable for another reason: report it rather than guess.
                Err(err) => return Err(err),
            };
            if !unchanged {
                modified.push(path);
            }
        }
        Ok(modified)
    }

    pub(crate) fn db(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock can't leave the connection half-used:
        // every write is a transaction that rolls back when dropped.
        self.db
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Block until this process holds the store's write lock.
    pub(crate) fn lock(&self) -> Result<File> {
        self.lock_file("store.lock", true)
    }

    /// Keep the garbage collector away while the returned guard lives:
    /// builds and other work that creates paths not yet reachable from a GC
    /// root hold it. Any number of processes can hold it at once.
    pub fn gc_guard(&self) -> Result<File> {
        self.lock_file("gc.lock", false)
    }

    pub(crate) fn lock_file(&self, name: &str, exclusive: bool) -> Result<File> {
        let file_path = self.state_dir.join(name);
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&file_path)
            .map_err(at(&file_path))?;
        if exclusive {
            file.lock()
        } else {
            file.lock_shared()
        }
        .map_err(at(&file_path))?;
        Ok(file)
    }

    /// A fresh name in the store dir for an entry being added. It can't be
    /// mistaken for a store path, which never starts with `.`.
    pub(crate) fn temp_path(&self) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        self.dir.path().join(format!(
            ".tmp-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// Move the finished entry `tmp` to its store location, make it
    /// read-only and register it. The caller holds the write lock and has
    /// checked that the path isn't valid yet.
    pub(crate) fn install(&self, tmp: &Path, info: &PathInfo) -> Result<()> {
        let dest = self.dir.to_path(&info.path);
        // Left over from an add that was interrupted before registering.
        neki_platform::fs::remove_tree(&dest).map_err(at(&dest))?;
        fs::rename(tmp, &dest).map_err(at(&dest))?;
        make_readonly(&dest)?;
        let mut db = self.db();
        let tx = db.transaction()?;
        db::register(&tx, std::slice::from_ref(info))?;
        tx.commit()?;
        Ok(())
    }

    /// Run `add` with the write lock held unless `path` is already valid,
    /// removing the temporary entry it was given if it fails.
    fn add_with(
        &self,
        path: StorePath,
        add: impl FnOnce(&Path) -> Result<PathInfo>,
    ) -> Result<StorePath> {
        if self.is_valid_path(&path)? {
            return Ok(path);
        }
        let _lock = self.lock()?;
        if self.is_valid_path(&path)? {
            return Ok(path);
        }
        let tmp = self.temp_path();
        let result = add(&tmp).and_then(|info| self.install(&tmp, &info));
        if result.is_err() {
            // Best effort: the add already failed, and this error would hide why.
            let _ = neki_platform::fs::remove_tree(&tmp);
        }
        result.map(|()| path)
    }
}

/// Operations used by builders, fetchers and profiles.
impl LocalStore {
    /// Add the file (`recursive` false) or tree at `src` as the fixed-output
    /// path for `hash`, after checking that `src` really has that hash.
    pub fn add_fixed(
        &self,
        name: &str,
        src: &Path,
        hash: &Hash,
        recursive: bool,
    ) -> Result<StorePath> {
        let got = if recursive {
            nar_hash_with(src, hash.algo())?.0
        } else {
            file_hash(src, hash.algo())?
        };
        if got != *hash {
            return Err(Error::HashMismatch {
                what: src.display().to_string(),
                expected: hash.to_sri(),
                got: got.to_sri(),
            });
        }
        let path = neki_derivation::fixed_output_path(&self.dir, name, hash, recursive)?;
        let (nar, size) = nar_hash(src)?;
        self.add_with(path.clone(), |tmp| {
            if copy(src, tmp)? != (nar.clone(), size) {
                return Err(Error::Changed(src.to_path_buf()));
            }
            Ok(new_info(path, nar, size, BTreeSet::new()))
        })
    }

    /// Like [`Store::add_path`], but only entries below `src` that `filter`
    /// accepts (given each entry's path and kind) are copied; a rejected
    /// directory is skipped with everything in it. The copy is hashed, so a
    /// source changing meanwhile can't make the path and contents disagree.
    pub fn add_path_filtered(
        &self,
        name: &str,
        src: &Path,
        filter: &mut dyn FnMut(&Path, EntryKind) -> bool,
    ) -> Result<StorePath> {
        let tmp = self.temp_entry();
        copy_filtered(src, tmp.path(), filter, true)?;
        let (hash, size) = nar_hash(tmp.path())?;
        let path = self.dir.make_path("source", &hash, name)?;
        if self.is_valid_path(&path)? {
            return Ok(path);
        }
        let _lock = self.lock()?;
        if !self.is_valid_path(&path)? {
            self.install(
                tmp.path(),
                &new_info(path.clone(), hash, size, BTreeSet::new()),
            )?;
        }
        Ok(path)
    }

    /// Store `drv` (finalized) as a `.drv` file.
    pub fn add_derivation(&self, drv: &Derivation) -> Result<StorePath> {
        let path = self.add_text(
            &format!("{}.drv", drv.name()?),
            &drv.to_aterm(&self.dir)?,
            &drv.references(),
        )?;
        debug_assert_eq!(path, drv.drv_path(&self.dir)?);
        Ok(path)
    }

    /// The valid path whose hash part is `hash_part`, if any.
    pub fn query_path_from_hash_part(&self, hash_part: &str) -> Result<Option<StorePath>> {
        use rusqlite::OptionalExtension;
        let found: Option<String> = self
            .db()
            .query_row(
                "SELECT path FROM ValidPaths WHERE path >= ?1 AND path < ?2 LIMIT 1",
                [format!("{hash_part}-"), format!("{hash_part}.")],
                |row| row.get(0),
            )
            .optional()?;
        found.map(|p| Ok(StorePath::parse(&p)?)).transpose()
    }

    /// Every valid path, sorted.
    pub fn valid_paths(&self) -> Result<Vec<StorePath>> {
        db::valid_paths(&self.db())
    }

    pub fn read_derivation(&self, path: &StorePath) -> Result<Derivation> {
        if !self.is_valid_path(path)? {
            return Err(Error::NotValid(path.to_string()));
        }
        let file = self.dir.to_path(path);
        let text = fs::read_to_string(&file).map_err(at(&file))?;
        Ok(Derivation::parse_aterm(&text, &self.dir)?)
    }

    /// An exclusive lock on work that creates `path` (a build output), held
    /// until the returned file is dropped. Other processes building the same
    /// path wait for it.
    pub fn lock_path(&self, path: &StorePath) -> Result<File> {
        let dir = self.state_dir.join("locks");
        fs::create_dir_all(&dir).map_err(at(&dir))?;
        let file_path = dir.join(format!("{}.lock", path.hash_part()));
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&file_path)
            .map_err(at(&file_path))?;
        file.lock().map_err(at(&file_path))?;
        Ok(file)
    }

    /// Delete whatever is at the location of `path`, which must not be
    /// valid: leftovers of an interrupted build, before building it again.
    pub fn clear_invalid(&self, path: &StorePath) -> Result<()> {
        if self.is_valid_path(path)? {
            return Err(Error::Corrupt(format!(
                "{path} is valid; refusing to delete it"
            )));
        }
        let dest = self.dir.to_path(path);
        neki_platform::fs::remove_tree(&dest).map_err(at(&dest))
    }

    /// Register the trees a builder left at the locations of `paths` (the
    /// outputs of one derivation), together. Their references are the paths
    /// among `candidates` and `paths` that they mention; pass `scan: false`
    /// for outputs that may not have any (fixed outputs).
    pub fn register_built(
        &self,
        paths: &[StorePath],
        deriver: Option<&StorePath>,
        candidates: &BTreeSet<StorePath>,
        scan: bool,
    ) -> Result<Vec<PathInfo>> {
        let mut infos = Vec::new();
        for path in paths {
            let dest = self.dir.to_path(path);
            let references = if scan {
                crate::scan_references(&dest, candidates.iter().chain(paths))?
            } else {
                BTreeSet::new()
            };
            let (hash, size) = nar_hash(&dest)?;
            let mut info = new_info(path.clone(), hash, size, references);
            info.deriver = deriver.cloned();
            make_readonly(&dest)?;
            infos.push(info);
        }
        let _lock = self.lock()?;
        let mut db = self.db();
        let tx = db.transaction()?;
        db::register(&tx, &infos)?;
        tx.commit()?;
        Ok(infos)
    }

    /// A fresh, not yet created location in the store dir to assemble a
    /// tree in (on the same filesystem, so it can be renamed into place).
    /// Whatever is left there is deleted when the entry is dropped.
    pub fn temp_entry(&self) -> TempEntry {
        TempEntry(self.temp_path())
    }

    /// Add the tree assembled in `tmp` as a content-addressed path named
    /// `name` whose references are the `candidates` it mentions. The tree is
    /// moved, not copied, so hard links in it stay hard links.
    pub fn add_tree(
        &self,
        name: &str,
        tmp: TempEntry,
        candidates: &BTreeSet<StorePath>,
    ) -> Result<StorePath> {
        let references = crate::scan_references(&tmp.0, candidates)?;
        let (hash, size) = nar_hash(&tmp.0)?;
        let mut kind = String::from("source");
        for reference in &references {
            kind.push(':');
            kind.push_str(&self.dir.to_path(reference).display().to_string());
        }
        let path = self.dir.make_path(&kind, &hash, name)?;
        if self.is_valid_path(&path)? {
            return Ok(path);
        }
        let _lock = self.lock()?;
        if !self.is_valid_path(&path)? {
            self.install(&tmp.0, &new_info(path.clone(), hash, size, references))?;
        }
        Ok(path)
    }
}

/// What a directory entry is, for [`LocalStore::add_path_filtered`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Regular,
    Directory,
    Symlink,
}

impl EntryKind {
    /// `regular`, `directory` or `symlink`, as Nix's path filters see them.
    pub fn name(self) -> &'static str {
        match self {
            EntryKind::Regular => "regular",
            EntryKind::Directory => "directory",
            EntryKind::Symlink => "symlink",
        }
    }
}

fn copy_filtered(
    src: &Path,
    dest: &Path,
    filter: &mut dyn FnMut(&Path, EntryKind) -> bool,
    root: bool,
) -> Result<()> {
    let meta = fs::symlink_metadata(src).map_err(at(src))?;
    let kind = if meta.file_type().is_symlink() {
        EntryKind::Symlink
    } else if meta.is_dir() {
        EntryKind::Directory
    } else {
        EntryKind::Regular
    };
    if !root && !filter(src, kind) {
        return Ok(());
    }
    match kind {
        EntryKind::Symlink => {
            let target = fs::read_link(src).map_err(at(src))?;
            neki_platform::fs::symlink(&target, dest).map_err(at(dest))?;
        }
        EntryKind::Directory => {
            fs::create_dir(dest).map_err(at(dest))?;
            for entry in fs::read_dir(src).map_err(at(src))? {
                let entry = entry.map_err(at(src))?;
                copy_filtered(&entry.path(), &dest.join(entry.file_name()), filter, false)?;
            }
        }
        EntryKind::Regular => {
            fs::copy(src, dest).map_err(at(src))?;
            // `fs::copy` keeps permissions; a read-only source must not make
            // a read-only copy that can't be renamed or replaced.
            neki_platform::fs::set_readonly(dest, false).map_err(at(dest))?;
        }
    }
    Ok(())
}

/// A temporary location in the store dir; see [`LocalStore::temp_entry`].
#[derive(Debug)]
pub struct TempEntry(PathBuf);

impl TempEntry {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempEntry {
    fn drop(&mut self) {
        let _ = neki_platform::fs::remove_tree(&self.0);
    }
}

impl Store for LocalStore {
    fn store_dir(&self) -> &StoreDir {
        &self.dir
    }

    fn query_path_info(&self, path: &StorePath) -> Result<Option<PathInfo>> {
        db::query(&self.db(), path)
    }

    fn add_path(&self, name: &str, src: &Path) -> Result<StorePath> {
        let (hash, size) = nar_hash(src)?;
        let path = self.dir.make_path("source", &hash, name)?;
        self.add_with(path.clone(), |tmp| {
            if copy(src, tmp)? != (hash.clone(), size) {
                return Err(Error::Changed(src.to_path_buf()));
            }
            Ok(new_info(path, hash, size, BTreeSet::new()))
        })
    }

    fn add_text(
        &self,
        name: &str,
        text: &str,
        references: &BTreeSet<StorePath>,
    ) -> Result<StorePath> {
        // Like Nix: the references are part of the path's fingerprint.
        let mut kind = String::from("text");
        for reference in references {
            if !self.is_valid_path(reference)? {
                return Err(Error::NotValid(reference.to_string()));
            }
            kind.push(':');
            kind.push_str(&self.dir.to_path(reference).display().to_string());
        }
        let path = self.dir.make_path(
            &kind,
            &Hash::of_bytes(HashAlgo::Sha256, text.as_bytes()),
            name,
        )?;
        self.add_with(path.clone(), |tmp| {
            fs::write(tmp, text).map_err(at(tmp))?;
            let (hash, size) = nar_hash(tmp)?;
            Ok(new_info(path, hash, size, references.clone()))
        })
    }

    fn closure(&self, paths: &[StorePath]) -> Result<BTreeSet<StorePath>> {
        let mut closure = BTreeSet::new();
        let mut todo = paths.to_vec();
        while let Some(path) = todo.pop() {
            if closure.contains(&path) {
                continue;
            }
            let info = self
                .query_path_info(&path)?
                .ok_or_else(|| Error::NotValid(path.to_string()))?;
            todo.extend(
                info.references
                    .into_iter()
                    .filter(|reference| !closure.contains(reference)),
            );
            closure.insert(path);
        }
        Ok(closure)
    }
}

pub(crate) fn new_info(
    path: StorePath,
    nar_hash: Hash,
    nar_size: u64,
    references: BTreeSet<StorePath>,
) -> PathInfo {
    let registration_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    PathInfo {
        path,
        nar_hash,
        nar_size,
        references,
        deriver: None,
        registration_time,
        signatures: Vec::new(),
    }
}

/// Whether `err` is an I/O error other than a missing file.
fn is_unreadable(err: &Error) -> bool {
    matches!(err, Error::Nar(neki_nar::Error::Io { source, .. }) if source.kind() != io::ErrorKind::NotFound)
}

/// SHA-256 and size of the NAR serialization of `path`.
pub fn nar_hash(path: &Path) -> Result<(Hash, u64)> {
    nar_hash_with(path, HashAlgo::Sha256)
}

fn nar_hash_with(path: &Path, algo: HashAlgo) -> Result<(Hash, u64)> {
    let mut hasher = Hasher::new(algo);
    neki_nar::dump(path, &mut BufWriter::new(&mut hasher))?;
    let size = hasher.bytes_hashed();
    Ok((hasher.finish(), size))
}

/// Hash of the contents of the regular file at `path`.
pub fn file_hash(path: &Path, algo: HashAlgo) -> Result<Hash> {
    if !fs::symlink_metadata(path).map_err(at(path))?.is_file() {
        return Err(Error::File {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"),
        });
    }
    let mut hasher = Hasher::new(algo);
    let mut file = File::open(path).map_err(at(path))?;
    io::copy(&mut file, &mut hasher).map_err(at(path))?;
    Ok(hasher.finish())
}

/// Copy the tree at `src` to the new path `dest` (writable, with the
/// executable bits a NAR keeps), checking names like an unpacked NAR.
pub fn copy_tree(src: &Path, dest: &Path) -> Result<()> {
    copy(src, dest).map(|_| ())
}

/// Copy `src` to the new path `dest` by streaming its NAR from a dumping
/// thread into `restore`, so the copy is checked exactly like a downloaded
/// archive would be. Returns the hash and size of the NAR that was copied.
fn copy(src: &Path, dest: &Path) -> Result<(Hash, u64)> {
    let (reader, writer) = io::pipe()?;
    std::thread::scope(|scope| {
        let dumper = scope.spawn(move || -> neki_nar::Result<Hasher> {
            let mut out = BufWriter::new(Tee {
                out: writer,
                hasher: Hasher::new(HashAlgo::Sha256),
            });
            neki_nar::dump(src, &mut out)?;
            // Dropping the writer here ends the archive for the reader.
            let tee = out.into_inner().map_err(|err| err.into_error())?;
            Ok(tee.hasher)
        });
        let restored = neki_nar::restore(&mut BufReader::new(reader), dest);
        // The reader is dropped by now, so a dumper still writing gets an error.
        let dumped = dumper
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        match (dumped, restored) {
            (Ok(hasher), Ok(())) => {
                let size = hasher.bytes_hashed();
                Ok((hasher.finish(), size))
            }
            // The dumper stopped because the restore did.
            (Err(neki_nar::Error::Stream(_)), Err(err)) => Err(err.into()),
            // A failed dump makes the restore see a truncated archive; the
            // dump's error says why.
            (Err(err), _) | (Ok(_), Err(err)) => Err(err.into()),
        }
    })
}

/// Writes to `out` and hashes what was written.
struct Tee {
    out: io::PipeWriter,
    hasher: Hasher,
}

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.out.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Make `path` and everything below it read-only, children before their
/// directory. Symlinks have no permissions of their own and are skipped.
pub(crate) fn make_readonly(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(at(path))?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        for entry in fs::read_dir(path).map_err(at(path))? {
            make_readonly(&entry.map_err(at(path))?.path())?;
        }
    }
    neki_platform::fs::set_readonly(path, true).map_err(at(path))
}
