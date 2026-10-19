use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashAlgo {
    Sha256,
}

/// A digest together with the algorithm that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hash {
    pub algo: HashAlgo,
    pub digest: Vec<u8>,
}

impl Hash {
    pub fn of_bytes(algo: HashAlgo, data: &[u8]) -> Hash {
        todo!("M1: hash bytes")
    }

    /// Parse SRI (`sha256-<base64>`), `sha256:<base32>` or a bare base16/base32 digest.
    pub fn parse(s: &str) -> Result<Hash> {
        todo!("M1: parse hash")
    }

    /// SRI form, e.g. `sha256-<base64>`.
    pub fn to_sri(&self) -> String {
        todo!("M1: SRI encode")
    }
}
