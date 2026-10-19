//! Derivations: build recipes, stored in the store as `.drv` files (ATerm text).

#![allow(unused_variables)] // skeleton: stub bodies

use std::collections::{BTreeMap, BTreeSet};

use neki_hash::{Hash, StorePath};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid derivation: {0}")]
    Invalid(String),
    #[error(transparent)]
    Hash(#[from] neki_hash::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derivation {
    pub outputs: BTreeMap<String, DerivationOutput>,
    /// Input derivations and which of their outputs are used.
    pub input_drvs: BTreeMap<StorePath, BTreeSet<String>>,
    /// Plain store paths used as inputs (sources, scripts).
    pub input_srcs: BTreeSet<StorePath>,
    /// Target system, e.g. `x86_64-windows` or `aarch64-darwin`.
    pub system: String,
    pub builder: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DerivationOutput {
    /// Output path computed from the derivation's own hash.
    InputAddressed(StorePath),
    /// Output content known in advance (fetchers); the build may use the network.
    Fixed { hash: Hash },
}

impl Derivation {
    pub fn to_aterm(&self) -> String {
        todo!("M3: ATerm serialize")
    }

    pub fn parse_aterm(s: &str) -> Result<Self> {
        todo!("M3: ATerm parse")
    }

    /// Hash of the derivation with each fixed-output input replaced by its
    /// output hash, so changing how a source is fetched doesn't change
    /// dependents ("hashDerivationModulo"). `resolve` loads input derivations.
    pub fn hash_modulo(&self, resolve: &dyn Fn(&StorePath) -> Result<Derivation>) -> Result<Hash> {
        todo!("M3: hash modulo fixed outputs")
    }

    pub fn is_fixed_output(&self) -> bool {
        self.outputs.len() == 1
            && matches!(
                self.outputs.values().next(),
                Some(DerivationOutput::Fixed { .. })
            )
    }
}

/// System string of the host, e.g. `x86_64-linux`, `aarch64-darwin`, `x86_64-windows`.
pub fn host_system() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        os => os,
    };
    format!("{}-{os}", std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_system_is_arch_dash_os() {
        let system = host_system();
        let (arch, os) = system.split_once('-').unwrap();
        assert_eq!(arch, std::env::consts::ARCH);
        assert!(["linux", "darwin", "windows"].contains(&os), "{system}");
    }
}
