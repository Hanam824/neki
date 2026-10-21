use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Contents of `neki.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Manifest {
    pub project: ProjectMeta,
    /// External sources (package sets, other projects), pinned by `neki.lock`.
    #[serde(default)]
    pub inputs: BTreeMap<String, Input>,
    /// Environment for `neki develop`.
    #[serde(default)]
    pub dev_shell: Option<DevShell>,
    /// Buildable outputs by name; `default` is used when no name is given.
    #[serde(default)]
    pub packages: BTreeMap<String, PackageDecl>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectMeta {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// An input source: `github:owner/repo`, `git+https://…`, `path:../dir` or an archive URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DevShell {
    /// Packages as `<input>.<attr>`, e.g. `pkgs.nodejs`.
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Command run when the shell starts.
    #[serde(default)]
    pub shell_hook: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PackageDecl {
    /// Source directory, relative to the project root.
    #[serde(default = "default_src")]
    pub src: String,
    /// Build-time dependencies, same syntax as `dev-shell.packages`.
    #[serde(default)]
    pub build_inputs: Vec<String>,
    /// Commands as argv lists, run in order without a shell so they behave
    /// the same on every OS. `${out}` expands to the output directory, and
    /// `${NAME}` to any other build variable.
    #[serde(default)]
    pub build: Vec<Vec<String>>,
    /// Extra environment variables for the build commands.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Paths (relative to `src`) left out of the source. `.git` and
    /// `result` links are always left out.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Instead of `build`: a `.neki` file (relative to the project) holding
    /// a function `{ inputs, src, name, ... }:` that returns the package,
    /// e.g. using a package set's language adapters. `inputs.<name>` is each
    /// input's package set (or directory), `src` the filtered source.
    #[serde(default)]
    pub file: Option<String>,
}

fn default_src() -> String {
    ".".into()
}

impl Manifest {
    /// Parse `neki.toml` text. Unknown keys are errors, so typos don't go
    /// unnoticed.
    ///
    /// # Examples
    ///
    /// ```
    /// use neki_project::Manifest;
    /// let manifest = Manifest::parse("[project]\nname = \"app\"\n").unwrap();
    /// assert_eq!(manifest.project.name, "app");
    /// assert!(Manifest::parse("[project]\nname = \"app\"\nnmae = 1\n").is_err());
    /// ```
    pub fn parse(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Read and parse the `neki.toml` at `path`.
    pub fn from_file(path: &Path) -> Result<Self> {
        Self::parse(&crate::read(path)?).map_err(|err| Error::Parse {
            path: path.to_path_buf(),
            message: err.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_manifest() {
        let src = include_str!("../../../examples/hello/neki.toml");
        let manifest = Manifest::parse(src).unwrap();
        assert_eq!(manifest.project.name, "hello");
        assert!(manifest.inputs.contains_key("pkgs"));
        let default = &manifest.packages["default"];
        assert_eq!(default.src, ".");
        assert!(!default.build.is_empty());
    }

    #[test]
    fn starter_manifest_parses() {
        let manifest = Manifest::parse(&crate::starter_manifest("my \"app\"")).unwrap();
        assert_eq!(manifest.project.name, "my \"app\"");
        assert!(manifest.packages.contains_key("default"));
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(Manifest::parse("[project]\nname = \"x\"\nbogus = 1\n").is_err());
    }
}
