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
    #[error(
        "can only upload to file:// caches (serve one over HTTP with `neki store serve`), not {0}"
    )]
    ReadOnly(String),
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
        let Some(reader) = self.get(&narinfo.url)? else {
            return Ok(false);
        };
        let mut nar: Box<dyn Read> = match narinfo.compression.as_str() {
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
        };
        store.import_nar(&narinfo.info, &mut nar)?;
        done.insert(path.clone());
        Ok(true)
    }

    /// Upload the closures of `paths`, signed with `key`, skipping paths the
    /// cache already has. Only `file://` caches can be written. Returns the
    /// paths uploaded.
    pub fn push(
        &self,
        store: &LocalStore,
        paths: &[StorePath],
        key: &SecretKey,
    ) -> Result<Vec<StorePath>> {
        let dir = self
            .dir()
            .ok_or_else(|| Error::ReadOnly(self.url.clone()))?;
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
            let info = store
                .query_path_info(&path)?
                .ok_or_else(|| neki_store::Error::NotValid(path.to_string()))?;
            // Compress into a temporary file, hashing the compressed bytes.
            let tmp = nar_dir.join(format!(".tmp-{}", path.hash_part()));
            let file = fs::File::create(&tmp).map_err(at(&tmp))?;
            let mut hashing = HashingWriter {
                inner: io::BufWriter::new(file),
                hasher: Hasher::new(HashAlgo::Sha256),
            };
            {
                let mut encoder =
                    zstd::stream::write::Encoder::new(&mut hashing, 3).map_err(at(&tmp))?;
                neki_nar::dump(&store_dir.to_path(&path), &mut encoder)?;
                encoder.finish().map_err(at(&tmp))?;
            }
            hashing.inner.flush().map_err(at(&tmp))?;
            let file_size = hashing.hasher.bytes_hashed();
            let file_hash = hashing.hasher.finish();
            let url = format!("nar/{}.nar.zst", file_hash.to_base32());
            let dest = dir.join(&url);
            fs::rename(&tmp, &dest).map_err(at(&dest))?;
            let mut narinfo = NarInfo {
                info,
                url,
                compression: "zstd".into(),
                file_hash: Some(file_hash),
                file_size: Some(file_size),
            };
            narinfo.info.signatures = vec![key.sign(&fingerprint(store_dir, &narinfo.info))];
            write_atomic(&narinfo_file, narinfo.to_text(store_dir).as_bytes())?;
            uploaded.push(path);
        }
        Ok(uploaded)
    }
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
