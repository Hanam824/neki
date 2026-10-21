//! Fetchers: download sources into the store as fixed-output paths.
//!
//! URLs can be `https://`, `http://` or `file://` (local mirrors and tests).
//! Archives (zip, tar, tar.gz, tar.xz) are unpacked with every entry name
//! checked like a NAR entry, so a malicious archive can't write outside its
//! destination or create names that are invalid on another OS.

mod git;
mod unpack;

pub use git::{git_checkout, git_resolve, git_source_files, github_resolve, github_tarball_url};
pub use unpack::unpack;

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use neki_hash::{Hash, StorePath};
use neki_store::{LocalStore, Store};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Store(#[from] neki_store::Error),
    #[error("hash mismatch for {url}: expected {expected}, got {got}")]
    HashMismatch {
        url: String,
        expected: String,
        got: String,
    },
    #[error("fetching {url} failed: {reason}")]
    Failed { url: String, reason: String },
    #[error("listing the files of {} failed: {reason}", dir.display())]
    SourceFiles { dir: PathBuf, reason: String },
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("cannot unpack {}: {reason}", archive.display())]
    Unpack { archive: PathBuf, reason: String },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What to fetch. Everything is pinned: by content hash, or by git
/// revision. A `None` hash means "not known yet" (when locking an input);
/// the path is then computed from what was fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchSpec {
    /// A single file; `hash` is of its contents.
    Url { url: String, hash: Hash },
    /// An archive (zip, tar, tar.gz, tar.xz) unpacked into a directory;
    /// `strip_root` removes a single top-level directory. `hash` is of the
    /// unpacked tree's NAR.
    Archive {
        url: String,
        hash: Option<Hash>,
        strip_root: bool,
    },
    /// A git checkout at `rev`, without `.git` and without any line-ending
    /// conversion. `hash` is of the checkout's NAR.
    Git {
        url: String,
        rev: String,
        hash: Option<Hash>,
    },
}

/// Download progress: bytes done and, if known, the total.
pub type Progress<'a> = &'a dyn Fn(u64, Option<u64>);

/// Fetch `spec` into `store` as a path named `name`. Nothing is downloaded
/// if a path with the expected hash is already valid.
pub fn fetch(
    store: &LocalStore,
    name: &str,
    spec: &FetchSpec,
    progress: Progress,
) -> Result<StorePath> {
    let known = match spec {
        FetchSpec::Url { hash, .. } => Some((hash, false)),
        FetchSpec::Archive { hash, .. } | FetchSpec::Git { hash, .. } => {
            hash.as_ref().map(|h| (h, true))
        }
    };
    if let Some((hash, recursive)) = known {
        let path = neki_derivation::fixed_output_path(store.store_dir(), name, hash, recursive)
            .map_err(neki_store::Error::from)?;
        if store.is_valid_path(&path)? {
            return Ok(path);
        }
    }
    let tmp = store.temp_entry();
    std::fs::create_dir(tmp.path()).map_err(at(tmp.path()))?;
    let (url, fetched) = match spec {
        FetchSpec::Url { url, .. } => {
            let file = tmp.path().join("file");
            download(url, &file, progress)?;
            (url, file)
        }
        FetchSpec::Archive {
            url, strip_root, ..
        } => {
            let archive = tmp.path().join("archive");
            download(url, &archive, progress)?;
            let tree = tmp.path().join("tree");
            // Case collisions are dropped; the hash check catches surprises.
            unpack(&archive, &tree, *strip_root)?;
            (url, tree)
        }
        FetchSpec::Git { url, rev, .. } => {
            let tree = tmp.path().join("tree");
            git_checkout(url, rev, &tree)?;
            (url, tree)
        }
    };
    let added = match known {
        Some((hash, recursive)) => store.add_fixed(name, &fetched, hash, recursive),
        None => store.add_path(name, &fetched),
    };
    added.map_err(|err| match err {
        neki_store::Error::HashMismatch { expected, got, .. } => Error::HashMismatch {
            url: url.clone(),
            expected,
            got,
        },
        err => err.into(),
    })
}

/// Download `url` to the new file `dest`.
pub fn download(url: &str, dest: &Path, progress: Progress) -> Result<()> {
    let failed = |reason: String| Error::Failed {
        url: url.to_string(),
        reason,
    };
    let mut out = File::create_new(dest).map_err(at(dest))?;
    if let Some(path) = file_url_path(url) {
        let mut file = File::open(&path).map_err(|err| failed(err.to_string()))?;
        let total = file.metadata().ok().map(|m| m.len());
        copy_with_progress(&mut file, &mut out, total, progress)
            .map_err(|err| failed(err.to_string()))?;
        return Ok(());
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(failed(
            "only https://, http:// and file:// URLs are supported".into(),
        ));
    }
    let response = http_get(url, &[])?;
    let total = response.body().content_length();
    let mut body = response.into_body().into_reader();
    copy_with_progress(&mut body, &mut out, total, progress)
        .map_err(|err| failed(err.to_string()))?;
    out.sync_all().map_err(at(dest))?;
    Ok(())
}

/// GET `url` with extra `headers`, failing on non-success statuses.
fn http_get(url: &str, headers: &[(&str, &str)]) -> Result<ureq::http::Response<ureq::Body>> {
    let mut request =
        ureq::get(url).header("User-Agent", concat!("neki/", env!("CARGO_PKG_VERSION")));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.call().map_err(|err| Error::Failed {
        url: url.to_string(),
        reason: err.to_string(),
    })
}

fn copy_with_progress(
    from: &mut impl Read,
    to: &mut impl Write,
    total: Option<u64>,
    progress: Progress,
) -> io::Result<()> {
    let mut buf = vec![0; 64 * 1024];
    let mut done = 0;
    progress(0, total);
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            break;
        }
        to.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, total);
    }
    to.flush()
}

/// The local path of a `file://` URL (`file:///C:/x` on Windows).
fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    // `/C:/…` names a drive; drop the slash before it.
    let rest = if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' {
        &rest[1..]
    } else {
        rest
    };
    Some(PathBuf::from(percent_decode(rest)))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A `file://` URL for a local path.
pub fn file_url(path: &Path) -> String {
    let path = path.display().to_string().replace('\\', "/");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

fn at(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests;
