use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

pub const LOCK_VERSION: u32 = 1;

/// Contents of `neki.lock`: the exact version of every input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockFile {
    pub version: u32,
    pub inputs: BTreeMap<String, LockedInput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockedInput {
    pub url: String,
    /// Resolved revision (git commit), if the input type has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    /// SRI hash of the fetched source's NAR.
    pub nar_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<i64>,
}

impl LockFile {
    /// Read the lock file at `path`.
    pub fn from_file(path: &Path) -> Result<Self> {
        serde_json::from_str(&crate::read(path)?).map_err(|err| Error::Parse {
            path: path.to_path_buf(),
            message: err.to_string(),
        })
    }

    /// The lock file as written to disk: pretty-printed JSON with a final
    /// newline.
    pub fn to_json(&self) -> String {
        let mut json = serde_json::to_string_pretty(self).expect("lock file is serializable");
        json.push('\n');
        json
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip() {
        let lock = LockFile {
            version: LOCK_VERSION,
            inputs: BTreeMap::from([(
                "pkgs".to_string(),
                LockedInput {
                    url: "github:example/pkgs".into(),
                    rev: Some("0123abcd".into()),
                    nar_hash: "sha256-AAAA".into(),
                    last_modified: None,
                },
            )]),
        };
        let parsed: LockFile = serde_json::from_str(&lock.to_json()).unwrap();
        assert_eq!(parsed, lock);
    }
}
