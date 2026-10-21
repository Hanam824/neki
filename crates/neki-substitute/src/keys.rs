//! Signing keys, in Nix's text form `<name>:<base64>`.

use std::fmt;

use data_encoding::BASE64;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use neki_hash::StoreDir;
use neki_store::PathInfo;

use crate::{Error, Result};

/// An ed25519 public key with its name, e.g. `cache.example.org-1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    pub name: String,
    pub key: [u8; 32],
}

#[derive(Clone)]
pub struct SecretKey {
    pub name: String,
    /// The 32-byte seed followed by the 32-byte public key, as Nix stores it.
    pub key: [u8; 64],
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretKey({})", self.name)
    }
}

fn split(s: &str) -> Result<(&str, Vec<u8>)> {
    let (name, b64) = s
        .trim()
        .split_once(':')
        .ok_or_else(|| Error::InvalidKey("expected <name>:<base64>".into()))?;
    if name.is_empty() {
        return Err(Error::InvalidKey("the key has no name".into()));
    }
    let bytes = BASE64
        .decode(b64.as_bytes())
        .map_err(|e| Error::InvalidKey(format!("{name}: {e}")))?;
    Ok((name, bytes))
}

impl PublicKey {
    /// Parse `<name>:<base64>`, the form used in `trusted-public-keys`; the
    /// key is 32 bytes.
    pub fn parse(s: &str) -> Result<Self> {
        let (name, bytes) = split(s)?;
        let key = bytes
            .try_into()
            .map_err(|_| Error::InvalidKey(format!("{name}: a public key is 32 bytes")))?;
        Ok(Self {
            name: name.to_string(),
            key,
        })
    }

    /// Whether `signature` (`<name>:<base64>`) is this key's valid signature
    /// of `message`.
    pub fn verifies(&self, message: &str, signature: &str) -> bool {
        let Some((name, b64)) = signature.split_once(':') else {
            return false;
        };
        if name != self.name {
            return false;
        }
        let Ok(bytes) = BASE64.decode(b64.as_bytes()) else {
            return false;
        };
        let Ok(bytes) = <[u8; 64]>::try_from(bytes) else {
            return false;
        };
        let Ok(key) = VerifyingKey::from_bytes(&self.key) else {
            return false;
        };
        key.verify_strict(message.as_bytes(), &Signature::from_bytes(&bytes))
            .is_ok()
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.name, BASE64.encode(&self.key))
    }
}

impl SecretKey {
    /// A new random key named `name`.
    pub fn generate(name: &str) -> Result<Self> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| Error::InvalidKey(format!("no randomness: {e}")))?;
        Ok(Self {
            name: name.to_string(),
            key: SigningKey::from_bytes(&seed).to_keypair_bytes(),
        })
    }

    /// Parse `<name>:<base64>`, the form [`SecretKey::to_text`] writes; the
    /// key is the 64-byte ed25519 key pair, and it must be valid.
    pub fn parse(s: &str) -> Result<Self> {
        let (name, bytes) = split(s)?;
        let key: [u8; 64] = bytes
            .try_into()
            .map_err(|_| Error::InvalidKey(format!("{name}: a secret key is 64 bytes")))?;
        SigningKey::from_keypair_bytes(&key)
            .map_err(|e| Error::InvalidKey(format!("{name}: {e}")))?;
        Ok(Self {
            name: name.to_string(),
            key,
        })
    }

    fn signing_key(&self) -> SigningKey {
        SigningKey::from_keypair_bytes(&self.key).expect("checked when parsed or generated")
    }

    /// The public key that verifies this key's signatures, with the same
    /// name.
    pub fn public(&self) -> PublicKey {
        PublicKey {
            name: self.name.clone(),
            key: self.signing_key().verifying_key().to_bytes(),
        }
    }

    /// `<name>:<base64 signature>` of `message`.
    pub fn sign(&self, message: &str) -> String {
        let signature = self.signing_key().sign(message.as_bytes());
        format!("{}:{}", self.name, BASE64.encode(&signature.to_bytes()))
    }

    /// The key in text form. Keep it secret.
    pub fn to_text(&self) -> String {
        format!("{}:{}", self.name, BASE64.encode(&self.key))
    }
}

/// What a path's signature signs: `1;<path>;<nar hash>;<nar size>;<refs>`,
/// with full paths, the NAR hash as `sha256:<base32>`, and the references
/// comma-separated.
pub fn fingerprint(store_dir: &StoreDir, info: &PathInfo) -> String {
    let full = |p| store_dir.to_path(p).display().to_string();
    let refs: Vec<String> = info.references.iter().map(full).collect();
    format!(
        "1;{};{}:{};{};{}",
        full(&info.path),
        info.nar_hash.algo(),
        info.nar_hash.to_base32(),
        info.nar_size,
        refs.join(",")
    )
}
