//! Derivations: build recipes, stored in the store as `.drv` files (ATerm text).
//!
//! ```text
//! Derive([("out","<path>","<hash algo>","<hash>"),…],   outputs
//!        [("<drv path>",["out",…]),…],                  input derivations
//!        ["<path>",…],                                   input sources
//!        "<system>","<builder>",["<arg>",…],
//!        [("<name>","<value>"),…])                       environment
//! ```
//!
//! All paths are absolute. Strings escape `"`, `\`, newline, carriage return
//! and tab with a backslash.

mod aterm;

use std::collections::{BTreeMap, BTreeSet};

use neki_hash::{Hash, HashAlgo, StoreDir, StorePath};

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
    /// Must contain `name`, which names the outputs.
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DerivationOutput {
    /// Output path computed from the derivation's own hash.
    InputAddressed(StorePath),
    /// Output content known in advance (fetchers); the build may use the
    /// network. `recursive` means `hash` is of the output's NAR, otherwise of
    /// the output file's contents.
    Fixed { hash: Hash, recursive: bool },
    /// An input-addressed output whose path isn't computed yet; see
    /// [`Derivation::finalize`].
    Deferred,
}

impl Derivation {
    /// The derivation's name (`env.name`).
    pub fn name(&self) -> Result<&str> {
        self.env
            .get("name")
            .map(String::as_str)
            .ok_or_else(|| Error::Invalid("no `name` in the environment".into()))
    }

    pub fn to_aterm(&self, store: &StoreDir) -> Result<String> {
        let inputs = self
            .input_drvs
            .iter()
            .map(|(path, outputs)| (full(store, path), outputs.clone()))
            .collect();
        self.write_aterm(store, &inputs)
    }

    fn write_aterm(
        &self,
        store: &StoreDir,
        input_drvs: &BTreeMap<String, BTreeSet<String>>,
    ) -> Result<String> {
        let mut outputs = Vec::new();
        for (name, output) in &self.outputs {
            outputs.push(match output {
                DerivationOutput::InputAddressed(path) => [
                    name.clone(),
                    full(store, path),
                    String::new(),
                    String::new(),
                ],
                DerivationOutput::Fixed { hash, recursive } => {
                    let path = fixed_output_path(store, self.name()?, hash, *recursive)?;
                    let algo = format!("{}{}", if *recursive { "r:" } else { "" }, hash.algo());
                    [name.clone(), full(store, &path), algo, hash.to_base16()]
                }
                DerivationOutput::Deferred => {
                    [name.clone(), String::new(), String::new(), String::new()]
                }
            });
        }
        let srcs: Vec<String> = self.input_srcs.iter().map(|p| full(store, p)).collect();
        Ok(aterm::write(&aterm::Parts {
            outputs,
            input_drvs: input_drvs.clone(),
            input_srcs: srcs,
            system: self.system.clone(),
            builder: self.builder.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
        }))
    }

    pub fn parse_aterm(s: &str, store: &StoreDir) -> Result<Self> {
        let parts = aterm::parse(s)?;
        let path = |p: &str| Ok::<_, Error>(store.parse_path(std::path::Path::new(p))?);
        let mut outputs = BTreeMap::new();
        for [name, out_path, algo, hash] in parts.outputs {
            let output = if !algo.is_empty() {
                let (recursive, algo) = match algo.strip_prefix("r:") {
                    Some(algo) => (true, algo),
                    None => (false, algo.as_str()),
                };
                let hash = Hash::parse(&format!("{algo}:{hash}"))?;
                DerivationOutput::Fixed { hash, recursive }
            } else if out_path.is_empty() {
                DerivationOutput::Deferred
            } else {
                DerivationOutput::InputAddressed(path(&out_path)?)
            };
            outputs.insert(name, output);
        }
        let mut input_drvs = BTreeMap::new();
        for (drv, outs) in parts.input_drvs {
            input_drvs.insert(path(&drv)?, outs);
        }
        let input_srcs = parts
            .input_srcs
            .iter()
            .map(|p| path(p))
            .collect::<Result<_>>()?;
        Ok(Self {
            outputs,
            input_drvs,
            input_srcs,
            system: parts.system,
            builder: parts.builder,
            args: parts.args,
            env: parts.env,
        })
    }

    /// Hash of the derivation with each input derivation replaced by its own
    /// hash modulo, recursively, and a fixed-output derivation reduced to its
    /// output hash and path, so changing how a source is fetched doesn't
    /// change dependents ("hashDerivationModulo"). `resolve` loads input
    /// derivations.
    pub fn hash_modulo(
        &self,
        store: &StoreDir,
        resolve: &dyn Fn(&StorePath) -> Result<Derivation>,
    ) -> Result<Hash> {
        if let Some((hash, recursive)) = self.fixed_output() {
            let path = fixed_output_path(store, self.name()?, hash, recursive)?;
            return Ok(Hash::of_bytes(
                HashAlgo::Sha256,
                format!(
                    "fixed:out:{}{}:{}:{}",
                    if recursive { "r:" } else { "" },
                    hash.algo(),
                    hash.to_base16(),
                    full(store, &path)
                )
                .as_bytes(),
            ));
        }
        let mut inputs = BTreeMap::new();
        for (drv, outputs) in &self.input_drvs {
            let hash = resolve(drv)?.hash_modulo(store, resolve)?;
            inputs.insert(hash.to_base16(), outputs.clone());
        }
        let aterm = self.write_aterm(store, &inputs)?;
        Ok(Hash::of_bytes(HashAlgo::Sha256, aterm.as_bytes()))
    }

    /// Compute the paths of [`DerivationOutput::Deferred`] outputs and put
    /// every output path in the environment under the output's name, as
    /// builders expect (`$out`).
    pub fn finalize(
        &mut self,
        store: &StoreDir,
        resolve: &dyn Fn(&StorePath) -> Result<Derivation>,
    ) -> Result<()> {
        let name = self.name()?.to_string();
        for output in self.outputs.keys() {
            self.env.insert(output.clone(), String::new());
        }
        if self
            .outputs
            .values()
            .any(|o| matches!(o, DerivationOutput::Deferred))
        {
            for output in self.outputs.values_mut() {
                if let DerivationOutput::InputAddressed(_) = output {
                    *output = DerivationOutput::Deferred;
                }
            }
            let hash = self.hash_modulo(store, resolve)?;
            for (output, value) in self.outputs.iter_mut() {
                if let DerivationOutput::Deferred = value {
                    let path = store.make_path(
                        &format!("output:{output}"),
                        &hash,
                        &output_path_name(&name, output),
                    )?;
                    *value = DerivationOutput::InputAddressed(path);
                }
            }
        }
        for (output, path) in self.output_paths(store)? {
            self.env.insert(output, full(store, &path));
        }
        Ok(())
    }

    /// The store path of every output. Fails if an output is still deferred.
    pub fn output_paths(&self, store: &StoreDir) -> Result<BTreeMap<String, StorePath>> {
        let name = self.name()?;
        self.outputs
            .iter()
            .map(|(output, value)| {
                let path = match value {
                    DerivationOutput::InputAddressed(path) => path.clone(),
                    DerivationOutput::Fixed { hash, recursive } => {
                        fixed_output_path(store, name, hash, *recursive)?
                    }
                    DerivationOutput::Deferred => {
                        return Err(Error::Invalid(format!(
                            "output {output} has no path yet; finalize the derivation first"
                        )));
                    }
                };
                Ok((output.clone(), path))
            })
            .collect()
    }

    /// The paths this derivation's `.drv` file refers to.
    pub fn references(&self) -> BTreeSet<StorePath> {
        self.input_drvs
            .keys()
            .chain(&self.input_srcs)
            .cloned()
            .collect()
    }

    /// Where the `.drv` file is stored: a text path (see
    /// `neki_store::Store::add_text`) named `<name>.drv`.
    pub fn drv_path(&self, store: &StoreDir) -> Result<StorePath> {
        let mut kind = String::from("text");
        for reference in self.references() {
            kind.push(':');
            kind.push_str(&full(store, &reference));
        }
        let aterm = self.to_aterm(store)?;
        Ok(store.make_path(
            &kind,
            &Hash::of_bytes(HashAlgo::Sha256, aterm.as_bytes()),
            &format!("{}.drv", self.name()?),
        )?)
    }

    pub fn is_fixed_output(&self) -> bool {
        self.fixed_output().is_some()
    }

    fn fixed_output(&self) -> Option<(&Hash, bool)> {
        match (self.outputs.len(), self.outputs.get("out")) {
            (1, Some(DerivationOutput::Fixed { hash, recursive })) => Some((hash, *recursive)),
            _ => None,
        }
    }
}

/// The path of a fixed output named `name` with content hash `hash`. A
/// recursive SHA-256 gives the same path as adding the tree with
/// `add_path`, so a fetched source and the same source added by hand match.
pub fn fixed_output_path(
    store: &StoreDir,
    name: &str,
    hash: &Hash,
    recursive: bool,
) -> Result<StorePath> {
    if recursive && hash.algo() == HashAlgo::Sha256 {
        return Ok(store.make_path("source", hash, name)?);
    }
    let inner = format!(
        "fixed:out:{}{}:{}:",
        if recursive { "r:" } else { "" },
        hash.algo(),
        hash.to_base16()
    );
    Ok(store.make_path(
        "output:out",
        &Hash::of_bytes(HashAlgo::Sha256, inner.as_bytes()),
        name,
    )?)
}

/// `<name>` for the `out` output, `<name>-<output>` for the others.
pub fn output_path_name(name: &str, output: &str) -> String {
    if output == "out" {
        name.to_string()
    } else {
        format!("{name}-{output}")
    }
}

fn full(store: &StoreDir, path: &StorePath) -> String {
    store.to_path(path).display().to_string()
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
mod tests;
