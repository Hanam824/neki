//! Input URLs (`neki.toml` `[inputs]`) and fetching them, pinned by a lock.
//!
//! - `github:<owner>/<repo>[/<ref>]`: a GitHub repository at a branch, tag
//!   or commit (the default branch if none), fetched as a tarball;
//! - `git+<url>`: any git repository (needs `git`), with `?ref=` and `?rev=`;
//! - `path:<dir>`: a local directory, relative to the project;
//! - `https://…`, `http://…`, `file://…`: an archive (zip or tarball).
//!
//! Any of them can end in `?dir=<subdir>` to use a directory inside.

use std::path::{Path, PathBuf};

use neki_fetch::FetchSpec;
use neki_hash::{Hash, StorePath};
use neki_project::LockedInput;
use neki_store::{EntryKind, LocalStore, Store};

use crate::{Error, Event, EventSink, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputUrl {
    Path(String),
    GitHub {
        owner: String,
        repo: String,
        reference: Option<String>,
    },
    Git {
        url: String,
        reference: Option<String>,
        rev: Option<String>,
    },
    Archive(String),
}

/// A parsed input URL with its `?dir=`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub url: InputUrl,
    pub dir: Option<String>,
}

impl Input {
    /// Parse an input URL: `path:<dir>`, `github:<owner>/<repo>[/<ref>]`,
    /// `git+<url>` or an archive URL, each with optional `?dir=<subdir>`
    /// (and `?ref=` / `?rev=` for git).
    ///
    /// # Examples
    ///
    /// ```
    /// use neki_core::{Input, InputUrl};
    /// let input = Input::parse("github:Hanam824/neki?dir=pkgs").unwrap();
    /// assert_eq!(input.dir.as_deref(), Some("pkgs"));
    /// assert!(matches!(input.url, InputUrl::GitHub { reference: None, .. }));
    /// assert!(Input::parse("path:../pkgs?bogus=1").is_err());
    /// ```
    pub fn parse(s: &str) -> Result<Self> {
        let bad = |why: &str| Error::Input(format!("{s}: {why}"));
        let (base, query) = match s.split_once('?') {
            Some((b, q)) => (b, Some(q)),
            None => (s, None),
        };
        let (mut dir, mut reference, mut rev) = (None, None, None);
        for pair in query.into_iter().flat_map(|q| q.split('&')) {
            match pair.split_once('=') {
                Some(("dir", v)) => dir = Some(v.to_string()),
                Some(("ref", v)) => reference = Some(v.to_string()),
                Some(("rev", v)) => rev = Some(v.to_string()),
                _ => return Err(bad(&format!("unknown parameter {pair:?}"))),
            }
        }
        let url = if let Some(path) = base.strip_prefix("path:") {
            InputUrl::Path(path.to_string())
        } else if let Some(rest) = base.strip_prefix("github:") {
            let mut parts = rest.splitn(3, '/');
            let (Some(owner), Some(repo)) = (parts.next(), parts.next()) else {
                return Err(bad("expected github:<owner>/<repo>"));
            };
            if owner.is_empty() || repo.is_empty() {
                return Err(bad("expected github:<owner>/<repo>"));
            }
            InputUrl::GitHub {
                owner: owner.to_string(),
                repo: repo.to_string(),
                reference: parts
                    .next()
                    .map(String::from)
                    .or(reference.clone())
                    .or(rev.clone()),
            }
        } else if let Some(url) = base.strip_prefix("git+") {
            InputUrl::Git {
                url: url.to_string(),
                reference: reference.clone(),
                rev: rev.clone(),
            }
        } else if ["https://", "http://", "file://"]
            .iter()
            .any(|p| base.starts_with(p))
        {
            InputUrl::Archive(base.to_string())
        } else {
            return Err(bad("expected github:, git+, path: or an archive URL"));
        };
        Ok(Input { url, dir })
    }

    /// Fetch the input into `store`: the locked version if `locked` is
    /// given (and still for the same URL), otherwise the newest. Returns the
    /// fetched path, the directory to use (with `?dir=`) and the lock entry.
    pub fn fetch(
        &self,
        original: &str,
        store: &LocalStore,
        base: &Path,
        locked: Option<&LockedInput>,
        events: &dyn EventSink,
    ) -> Result<(StorePath, PathBuf, LockedInput)> {
        let locked = locked.filter(|l| l.url == original);
        let locked_hash = locked
            .map(|l| {
                Hash::parse(&l.nar_hash)
                    .map_err(|e| Error::Input(format!("{original}: neki.lock: {e}")))
            })
            .transpose()?;
        let progress = |done: u64, total: Option<u64>| {
            if done == 0 {
                events.event(Event::Message(match total {
                    Some(t) => format!("fetching {original} ({t} bytes)"),
                    None => format!("fetching {original}"),
                }));
            }
        };
        let (path, rev) = match &self.url {
            InputUrl::Path(p) => {
                let dir = base.join(p);
                let path =
                    store.add_path_filtered("source", &dir, &mut |entry, _: EntryKind| {
                        entry.file_name().is_none_or(|n| n != ".git")
                    })?;
                (path, None)
            }
            InputUrl::GitHub {
                owner,
                repo,
                reference,
            } => {
                let rev = match locked.and_then(|l| l.rev.clone()) {
                    Some(rev) => rev,
                    None => {
                        events.event(Event::Message(format!("resolving {original}")));
                        neki_fetch::github_resolve(owner, repo, reference.as_deref())?
                    }
                };
                let spec = FetchSpec::Archive {
                    url: neki_fetch::github_tarball_url(owner, repo, &rev),
                    hash: locked_hash.clone(),
                    strip_root: true,
                };
                (
                    neki_fetch::fetch(store, "source", &spec, &progress)?,
                    Some(rev),
                )
            }
            InputUrl::Git {
                url,
                reference,
                rev,
            } => {
                let rev = match locked.and_then(|l| l.rev.clone()).or_else(|| rev.clone()) {
                    Some(r) => r,
                    None => neki_fetch::git_resolve(url, reference.as_deref())?,
                };
                let spec = FetchSpec::Git {
                    url: url.clone(),
                    rev: rev.clone(),
                    hash: locked_hash.clone(),
                };
                (
                    neki_fetch::fetch(store, "source", &spec, &progress)?,
                    Some(rev),
                )
            }
            InputUrl::Archive(url) => {
                let spec = FetchSpec::Archive {
                    url: url.clone(),
                    hash: locked_hash.clone(),
                    strip_root: true,
                };
                (neki_fetch::fetch(store, "source", &spec, &progress)?, None)
            }
        };
        let info = store
            .query_path_info(&path)?
            .ok_or_else(|| neki_store::Error::NotValid(path.to_string()))?;
        let mut root = store.store_dir().to_path(&path);
        if let Some(dir) = &self.dir {
            root = root.join(dir);
        }
        let entry = LockedInput {
            url: original.to_string(),
            rev,
            nar_hash: info.nar_hash.to_sri(),
            last_modified: None,
        };
        Ok((path, root, entry))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_input_urls() {
        let p = |s: &str| Input::parse(s).unwrap();
        assert_eq!(
            p("github:Hanam824/neki?dir=pkgs"),
            Input {
                url: InputUrl::GitHub {
                    owner: "Hanam824".into(),
                    repo: "neki".into(),
                    reference: None
                },
                dir: Some("pkgs".into())
            }
        );
        assert_eq!(
            p("github:a/b/v1.0").url,
            InputUrl::GitHub {
                owner: "a".into(),
                repo: "b".into(),
                reference: Some("v1.0".into())
            }
        );
        assert_eq!(
            p("git+https://example.org/r.git?ref=main").url,
            InputUrl::Git {
                url: "https://example.org/r.git".into(),
                reference: Some("main".into()),
                rev: None
            }
        );
        assert_eq!(p("path:../pkgs").url, InputUrl::Path("../pkgs".into()));
        assert_eq!(
            p("https://x.org/a.tar.gz").url,
            InputUrl::Archive("https://x.org/a.tar.gz".into())
        );
        for bad in ["github:a", "nixpkgs", "path:x?color=red", "github:/b"] {
            assert!(Input::parse(bad).is_err(), "{bad}");
        }
    }
}
