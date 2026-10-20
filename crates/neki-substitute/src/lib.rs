//! Binary caches: download (substitute) and upload prebuilt store paths.
//!
//! A cache is a directory or web server with Nix's layout:
//!
//! - `nix-cache-info`: `StoreDir: <store dir>`; paths are only usable by a
//!   store with the same directory;
//! - `<hash part>.narinfo`: metadata of one path (see [`NarInfo`]);
//! - `nar/<file hash>.nar.zst` (or `.nar`): its NAR, compressed.
//!
//! Every narinfo is signed with an ed25519 key; a path is only used if it
//! has a valid signature from a trusted key.

mod keys;
mod narinfo;
mod serve;

pub use keys::{PublicKey, SecretKey, fingerprint};
pub use narinfo::NarInfo;
pub use serve::CacheServer;

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use neki_hash::{Hash, HashAlgo, Hasher, StoreDir, StorePath};
use neki_store::{LocalStore, Store};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Store(#[from] neki_store::Error),
    #[error(transparent)]
    Nar(#[from] neki_nar::Error),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("cache request failed: {0}")]
    Http(String),
    #[error("invalid narinfo: {0}")]
    InvalidNarInfo(String),
    #[error("invalid key: {0}")]
    InvalidKey(String),
    #[error("no trusted signature for {0}")]
    Untrusted(String),
    #[error("cache {url} is for store {theirs}, not {ours}")]
    WrongStore {
        url: String,
        theirs: String,
        ours: String,
    },
    #[error("upload rejected: {0}")]
    Rejected(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

fn at(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// A binary cache at a `file://`, `http://` or `https://` URL.
pub struct BinaryCache {
    pub url: String,
    pub trusted_keys: Vec<PublicKey>,
}

impl BinaryCache {
    pub fn new(url: impl Into<String>, trusted_keys: Vec<PublicKey>) -> Self {
        Self {
            url: url.into().trim_end_matches('/').to_string(),
            trusted_keys,
        }
    }

    /// The local directory of a `file://` cache.
    fn dir(&self) -> Option<PathBuf> {
        let rest = self.url.strip_prefix("file://")?;
        let b = rest.as_bytes();
        let rest = if b.len() >= 3 && b[0] == b'/' && b[2] == b':' {
            &rest[1..]
        } else {
            rest
        };
        Some(PathBuf::from(rest))
    }

    /// `file` relative to the cache, or `None` if it doesn't exist.
    fn get(&self, file: &str) -> Result<Option<Box<dyn Read>>> {
        if let Some(dir) = self.dir() {
            let path = dir.join(file);
            return match fs::File::open(&path) {
                Ok(f) => Ok(Some(Box::new(io::BufReader::new(f)))),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(at(&path)(e)),
            };
        }
        let url = format!("{}/{file}", self.url);
        match ureq::get(&url)
            .header("User-Agent", concat!("neki/", env!("CARGO_PKG_VERSION")))
            .call()
        {
            Ok(response) => Ok(Some(Box::new(response.into_body().into_reader()))),
            Err(ureq::Error::StatusCode(404 | 403)) => Ok(None),
            Err(e) => Err(Error::Http(format!("{url}: {e}"))),
        }
    }

    fn get_text(&self, file: &str) -> Result<Option<String>> {
        let Some(reader) = self.get(file)? else {
            return Ok(None);
        };
        let mut text = String::new();
        reader
            .take(1 << 20)
            .read_to_string(&mut text)
            .map_err(|e| Error::Http(format!("{}/{file}: {e}", self.url)))?;
        Ok(Some(text))
    }

    /// Check that the cache is for `store_dir`.
    fn check_store_dir(&self, store_dir: &StoreDir) -> Result<()> {
        let ours = store_dir.path().display().to_string();
        let info = self.get_text("nix-cache-info")?.unwrap_or_default();
        let theirs = info
            .lines()
            .find_map(|l| l.strip_prefix("StoreDir: "))
            .unwrap_or("")
            .trim()
            .to_string();
        if theirs != ours {
            return Err(Error::WrongStore {
                url: self.url.clone(),
                theirs,
                ours,
            });
        }
        Ok(())
    }

    /// The narinfo of `path`, if the cache has it.
    pub fn query(&self, store_dir: &StoreDir, path: &StorePath) -> Result<Option<NarInfo>> {
        let Some(text) = self.get_text(&format!("{}.narinfo", path.hash_part()))? else {
            return Ok(None);
        };
        let info = NarInfo::parse(&text, store_dir)?;
        if info.info.path != *path {
            return Err(Error::InvalidNarInfo(format!(
                "asked for {path}, got {}",
                info.info.path
            )));
        }
        Ok(Some(info))
    }

    /// Download `path` and everything it refers to into `store`, after
    /// checking their signatures. Returns false if the cache doesn't have
    /// all of them.
    pub fn substitute(&self, store: &LocalStore, path: &StorePath) -> Result<bool> {
        self.check_store_dir(store.store_dir())?;
        let mut done = BTreeSet::new();
        self.substitute_one(store, path, &mut done)
    }

    fn substitute_one(
        &self,
        store: &LocalStore,
        path: &StorePath,
        done: &mut BTreeSet<StorePath>,
    ) -> Result<bool> {
        if done.contains(path) || store.is_valid_path(path)? {
            return Ok(true);
        }
        let Some(narinfo) = self.query(store.store_dir(), path)? else {
            return Ok(false);
        };
        if !narinfo.is_trusted(store.store_dir(), &self.trusted_keys) {
            return Err(Error::Untrusted(path.to_string()));
        }
        for reference in &narinfo.info.references {
            if reference != path && !self.substitute_one(store, reference, done)? {
                return Ok(false);
            }
        }
        let Some(mut nar) = self.nar(&narinfo)? else {
            return Ok(false);
        };
        store.import_nar(&narinfo.info, &mut nar)?;
        done.insert(path.clone());
        Ok(true)
    }

    /// The (decompressed) NAR that `narinfo` describes, if the cache has it.
    fn nar(&self, narinfo: &NarInfo) -> Result<Option<Box<dyn Read>>> {
        let Some(reader) = self.get(&narinfo.url)? else {
            return Ok(None);
        };
        Ok(Some(match narinfo.compression.as_str() {
            "none" | "" => reader,
            "zstd" => Box::new(
                zstd::stream::read::Decoder::new(reader)
                    .map_err(|e| Error::Http(format!("{}: {e}", narinfo.url)))?,
            ),
            other => {
                return Err(Error::InvalidNarInfo(format!(
                    "unsupported compression {other:?}"
                )));
            }
        }))
    }

    /// Unpack the cache's copy of the valid path `path` into the new
    /// location `dest` (for repairing it), after checking its signature.
    /// Returns false if the cache doesn't have it.
    pub fn fetch_into(&self, store: &LocalStore, path: &StorePath, dest: &Path) -> Result<bool> {
        self.check_store_dir(store.store_dir())?;
        let Some(narinfo) = self.query(store.store_dir(), path)? else {
            return Ok(false);
        };
        if !narinfo.is_trusted(store.store_dir(), &self.trusted_keys) {
            return Err(Error::Untrusted(path.to_string()));
        }
        let Some(mut nar) = self.nar(&narinfo)? else {
            return Ok(false);
        };
        neki_nar::restore(&mut io::BufReader::new(&mut nar), dest)?;
        Ok(true)
    }

    /// Upload the closures of `paths`, signed with `key`, skipping paths the
    /// cache already has. A `file://` cache is written directly; an HTTP
    /// cache must be a `neki store serve --allow-push` that trusts `key`.
    /// Returns the paths uploaded.
    pub fn push(
        &self,
        store: &LocalStore,
        paths: &[StorePath],
        key: &SecretKey,
    ) -> Result<Vec<StorePath>> {
        match self.dir() {
            Some(dir) => self.push_to_dir(store, &dir, paths, key),
            None => self.push_over_http(store, paths, key),
        }
    }

    fn push_to_dir(
        &self,
        store: &LocalStore,
        dir: &Path,
        paths: &[StorePath],
        key: &SecretKey,
    ) -> Result<Vec<StorePath>> {
        let nar_dir = dir.join("nar");
        fs::create_dir_all(&nar_dir).map_err(at(&nar_dir))?;
        let info_file = dir.join("nix-cache-info");
        let store_dir = store.store_dir();
        if info_file.exists() {
            self.check_store_dir(store_dir)?;
        } else {
            write_atomic(
                &info_file,
                format!(
                    "StoreDir: {}\nWantMassQuery: 1\nPriority: 40\n",
                    store_dir.path().display()
                )
                .as_bytes(),
            )?;
        }
        let mut uploaded = Vec::new();
        for path in store.closure(paths)? {
            let narinfo_file = dir.join(format!("{}.narinfo", path.hash_part()));
            if narinfo_file.exists() {
                continue;
            }
            let tmp = nar_dir.join(format!(".tmp-{}", path.hash_part()));
            let narinfo = compressed_nar(store, &path, &tmp, key)?;
            let dest = dir.join(&narinfo.url);
            fs::rename(&tmp, &dest).map_err(at(&dest))?;
            write_atomic(&narinfo_file, narinfo.to_text(store_dir).as_bytes())?;
            uploaded.push(path);
        }
        Ok(uploaded)
    }

    fn push_over_http(
        &self,
        store: &LocalStore,
        paths: &[StorePath],
        key: &SecretKey,
    ) -> Result<Vec<StorePath>> {
        self.check_store_dir(store.store_dir())?;
        let tmp_dir = store.state_dir().join("tmp");
        fs::create_dir_all(&tmp_dir).map_err(at(&tmp_dir))?;
        let mut uploaded = Vec::new();
        // References first: the server only accepts a path whose references
        // it already has.
        for path in references_first(store, paths)? {
            if self.query(store.store_dir(), &path)?.is_some() {
                continue;
            }
            let tmp = tmp_dir.join(format!("upload-{}", path.hash_part()));
            let narinfo = compressed_nar(store, &path, &tmp, key)?;
            let file = fs::File::open(&tmp).map_err(at(&tmp))?;
            let sent = self
                .put(&format!("nar/{}.nar.zst", path.hash_part()), file)
                .and_then(|()| {
                    self.put(
                        &format!("{}.narinfo", path.hash_part()),
                        narinfo.to_text(store.store_dir()),
                    )
                });
            let _ = fs::remove_file(&tmp);
            sent?;
            uploaded.push(path);
        }
        Ok(uploaded)
    }

    fn put(&self, file: &str, body: impl ureq::AsSendBody) -> Result<()> {
        let url = format!("{}/{file}", self.url);
        ureq::put(&url)
            .header("User-Agent", concat!("neki/", env!("CARGO_PKG_VERSION")))
            .send(body)
            .map(|_| ())
            .map_err(|e| match e {
                ureq::Error::StatusCode(403) => {
                    Error::Http(format!("{url}: rejected; does the server trust this key, and was it started with --allow-push?"))
                }
                ureq::Error::StatusCode(405) => {
                    Error::Http(format!("{url}: the server doesn't accept uploads (start it with --allow-push)"))
                }
                e => Error::Http(format!("{url}: {e}")),
            })
    }
}

/// Write `path`'s NAR, zstd-compressed, to the new file `dest` and return
/// its narinfo (URL `nar/<file hash>.nar.zst`) signed with `key`.
fn compressed_nar(
    store: &LocalStore,
    path: &StorePath,
    dest: &Path,
    key: &SecretKey,
) -> Result<NarInfo> {
    let info = store
        .query_path_info(path)?
        .ok_or_else(|| neki_store::Error::NotValid(path.to_string()))?;
    let file = fs::File::create(dest).map_err(at(dest))?;
    let mut hashing = HashingWriter {
        inner: io::BufWriter::new(file),
        hasher: Hasher::new(HashAlgo::Sha256),
    };
    {
        let mut encoder = zstd::stream::write::Encoder::new(&mut hashing, 3).map_err(at(dest))?;
        neki_nar::dump(&store.store_dir().to_path(path), &mut encoder)?;
        encoder.finish().map_err(at(dest))?;
    }
    hashing.inner.flush().map_err(at(dest))?;
    let file_size = hashing.hasher.bytes_hashed();
    let file_hash = hashing.hasher.finish();
    let mut narinfo = NarInfo {
        info,
        url: format!("nar/{}.nar.zst", file_hash.to_base32()),
        compression: "zstd".into(),
        file_hash: Some(file_hash),
        file_size: Some(file_size),
    };
    narinfo.info.signatures = vec![key.sign(&fingerprint(store.store_dir(), &narinfo.info))];
    Ok(narinfo)
}

/// The closure of `paths`, every path after the paths it refers to.
fn references_first(store: &LocalStore, paths: &[StorePath]) -> Result<Vec<StorePath>> {
    fn visit(
        store: &LocalStore,
        path: &StorePath,
        seen: &mut BTreeSet<StorePath>,
        out: &mut Vec<StorePath>,
    ) -> Result<()> {
        if !seen.insert(path.clone()) {
            return Ok(());
        }
        let info = store
            .query_path_info(path)?
            .ok_or_else(|| neki_store::Error::NotValid(path.to_string()))?;
        for reference in &info.references {
            visit(store, reference, seen, out)?;
        }
        out.push(path.clone());
        Ok(())
    }
    let (mut seen, mut out) = (BTreeSet::new(), Vec::new());
    for path in paths {
        visit(store, path, &mut seen, &mut out)?;
    }
    Ok(out)
}

struct HashingWriter<W: Write> {
    inner: W,
    hasher: Hasher,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Write `data` to `path` through a temporary file, so readers never see a
/// partial file.
fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_file_name(format!(
        ".tmp-{}",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    fs::write(&tmp, data).map_err(at(&tmp))?;
    fs::rename(&tmp, path).map_err(at(path))
}

/// Parse `sha256:<base32>` (or any form [`Hash::parse`] accepts).
fn parse_hash(s: &str) -> Result<Hash> {
    Hash::parse(s).map_err(|e| Error::InvalidNarInfo(e.to_string()))
}

#[cfg(test)]
mod tests;
