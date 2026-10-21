use std::fmt;
use std::io;

use data_encoding::{BASE64, HEXLOWER, HEXLOWER_PERMISSIVE};
use sha2::Digest;

use crate::{Error, Result, base32};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashAlgo {
    Sha256,
    Sha512,
}

impl HashAlgo {
    /// The name used in hash strings, such as the `sha256` in `sha256-…`.
    pub fn name(self) -> &'static str {
        match self {
            HashAlgo::Sha256 => "sha256",
            HashAlgo::Sha512 => "sha512",
        }
    }

    /// The length of a digest in bytes: 32 for SHA-256, 64 for SHA-512.
    pub fn digest_len(self) -> usize {
        match self {
            HashAlgo::Sha256 => 32,
            HashAlgo::Sha512 => 64,
        }
    }

    /// The algorithm called `name` (`sha256` or `sha512`); fails on any other.
    pub fn from_name(name: &str) -> Result<Self> {
        match name {
            "sha256" => Ok(HashAlgo::Sha256),
            "sha512" => Ok(HashAlgo::Sha512),
            _ => Err(Error::InvalidHash(format!(
                "unknown hash algorithm {name:?}"
            ))),
        }
    }
}

impl fmt::Display for HashAlgo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A digest together with the algorithm that produced it. Displays as SRI.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hash {
    algo: HashAlgo,
    digest: Vec<u8>,
}

impl Hash {
    /// A hash from an existing `digest`, which must be
    /// [`HashAlgo::digest_len`] bytes long.
    pub fn new(algo: HashAlgo, digest: Vec<u8>) -> Result<Self> {
        if digest.len() != algo.digest_len() {
            return Err(Error::InvalidHash(format!(
                "{algo} digest must be {} bytes",
                algo.digest_len()
            )));
        }
        Ok(Self { algo, digest })
    }

    /// The hash of `data`. Use a [`Hasher`] to hash a stream instead.
    ///
    /// # Examples
    ///
    /// ```
    /// use neki_hash::{Hash, HashAlgo};
    /// let hash = Hash::of_bytes(HashAlgo::Sha256, b"abc");
    /// assert_eq!(
    ///     hash.to_base16(),
    ///     "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    /// );
    /// assert_eq!(Hash::parse(&hash.to_sri()).unwrap(), hash);
    /// ```
    pub fn of_bytes(algo: HashAlgo, data: &[u8]) -> Hash {
        let mut hasher = Hasher::new(algo);
        hasher.update(data);
        hasher.finish()
    }

    /// Parse SRI (`sha256-<base64>`), `<algo>:<digest>` with the digest in
    /// base16, Nix base32 or base64, or a bare base16/base32 digest (whose
    /// length decides the algorithm).
    pub fn parse(s: &str) -> Result<Hash> {
        let invalid = || Error::InvalidHash(s.to_string());
        if let Some((algo, digest)) = s.split_once(':') {
            return Self::decode(HashAlgo::from_name(algo)?, digest).ok_or_else(invalid);
        }
        if let Some((algo, digest)) = s.split_once('-')
            && let Ok(algo) = HashAlgo::from_name(algo)
        {
            let digest = BASE64.decode(digest.as_bytes()).map_err(|_| invalid())?;
            return Self::new(algo, digest).map_err(|_| invalid());
        }
        [HashAlgo::Sha256, HashAlgo::Sha512]
            .into_iter()
            .find_map(|algo| Self::decode(algo, s))
            .ok_or_else(invalid)
    }

    /// Decode `encoded` as base16, Nix base32 or base64, chosen by its length.
    fn decode(algo: HashAlgo, encoded: &str) -> Option<Hash> {
        let len = algo.digest_len();
        let digest = if encoded.len() == len * 2 {
            HEXLOWER_PERMISSIVE.decode(encoded.as_bytes()).ok()
        } else if encoded.len() == base32::encoded_len(len) {
            base32::decode(encoded).ok()
        } else if encoded.len() == BASE64.encode_len(len) {
            BASE64.decode(encoded.as_bytes()).ok()
        } else {
            None
        }?;
        Self::new(algo, digest).ok()
    }

    /// The algorithm that produced the digest.
    pub fn algo(&self) -> HashAlgo {
        self.algo
    }

    /// The raw digest bytes.
    pub fn digest(&self) -> &[u8] {
        &self.digest
    }

    /// SRI form, e.g. `sha256-<base64>`.
    pub fn to_sri(&self) -> String {
        format!("{}-{}", self.algo, BASE64.encode(&self.digest))
    }

    /// The digest in lowercase hex, without the algorithm.
    pub fn to_base16(&self) -> String {
        HEXLOWER.encode(&self.digest)
    }

    /// The digest in Nix base32, without the algorithm.
    pub fn to_base32(&self) -> String {
        base32::encode(&self.digest)
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_sri())
    }
}

/// Incremental hasher. As an [`io::Write`] sink it can hash a stream (e.g. a
/// NAR) while counting its size.
pub struct Hasher {
    state: State,
    bytes: u64,
}

enum State {
    Sha256(sha2::Sha256),
    Sha512(sha2::Sha512),
}

impl Hasher {
    /// A hasher with nothing hashed yet.
    pub fn new(algo: HashAlgo) -> Self {
        let state = match algo {
            HashAlgo::Sha256 => State::Sha256(sha2::Sha256::new()),
            HashAlgo::Sha512 => State::Sha512(sha2::Sha512::new()),
        };
        Self { state, bytes: 0 }
    }

    /// Hash `data` after what was hashed before.
    pub fn update(&mut self, data: &[u8]) {
        match &mut self.state {
            State::Sha256(state) => state.update(data),
            State::Sha512(state) => state.update(data),
        }
        self.bytes += data.len() as u64;
    }

    /// Number of bytes hashed so far.
    pub fn bytes_hashed(&self) -> u64 {
        self.bytes
    }

    /// The hash of everything given to [`Hasher::update`] (or written).
    pub fn finish(self) -> Hash {
        let (algo, digest) = match self.state {
            State::Sha256(state) => (HashAlgo::Sha256, state.finalize().to_vec()),
            State::Sha512(state) => (HashAlgo::Sha512, state.finalize().to_vec()),
        };
        Hash { algo, digest }
    }
}

impl io::Write for Hasher {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference values computed with `openssl dgst`.
    const ABC_SHA256_HEX: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const ABC_SHA256_SRI: &str = "sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=";
    const ABC_SHA512_SRI: &str = "sha512-3a81oZNherrMQXNJriBBMRLm+k6JqX6iCp7u5ktV05ohkpkqJ0/BqDa6PCOj/uu9RU1EI2Q86A4qmslPpUyknw==";
    const EMPTY_SHA256_SRI: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";

    #[test]
    fn hashes_match_reference_values() {
        let abc = Hash::of_bytes(HashAlgo::Sha256, b"abc");
        assert_eq!(abc.to_base16(), ABC_SHA256_HEX);
        assert_eq!(abc.to_sri(), ABC_SHA256_SRI);
        assert_eq!(
            Hash::of_bytes(HashAlgo::Sha512, b"abc").to_sri(),
            ABC_SHA512_SRI
        );
        assert_eq!(
            Hash::of_bytes(HashAlgo::Sha256, b"").to_string(),
            EMPTY_SHA256_SRI
        );
    }

    #[test]
    fn streaming_matches_one_shot() {
        let mut hasher = Hasher::new(HashAlgo::Sha256);
        io::Write::write_all(&mut hasher, b"a").unwrap();
        io::Write::write_all(&mut hasher, b"bc").unwrap();
        assert_eq!(hasher.bytes_hashed(), 3);
        assert_eq!(hasher.finish().to_sri(), ABC_SHA256_SRI);
    }

    #[test]
    fn parses_every_format() {
        let abc = Hash::of_bytes(HashAlgo::Sha256, b"abc");
        let forms = [
            ABC_SHA256_SRI.to_string(),
            format!("sha256:{ABC_SHA256_HEX}"),
            format!("sha256:{}", ABC_SHA256_HEX.to_uppercase()),
            format!("sha256:{}", abc.to_base32()),
            format!("sha256:{}", BASE64.encode(abc.digest())),
            ABC_SHA256_HEX.to_string(),
            abc.to_base32(),
        ];
        for form in forms {
            assert_eq!(Hash::parse(&form).unwrap(), abc, "{form}");
        }
        let sha512 = Hash::parse(ABC_SHA512_SRI).unwrap();
        assert_eq!(sha512.algo(), HashAlgo::Sha512);
        assert_eq!(Hash::parse(&sha512.to_base16()).unwrap(), sha512);
    }

    #[test]
    fn rejects_bad_hashes() {
        for bad in [
            "",
            "abc",
            "md5:00",
            "sha256:xyz",
            "sha256-AAAA",
            "sha1-qZk+NkcGgWq6PiVxeFDCbJzQ2J0=",
        ] {
            assert!(Hash::parse(bad).is_err(), "{bad:?}");
        }
        assert!(Hash::new(HashAlgo::Sha256, vec![0; 31]).is_err());
    }
}
