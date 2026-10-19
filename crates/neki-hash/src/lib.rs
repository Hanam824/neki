//! Hashing, Nix base32, SRI and store path computation.

#![allow(unused_variables)] // skeleton: stub bodies

pub mod base32;
mod hash;
mod store_path;

pub use hash::{Hash, HashAlgo};
pub use store_path::{HASH_LEN, StoreDir, StorePath};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid hash: {0}")]
    InvalidHash(String),
    #[error("invalid store path: {0}")]
    InvalidStorePath(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
