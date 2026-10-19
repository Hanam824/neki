use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::{Error, Hash, HashAlgo, Result, base32};

/// Length in bytes of the truncated hash in a store path name.
pub const HASH_LEN: usize = 20;

/// Longest allowed store path name (the part after `<hash>-`).
pub const MAX_NAME_LEN: usize = 211;

/// The store directory, e.g. `/opt/neki/store`. It is part of every store
/// path hash, so stores with different directories never share paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreDir(PathBuf);

impl StoreDir {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Absolute filesystem location of `path`.
    pub fn to_path(&self, path: &StorePath) -> PathBuf {
        self.0.join(path.to_string())
    }

    /// Parse a path that is directly inside this store.
    pub fn parse_path(&self, path: &Path) -> Result<StorePath> {
        let invalid = || {
            Error::InvalidStorePath(format!(
                "{} is not directly inside {}",
                path.display(),
                self.0.display()
            ))
        };
        let rest = path.strip_prefix(&self.0).map_err(|_| invalid())?;
        let mut components = rest.components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(name)), None) => {
                StorePath::parse(name.to_str().ok_or_else(invalid)?)
            }
            _ => Err(invalid()),
        }
    }

    /// Compute a store path, as Nix's `makeStorePath` does: hash the
    /// fingerprint `<kind>:<algo>:<inner hash in base16>:<store dir>:<name>`
    /// with SHA-256 and fold the digest to [`HASH_LEN`] bytes. `kind` says
    /// what `inner` is the hash of, e.g. `source` for a NAR added to the store.
    pub fn make_path(&self, kind: &str, inner: &Hash, name: &str) -> Result<StorePath> {
        validate_name(name)?;
        let fingerprint = format!(
            "{kind}:{}:{}:{}:{name}",
            inner.algo(),
            inner.to_base16(),
            self.0.display()
        );
        let digest = Hash::of_bytes(HashAlgo::Sha256, fingerprint.as_bytes());
        Ok(StorePath {
            digest: fold(digest.digest()),
            name: name.to_string(),
        })
    }
}

/// XOR-fold `digest` into [`HASH_LEN`] bytes.
fn fold(digest: &[u8]) -> [u8; HASH_LEN] {
    let mut out = [0; HASH_LEN];
    for (i, byte) in digest.iter().enumerate() {
        out[i % HASH_LEN] ^= byte;
    }
    out
}

/// Check a store path name: 1 to [`MAX_NAME_LEN`] characters from
/// `A-Z a-z 0-9 + - . _ =`, not starting or ending with `.`. (Nix also
/// allows `?`, which isn't valid in Windows file names.)
fn validate_name(name: &str) -> Result<()> {
    let invalid = |why: &str| Err(Error::InvalidStorePath(format!("name {name:?} {why}")));
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return invalid(&format!("must be 1 to {MAX_NAME_LEN} characters"));
    }
    if let Some(c) = name
        .chars()
        .find(|&c| !(c.is_ascii_alphanumeric() || "+-._=".contains(c)))
    {
        return invalid(&format!(
            "contains {c:?}; allowed are A-Z a-z 0-9 + - . _ ="
        ));
    }
    if name.starts_with('.') || name.ends_with('.') {
        return invalid("must not start or end with '.'");
    }
    Ok(())
}

/// A store path relative to its store directory: `<base32 hash>-<name>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StorePath {
    digest: [u8; HASH_LEN],
    name: String,
}

impl StorePath {
    /// Parse `<base32 hash>-<name>`.
    pub fn parse(s: &str) -> Result<Self> {
        let invalid = || Error::InvalidStorePath(format!("{s:?} is not <hash>-<name>"));
        let (hash, name) = s.split_once('-').ok_or_else(invalid)?;
        if hash.len() != base32::encoded_len(HASH_LEN) {
            return Err(invalid());
        }
        let digest = base32::decode(hash)
            .map_err(|_| invalid())?
            .try_into()
            .map_err(|_| invalid())?;
        validate_name(name)?;
        Ok(Self {
            digest,
            name: name.to_string(),
        })
    }

    pub fn digest(&self) -> &[u8; HASH_LEN] {
        &self.digest
    }

    /// The hash part of the name, in Nix base32.
    pub fn hash_part(&self) -> String {
        base32::encode(&self.digest)
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for StorePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.hash_part(), self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> StoreDir {
        StoreDir::new("/opt/neki/store")
    }

    #[test]
    fn make_path_is_stable() {
        // Golden vector: changing it changes every store path (see CLAUDE.md, format stability).
        // Cross-checked with an independent Python implementation of the same fingerprint scheme.
        let inner = Hash::of_bytes(HashAlgo::Sha256, b"abc");
        let path = store().make_path("source", &inner, "hello-1.0").unwrap();
        assert_eq!(path.to_string(), GOLDEN_SOURCE_PATH);
        assert_eq!(path.name(), "hello-1.0");
    }

    const GOLDEN_SOURCE_PATH: &str = "d05zlm1w7b5vpgi1cixas5r9jmxi4ksj-hello-1.0";

    #[test]
    fn parse_round_trips() {
        let path = store()
            .make_path(
                "source",
                &Hash::of_bytes(HashAlgo::Sha256, b"x"),
                "a+b_c=1.2",
            )
            .unwrap();
        let text = path.to_string();
        assert_eq!(StorePath::parse(&text).unwrap(), path);
        assert_eq!(store().parse_path(&store().to_path(&path)).unwrap(), path);
    }

    #[test]
    fn rejects_bad_paths() {
        let hash = "0".repeat(32);
        for bad in [
            "no-dash-hash".to_string(),
            format!("{hash}-"),
            format!("{hash}-.hidden"),
            format!("{hash}-trailing."),
            format!("{hash}-what?"),
            format!("{hash}-a b"),
            format!("{hash}-{}", "x".repeat(MAX_NAME_LEN + 1)),
            format!("{}-short", "0".repeat(31)),
            format!("{}-bad-alphabet", "e".repeat(32)),
        ] {
            assert!(StorePath::parse(&bad).is_err(), "{bad:?}");
        }
        let elsewhere = Path::new("/somewhere/else/00000000000000000000000000000000-x");
        assert!(store().parse_path(elsewhere).is_err());
        let nested = store().path().join(format!("{hash}-x")).join("bin");
        assert!(store().parse_path(&nested).is_err());
    }
}
