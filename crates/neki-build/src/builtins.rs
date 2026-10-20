//! The `builtin:unpack` and `builtin:assemble` builders.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{Error, Result};

fn invalid(reason: impl Into<String>) -> Error {
    Error::Builder(reason.into())
}

fn at(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::File {
        path: path.to_path_buf(),
        source,
    }
}

pub fn unpack(env: &BTreeMap<String, String>, out: &Path, build_dir: &Path) -> Result<()> {
    let src = Path::new(env.get("src").ok_or_else(|| invalid("no `src`"))?);
    if let Some(install) = env.get("install") {
        let dest = out.join(install);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(at(parent))?;
        }
        fs::copy(src, &dest).map_err(at(&dest))?;
        neki_platform::fs::set_readonly(&dest, false).map_err(at(&dest))?;
        neki_platform::fs::set_executable(&dest).map_err(at(&dest))?;
        let dir = Path::new(install).parent().unwrap_or(Path::new(""));
        let dir = if dir.as_os_str().is_empty() {
            "."
        } else {
            dir.to_str().unwrap_or(".")
        };
        return write_bin_dirs(out, &[dir.to_string()]);
    }

    let tree = build_dir.join("unpacked");
    neki_fetch::unpack(src, &tree, false)?;
    let root = match env.get("stripRoot").map(String::as_str) {
        None | Some("0") => tree.clone(),
        Some("1") => {
            single_dir(&tree)?.ok_or_else(|| invalid("expected a single top-level directory"))?
        }
        Some("auto") => single_dir(&tree)?.unwrap_or_else(|| tree.clone()),
        Some(other) => {
            return Err(invalid(format!(
                "stripRoot must be 0, 1 or auto, not {other:?}"
            )));
        }
    };
    match env.get("subdirs") {
        None => fs::rename(&root, out).map_err(at(out))?,
        Some(subdirs) => {
            fs::create_dir(out).map_err(at(out))?;
            for dir in subdirs.split_whitespace() {
                let from = root.join(dir);
                if !from.is_dir() {
                    return Err(invalid(format!("the archive has no directory {dir}")));
                }
                merge(&from, out)?;
            }
        }
    }
    let dirs = match env.get("binDirs") {
        Some(dirs) => dirs.split_whitespace().map(String::from).collect(),
        None => find_bin_dirs(out)?,
    };
    write_bin_dirs(out, &dirs)
}

/// The only entry of `dir`, if it is a directory.
fn single_dir(dir: &Path) -> Result<Option<PathBuf>> {
    let entries = fs::read_dir(dir)
        .map_err(at(dir))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(at(dir))?;
    Ok(match entries.as_slice() {
        [only] if only.file_type().is_ok_and(|t| t.is_dir()) => Some(only.path()),
        _ => None,
    })
}

fn has_programs(dir: &Path) -> bool {
    fs::read_dir(dir).into_iter().flatten().flatten().any(|e| {
        let path = e.path();
        fs::metadata(&path).is_ok_and(|m| neki_platform::fs::is_program(&path, &m))
    })
}

/// `bin` if it exists, else the top level if it has programs, else `bin`
/// directories one level down.
fn find_bin_dirs(out: &Path) -> Result<Vec<String>> {
    if out.join("bin").is_dir() {
        return Ok(vec!["bin".into()]);
    }
    if has_programs(out) {
        return Ok(vec![".".into()]);
    }
    let mut found = Vec::new();
    for entry in fs::read_dir(out).map_err(at(out))? {
        let entry = entry.map_err(at(out))?;
        if entry.path().join("bin").is_dir() {
            found.push(format!("{}/bin", entry.file_name().to_string_lossy()));
        }
    }
    found.sort();
    Ok(found)
}

fn write_bin_dirs(out: &Path, dirs: &[String]) -> Result<()> {
    if !out.is_dir() {
        return Ok(());
    }
    let file = out.join(neki_store::BIN_DIRS_FILE);
    let parent = file.parent().expect("has a parent");
    fs::create_dir_all(parent).map_err(at(parent))?;
    let mut text = dirs.join("\n");
    text.push('\n');
    fs::write(&file, text).map_err(at(&file))
}

/// Copy the tree `from` into `to`, merging directories; an existing file
/// wins over a later one.
fn merge(from: &Path, to: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(from).map_err(at(from))?;
    if meta.file_type().is_symlink() {
        if fs::symlink_metadata(to).is_err() {
            let target = fs::read_link(from).map_err(at(from))?;
            neki_platform::fs::symlink(&target, to).map_err(at(to))?;
        }
    } else if meta.is_dir() {
        fs::create_dir_all(to).map_err(at(to))?;
        for entry in fs::read_dir(from).map_err(at(from))? {
            let entry = entry.map_err(at(from))?;
            merge(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else if fs::symlink_metadata(to).is_err() {
        fs::copy(from, to).map_err(at(to))?;
        // Store files are read-only; the copy must stay writable until the
        // output is registered.
        neki_platform::fs::set_readonly(to, false).map_err(at(to))?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    src: Option<String>,
    text: Option<String>,
    #[serde(default)]
    executable: bool,
}

pub fn assemble(env: &BTreeMap<String, String>, out: &Path) -> Result<()> {
    let entries: Vec<Entry> = serde_json::from_str(env.get("entries").map_or("[]", String::as_str))
        .map_err(|e| invalid(format!("`entries`: {e}")))?;
    fs::create_dir(out).map_err(at(out))?;
    for entry in entries {
        let rel = Path::new(&entry.path);
        if rel.components().any(|c| {
            !matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        }) {
            return Err(invalid(format!(
                "entry path {:?} must be relative and inside $out",
                entry.path
            )));
        }
        let dest = out.join(rel);
        match (entry.src, entry.text) {
            (Some(src), None) => merge(Path::new(&src), &dest)?,
            (None, Some(text)) => {
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(at(parent))?;
                }
                fs::write(&dest, text).map_err(at(&dest))?;
                if entry.executable {
                    neki_platform::fs::set_executable(&dest).map_err(at(&dest))?;
                }
            }
            _ => {
                return Err(invalid(format!(
                    "entry {:?} needs exactly one of `src` and `text`",
                    entry.path
                )));
            }
        }
    }
    Ok(())
}
