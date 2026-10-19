//! Binary caches: download (substitute) and upload prebuilt store paths.

#![allow(unused_variables)] // skeleton: stub bodies

use neki_hash::StorePath;
use neki_store::{LocalStore, PathInfo};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Store(#[from] neki_store::Error),
    #[error("cache request failed: {0}")]
    Http(String),
    #[error("invalid narinfo: {0}")]
    InvalidNarInfo(String),
    #[error("no trusted signature for {0}")]
    Untrusted(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A `.narinfo` file: metadata for one store path in a cache.
#[derive(Debug, Clone)]
pub struct NarInfo {
    pub info: PathInfo,
    /// Location of the (compressed) NAR, relative to the cache URL.
    pub url: String,
    pub compression: String,
}

impl NarInfo {
    pub fn parse(s: &str) -> Result<Self> {
        todo!("M7: parse narinfo")
    }

    pub fn to_text(&self) -> String {
        todo!("M7: write narinfo")
    }
}

/// An ed25519 public key with its name, e.g. `cache.example.org-1`.
#[derive(Debug, Clone)]
pub struct PublicKey {
    pub name: String,
    pub key: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct SecretKey {
    pub name: String,
    pub key: [u8; 64],
}

/// A binary cache at a `file://` or `https://` URL.
pub struct BinaryCache {
    pub url: String,
    pub trusted_keys: Vec<PublicKey>,
}

impl BinaryCache {
    pub fn new(url: impl Into<String>, trusted_keys: Vec<PublicKey>) -> Self {
        Self {
            url: url.into(),
            trusted_keys,
        }
    }

    pub fn query(&self, path: &StorePath) -> Result<Option<NarInfo>> {
        todo!("M7: query cache")
    }

    /// Download `path` into `store`, after checking its signature.
    pub fn substitute(&self, store: &LocalStore, path: &StorePath) -> Result<()> {
        todo!("M7: substitute")
    }

    /// Upload the closure of `paths`, signed with `key`.
    pub fn push(&self, store: &LocalStore, paths: &[StorePath], key: &SecretKey) -> Result<()> {
        todo!("M7: push")
    }
}
