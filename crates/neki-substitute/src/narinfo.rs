use std::collections::BTreeSet;
use std::path::Path;

use neki_hash::{Hash, StoreDir, StorePath};
use neki_store::PathInfo;

use crate::{Error, PublicKey, Result, fingerprint, parse_hash};

/// A `.narinfo` file: metadata for one store path in a cache.
///
/// ```text
/// StorePath: /opt/neki/store/<hash>-hello
/// URL: nar/<file hash>.nar.zst
/// Compression: zstd
/// FileHash: sha256:<base32>
/// FileSize: 1234
/// NarHash: sha256:<base32>
/// NarSize: 5678
/// References: <hash>-glibc <hash>-hello
/// Deriver: <hash>-hello.drv
/// Sig: cache.example.org-1:<base64>
/// ```
#[derive(Debug, Clone)]
pub struct NarInfo {
    pub info: PathInfo,
    /// Location of the (compressed) NAR, relative to the cache URL.
    pub url: String,
    pub compression: String,
    pub file_hash: Option<Hash>,
    pub file_size: Option<u64>,
}

impl NarInfo {
    pub fn parse(s: &str, store_dir: &StoreDir) -> Result<Self> {
        let bad = |m: String| Error::InvalidNarInfo(m);
        let (mut path, mut url, mut nar_hash, mut nar_size) = (None, None, None, None);
        let mut compression = "bzip2".to_string();
        let (mut file_hash, mut file_size, mut deriver) = (None, None, None);
        let mut references = BTreeSet::new();
        let mut signatures = Vec::new();
        for line in s.lines() {
            let Some((key, value)) = line.split_once(": ") else {
                continue;
            };
            let value = value.trim();
            match key {
                "StorePath" => {
                    path = Some(
                        store_dir
                            .parse_path(Path::new(value))
                            .map_err(|e| bad(e.to_string()))?,
                    )
                }
                "URL" => url = Some(value.to_string()),
                "Compression" => compression = value.to_string(),
                "FileHash" => file_hash = Some(parse_hash(value)?),
                "FileSize" => {
                    file_size = Some(
                        value
                            .parse()
                            .map_err(|_| bad(format!("FileSize {value:?}")))?,
                    )
                }
                "NarHash" => nar_hash = Some(parse_hash(value)?),
                "NarSize" => {
                    nar_size = Some(
                        value
                            .parse()
                            .map_err(|_| bad(format!("NarSize {value:?}")))?,
                    )
                }
                "References" => {
                    for r in value.split_whitespace() {
                        references.insert(StorePath::parse(r).map_err(|e| bad(e.to_string()))?);
                    }
                }
                "Deriver" if value != "unknown-deriver" => {
                    deriver = Some(StorePath::parse(value).map_err(|e| bad(e.to_string()))?);
                }
                "Sig" => signatures.push(value.to_string()),
                _ => {}
            }
        }
        let missing = |field: &str| bad(format!("missing {field}"));
        Ok(NarInfo {
            info: PathInfo {
                path: path.ok_or_else(|| missing("StorePath"))?,
                nar_hash: nar_hash.ok_or_else(|| missing("NarHash"))?,
                nar_size: nar_size.ok_or_else(|| missing("NarSize"))?,
                references,
                deriver,
                registration_time: 0,
                signatures,
            },
            url: url.ok_or_else(|| missing("URL"))?,
            compression,
            file_hash,
            file_size,
        })
    }

    pub fn to_text(&self, store_dir: &StoreDir) -> String {
        let i = &self.info;
        let mut out = format!(
            "StorePath: {}\nURL: {}\nCompression: {}\n",
            store_dir.to_path(&i.path).display(),
            self.url,
            self.compression
        );
        if let Some(h) = &self.file_hash {
            out.push_str(&format!("FileHash: {}:{}\n", h.algo(), h.to_base32()));
        }
        if let Some(s) = self.file_size {
            out.push_str(&format!("FileSize: {s}\n"));
        }
        out.push_str(&format!(
            "NarHash: {}:{}\nNarSize: {}\n",
            i.nar_hash.algo(),
            i.nar_hash.to_base32(),
            i.nar_size
        ));
        let refs: Vec<String> = i.references.iter().map(ToString::to_string).collect();
        out.push_str(&format!("References: {}\n", refs.join(" ")));
        if let Some(d) = &i.deriver {
            out.push_str(&format!("Deriver: {d}\n"));
        }
        for sig in &i.signatures {
            out.push_str(&format!("Sig: {sig}\n"));
        }
        out
    }

    /// Whether one of the signatures is valid for one of `keys`.
    pub fn is_trusted(&self, store_dir: &StoreDir, keys: &[PublicKey]) -> bool {
        let message = fingerprint(store_dir, &self.info);
        self.info
            .signatures
            .iter()
            .any(|sig| keys.iter().any(|k| k.verifies(&message, sig)))
    }
}
