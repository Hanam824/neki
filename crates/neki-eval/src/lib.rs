//! Evaluator for a subset of the Nix expression language (no nixpkgs
//! compatibility). Evaluation is lazy: lists and attribute sets hold thunks.

#![allow(unused_variables)] // skeleton: stub bodies

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{}: parse error: {message}", file.display())]
    Parse { file: PathBuf, message: String },
    #[error("evaluation error: {0}")]
    Eval(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// A string plus the store paths it refers to ("string context").
    String {
        s: String,
        context: Vec<String>,
    },
    Path(PathBuf),
    List(Rc<Vec<Thunk>>),
    Attrs(Rc<BTreeMap<String, Thunk>>),
    Lambda(Rc<Lambda>),
}

/// A value that may not be evaluated yet.
#[derive(Debug)]
pub struct Thunk {
    _private: (),
}

/// A function value with its captured environment.
#[derive(Debug)]
pub struct Lambda {
    _private: (),
}

#[derive(Debug, Default)]
pub struct Evaluator {
    /// Directories searched for `<name>` lookups.
    pub search_path: Vec<PathBuf>,
}

impl Evaluator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn eval_file(&mut self, path: &Path) -> Result<Value> {
        todo!("M4: parse and evaluate file")
    }

    /// Evaluate `src`; relative paths in it resolve against `base_dir`.
    pub fn eval_str(&mut self, src: &str, base_dir: &Path) -> Result<Value> {
        todo!("M4: parse and evaluate string")
    }

    /// Force `value` and everything inside it.
    pub fn force_deep(&mut self, value: &Value) -> Result<Value> {
        todo!("M4: deep force")
    }
}
