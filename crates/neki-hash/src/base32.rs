//! Nix's base32 encoding, used for the hash part of store path names.
//!
//! It differs from RFC 4648 base32: the alphabet omits `e`, `o`, `u` and `t`,
//! and bytes are encoded starting from the end, least significant bits first.

use crate::{Error, Result};

/// The 32 characters used, omitting `e`, `o`, `u` and `t`.
pub const ALPHABET: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// Number of characters needed to encode `bytes` bytes.
pub const fn encoded_len(bytes: usize) -> usize {
    if bytes == 0 {
        0
    } else {
        (bytes * 8 - 1) / 5 + 1
    }
}

/// Encode `bytes` in Nix base32, [`encoded_len`] characters long.
///
/// # Examples
///
/// ```
/// use neki_hash::base32;
/// assert_eq!(base32::encode(&[0x1f]), "0z");
/// ```
pub fn encode(bytes: &[u8]) -> String {
    (0..encoded_len(bytes.len()))
        .rev()
        .map(|n| {
            let (i, j) = (n * 5 / 8, n * 5 % 8);
            let low = u16::from(bytes[i]) >> j;
            let high = bytes.get(i + 1).map_or(0, |&b| u16::from(b) << (8 - j));
            char::from(ALPHABET[usize::from((low | high) & 0x1f)])
        })
        .collect()
}

/// Decode the Nix base32 string `s`. Fails on characters outside
/// [`ALPHABET`] and on bits left over past the last byte.
///
/// # Examples
///
/// ```
/// use neki_hash::base32;
/// assert_eq!(base32::decode("0z").unwrap(), [0x1f]);
/// assert!(base32::decode("0e").is_err());
/// ```
pub fn decode(s: &str) -> Result<Vec<u8>> {
    let invalid = || Error::InvalidHash(format!("invalid base32: {s:?}"));
    let mut out = vec![0u8; s.len() * 5 / 8];
    for (n, c) in s.bytes().rev().enumerate() {
        let digit = ALPHABET.iter().position(|&a| a == c).ok_or_else(invalid)? as u16;
        let (i, j) = (n * 5 / 8, n * 5 % 8);
        // Truncation to u8 is intended: the bits above 8 go to the next byte.
        out[i] |= (digit << j) as u8;
        let carry = (digit << j) >> 8;
        match out.get_mut(i + 1) {
            Some(next) => *next |= carry as u8,
            None if carry != 0 => return Err(invalid()),
            None => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Nix base32 of SHA-256("") and SHA-256("abc"), the vectors from Nix's own test suite.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn matches_nix_vectors() {
        assert_eq!(
            encode(&hex(EMPTY_SHA256)),
            "0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73"
        );
        assert_eq!(
            encode(&hex(ABC_SHA256)),
            "1b8m03r63zqhnjf7l5wnldhh7c134ap5vpj0850ymkq1iyzicy5s"
        );
    }

    #[test]
    fn round_trips() {
        for len in [0, 1, 2, 5, 20, 32, 64] {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            let encoded = encode(&bytes);
            assert_eq!(encoded.len(), encoded_len(len));
            assert_eq!(decode(&encoded).unwrap(), bytes, "length {len}");
        }
    }

    #[test]
    fn rejects_bad_input() {
        assert!(decode("e").is_err(), "e is not in the alphabet");
        // 2 characters hold 10 bits but decode to 1 byte: the top bits must be zero.
        assert!(decode("z0").is_err());
    }
}
