//! GC roots, garbage collection, store optimisation and importing NARs.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use neki_hash::{HashAlgo, Hasher, StorePath, base32};
use rusqlite::OptionalExtension;

use crate::local::new_info;
use crate::{Error, LocalStore, PathInfo, Result, Store, at, db};

#[derive(Debug, Clone, Default)]
pub struct GcOptions {
    /// Only report what would be deleted.
    pub dry_run: bool,
    /// Also delete profile generations older than this (handled by
    /// `neki-core`, which removes those roots before collecting).
    pub delete_older_than: Option<Duration>,
}

#[derive(Debug, Clone, Default)]
pub struct GcResult {
    pub deleted: Vec<StorePath>,
    pub bytes_freed: u64,
}

/// A GC root: `path` and everything it refers to are kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub name: String,
    pub path: StorePath,
    /// For an indirect root: a link (such as a project's `result`) that
    /// must still point at `path` for the root to count.
    pub link: Option<PathBuf>,
}

/// Name of the directory in the store that holds one hard link per
/// distinct file content (see [`LocalStore::optimise`]).
const LINKS: &str = ".links";

impl LocalStore {
    /// Make `path` a GC root named `name`, replacing any root of that name.
    pub fn add_root(&self, name: &str, path: &StorePath) -> Result<()> {
        self.set_root(name, path, None)
    }

    /// A root that lasts only while `link` points at `path`.
    pub fn add_indirect_root(&self, name: &str, path: &StorePath, link: &Path) -> Result<()> {
        self.set_root(name, path, Some(link))
    }

    fn set_root(&self, name: &str, path: &StorePath, link: Option<&Path>) -> Result<()> {
        if !self.is_valid_path(path)? {
            return Err(Error::NotValid(path.to_string()));
        }
        self.db().execute(
            "INSERT OR REPLACE INTO Roots (name, path, link) VALUES (?1, ?2, ?3)",
            (
                name,
                path.to_string(),
                link.map(|l| l.display().to_string()),
            ),
        )?;
        Ok(())
    }

    pub fn remove_root(&self, name: &str) -> Result<()> {
        self.db()
            .execute("DELETE FROM Roots WHERE name = ?1", [name])?;
        Ok(())
    }

    pub fn root(&self, name: &str) -> Result<Option<Root>> {
        Ok(self.roots()?.into_iter().find(|r| r.name == name))
    }

    pub fn roots(&self) -> Result<Vec<Root>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT name, path, link FROM Roots ORDER BY name")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (name, path, link) = row?;
            Ok(Root {
                name,
                path: StorePath::parse(&path)?,
                link: link.map(PathBuf::from),
            })
        })
        .collect()
    }

    /// Whether an indirect root's link still points at its path.
    fn root_alive(&self, root: &Root) -> bool {
        match &root.link {
            None => true,
            Some(link) => {
                let target = fs::canonicalize(link);
                let expected = fs::canonicalize(self.dir.to_path(&root.path));
                matches!((target, expected), (Ok(t), Ok(e)) if t == e)
            }
        }
    }

    /// Every valid path not reachable from a GC root.
    pub fn dead_paths(&self) -> Result<BTreeSet<StorePath>> {
        let roots: Vec<StorePath> = self
            .roots()?
            .into_iter()
            .filter(|r| self.root_alive(r))
            .map(|r| r.path)
            .filter(|p| self.is_valid_path(p).unwrap_or(false))
            .collect();
        let live = self.closure(&roots)?;
        Ok(db::valid_paths(&self.db())?
            .into_iter()
            .filter(|p| !live.contains(p))
            .collect())
    }

    /// Delete every path not reachable from a GC root, then leftovers of
    /// interrupted adds and unused `.links` entries. Waits for running
    /// builds (which hold [`LocalStore::gc_guard`]).
    pub fn collect_garbage(&self, opts: &GcOptions) -> Result<GcResult> {
        let _gc = self.lock_file("gc.lock", true)?;
        let _lock = self.lock()?;
        let mut result = GcResult::default();
        if !opts.dry_run {
            for root in self.roots()? {
                if !self.root_alive(&root) {
                    self.remove_root(&root.name)?;
                }
            }
        }
        let dead = self.dead_paths()?;
        for path in &dead {
            if let Some(info) = self.query_path_info(path)? {
                result.bytes_freed += info.nar_size;
            }
        }
        result.deleted = dead.iter().cloned().collect();
        if opts.dry_run {
            return Ok(result);
        }
        {
            let mut db = self.db();
            let tx = db.transaction()?;
            for path in &dead {
                let id: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM ValidPaths WHERE path = ?1",
                        [path.to_string()],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(id) = id {
                    tx.execute("DELETE FROM Refs WHERE referrer = ?1", [id])?;
                }
            }
            for path in &dead {
                tx.execute("DELETE FROM ValidPaths WHERE path = ?1", [path.to_string()])?;
            }
            tx.commit()?;
        }
        for path in &dead {
            let location = self.dir.to_path(path);
            neki_platform::fs::remove_tree(&location).map_err(at(&location))?;
        }
        // Whatever else is in the store dir isn't valid: leftovers of
        // interrupted adds and builds (no build is running now).
        for entry in fs::read_dir(self.dir.path()).map_err(at(self.dir.path()))? {
            let entry = entry.map_err(at(self.dir.path()))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == LINKS {
                continue;
            }
            let valid = match StorePath::parse(&name) {
                Ok(path) => self.is_valid_path(&path)?,
                Err(_) => false,
            };
            if !valid {
                neki_platform::fs::remove_tree(&entry.path()).map_err(at(&entry.path()))?;
            }
        }
        let links = self.dir.path().join(LINKS);
        if links.is_dir() {
            for entry in fs::read_dir(&links).map_err(at(&links))? {
                let path = entry.map_err(at(&links))?.path();
                if neki_platform::fs::file_id(&path).is_ok_and(|id| id.links <= 1) {
                    neki_platform::fs::remove_tree(&path).map_err(at(&path))?;
                }
            }
        }
        Ok(result)
    }

    /// Replace identical files in the store with hard links to one copy
    /// (kept under `.links`, named by content hash and executable bit).
    /// Returns the bytes saved.
    pub fn optimise(&self) -> Result<u64> {
        let _lock = self.lock()?;
        let links = self.dir.path().join(LINKS);
        fs::create_dir_all(&links).map_err(at(&links))?;
        let mut saved = 0;
        for path in db::valid_paths(&self.db())? {
            let location = self.dir.to_path(&path);
            for file in regular_files(&location)? {
                saved += self.optimise_file(&file, &links)?;
            }
        }
        Ok(saved)
    }

    fn optimise_file(&self, file: &Path, links: &Path) -> Result<u64> {
        let meta = fs::symlink_metadata(file).map_err(at(file))?;
        let mut hasher = Hasher::new(HashAlgo::Sha256);
        io::copy(&mut fs::File::open(file).map_err(at(file))?, &mut hasher).map_err(at(file))?;
        let mut key = base32::encode(hasher.finish().digest());
        if neki_platform::fs::is_executable(&meta) {
            key.push_str("-x");
        }
        let link = links.join(key);
        if fs::symlink_metadata(&link).is_err() {
            // The first copy of this content; a failure (e.g. too many links)
            // just means it isn't shared.
            let _ = neki_platform::fs::hard_link(file, &link);
            return Ok(0);
        }
        let (a, b) = (
            neki_platform::fs::file_id(file).map_err(at(file))?,
            neki_platform::fs::file_id(&link).map_err(at(&link))?,
        );
        if (a.device, a.index) == (b.device, b.index) {
            return Ok(0);
        }
        let dir = file.parent().expect("store files have a parent");
        let tmp = dir.join(".tmp-optimise");
        let replace = || -> io::Result<()> {
            neki_platform::fs::set_readonly(dir, false)?;
            let _ = fs::remove_file(&tmp);
            neki_platform::fs::hard_link(&link, &tmp)?;
            // Windows can't replace a read-only file.
            neki_platform::fs::set_readonly(file, false)?;
            fs::rename(&tmp, file)
        };
        let result = replace();
        let _ = fs::remove_file(&tmp);
        neki_platform::fs::set_readonly(dir, true).map_err(at(dir))?;
        result.map_err(at(file))?;
        Ok(meta.len())
    }

    /// Unpack a NAR read from `nar` as the path described by `info` (from a
    /// binary cache), checking its hash and size. The references in `info`
    /// must already be valid.
    pub fn import_nar(&self, info: &PathInfo, nar: &mut dyn Read) -> Result<()> {
        if self.is_valid_path(&info.path)? {
            return Ok(());
        }
        for reference in &info.references {
            if *reference != info.path && !self.is_valid_path(reference)? {
                return Err(Error::NotValid(reference.to_string()));
            }
        }
        let tmp = self.temp_entry();
        let mut reader = HashingReader {
            inner: nar,
            hasher: Hasher::new(info.nar_hash.algo()),
        };
        neki_nar::restore(&mut io::BufReader::new(&mut reader), tmp.path())?;
        let size = reader.hasher.bytes_hashed();
        let got = reader.hasher.finish();
        if got != info.nar_hash || size != info.nar_size {
            return Err(Error::HashMismatch {
                what: info.path.to_string(),
                expected: format!("{} ({} bytes)", info.nar_hash.to_sri(), info.nar_size),
                got: format!("{} ({size} bytes)", got.to_sri()),
            });
        }
        let _lock = self.lock()?;
        if !self.is_valid_path(&info.path)? {
            let mut registered = new_info(info.path.clone(), got, size, info.references.clone());
            registered.deriver = info.deriver.clone();
            registered.signatures = info.signatures.clone();
            self.install(tmp.path(), &registered)?;
        }
        Ok(())
    }
}

struct HashingReader<'a> {
    inner: &'a mut dyn Read,
    hasher: Hasher,
}

impl Read for HashingReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

/// Every regular file in the tree at `path`.
fn regular_files(path: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut todo = vec![path.to_path_buf()];
    while let Some(p) = todo.pop() {
        let meta = fs::symlink_metadata(&p).map_err(at(&p))?;
        if meta.is_dir() {
            for entry in fs::read_dir(&p).map_err(at(&p))? {
                todo.push(entry.map_err(at(&p))?.path());
            }
        } else if meta.is_file() {
            out.push(p);
        }
    }
    Ok(out)
}
