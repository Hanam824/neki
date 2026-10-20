//! Evaluator for a subset of the Nix expression language (no nixpkgs
//! compatibility). Evaluation is lazy: lists and attribute sets hold thunks.
//!
//! Supported: all expression syntax except URI literals and paths with
//! interpolation; `derivation`, string context, `import` and the builtins
//! listed in `builtins.rs`. Files use the `.neki` extension; importing a
//! directory imports its `default.neki`.

mod ast;
mod builtins;
mod derivation;
mod eval;
mod parser;
mod value;

pub use value::{Attrs, Context, ContextElem, Lambda, PrimOpApp, Thunk, Value};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use neki_derivation::Derivation;
use neki_hash::{Hash, StoreDir, StorePath};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{}: parse error: {message}", file.display())]
    Parse { file: PathBuf, message: String },
    #[error("evaluation error: {0}")]
    Eval(String),
    /// `throw` and failed `assert`s, which `builtins.tryEval` catches.
    #[error("{0}")]
    Throw(String),
    #[error("evaluation aborted: {0}")]
    Abort(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Decides, given an entry's path and type (`regular`, `directory` or
/// `symlink`), whether it is copied to the store.
pub type PathFilter<'a> = &'a mut dyn FnMut(&Path, &str) -> Result<bool>;

/// What evaluation needs from the store: `derivation`, path literals in
/// strings, `builtins.toFile` and the fetchers write to it.
pub trait EvalStore {
    fn store_dir(&self) -> &StoreDir;
    /// Copy `path` into the store, keeping what `filter` accepts.
    fn add_path(
        &self,
        name: &str,
        path: &Path,
        filter: Option<PathFilter>,
    ) -> Result<StorePath, String>;
    fn add_text(
        &self,
        name: &str,
        text: &str,
        refs: &BTreeSet<StorePath>,
    ) -> Result<StorePath, String>;
    /// Store a finalized derivation.
    fn add_derivation(&self, drv: &Derivation) -> Result<StorePath, String>;
    fn read_derivation(&self, path: &StorePath) -> Result<Derivation, String>;
    /// Fetch at evaluation time (`builtins.fetchurl`, `fetchTarball`,
    /// `fetchGit`).
    fn fetch(&self, name: &str, request: &FetchRequest) -> Result<StorePath, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchRequest {
    /// A file; `hash` of its contents.
    Url { url: String, hash: Option<Hash> },
    /// An archive unpacked with its top-level directory removed; `hash` of
    /// the tree's NAR.
    Tarball { url: String, hash: Option<Hash> },
    Git {
        url: String,
        rev: Option<String>,
        reference: Option<String>,
    },
}

pub use eval::Evaluator;

#[cfg(test)]
mod tests;
