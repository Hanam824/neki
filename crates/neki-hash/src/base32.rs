//! Nix's base32 encoding, used for the hash part of store path names.

use crate::Result;

/// The 32 characters used, omitting `e`, `o`, `u` and `t`.
pub const ALPHABET: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";

pub fn encode(bytes: &[u8]) -> String {
    todo!("M1: Nix base32 encode")
}

pub fn decode(s: &str) -> Result<Vec<u8>> {
    todo!("M1: Nix base32 decode")
}
