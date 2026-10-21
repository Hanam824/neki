//! Merged environments: many packages in one store path.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use neki_hash::StorePath;
use neki_shim::{Shim, shim_sidecar};
use neki_store::{LocalStore, Store};

use crate::{Result, at};

/// Merge `packages` into one store path named `name`. Programs in each
/// package's program directories (`bin`, or as listed in its
/// `neki-support/bin-dirs`) become shims in the environment's `bin`: hard
/// links to the `shim` executable in the store, whose sidecars run the
/// original with its directory in front of `PATH`, so programs keep finding
/// the files (DLLs, siblings) next to them. Every other file is hard-linked.
/// When two packages have the same file, the earlier package wins.
pub fn build_env(
    store: &LocalStore,
    name: &str,
    packages: &[StorePath],
    shim: &StorePath,
) -> Result<StorePath> {
    let shim_exe = store.store_dir().to_path(shim);
    let tmp = store.temp_entry();
    fs::create_dir(tmp.path()).map_err(at(tmp.path()))?;
    for package in packages {
        let root = store.store_dir().to_path(package);
        if root.is_dir() {
            let bin = tmp.path().join("bin");
            fs::create_dir_all(&bin).map_err(at(&bin))?;
            for dir in neki_store::bin_dirs(&root) {
                shims(&dir, &bin, &shim_exe)?;
            }
            merge(&root, tmp.path())?;
        }
    }
    // The merged files are the packages' own, so they can mention anything
    // in the packages' closures.
    let mut candidates: BTreeSet<StorePath> = store.closure(packages)?;
    candidates.insert(shim.clone());
    Ok(store.add_tree(name, tmp, &candidates)?)
}

/// Shims in `bin` for the programs directly in `dir`, each running the
/// original with `dir` in front of `PATH`. An existing shim (from an
/// earlier package) wins.
fn shims(dir: &Path, bin: &Path, shim_exe: &Path) -> Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry.map_err(at(dir))?;
        let from = entry.path();
        let meta = fs::metadata(&from).map_err(at(&from))?;
        if !neki_platform::fs::is_program(&from, &meta) {
            continue;
        }
        let to = bin.join(entry.file_name());
        let shim_name = if cfg!(windows) {
            to.with_extension("exe")
        } else {
            to
        };
        if fs::symlink_metadata(&shim_name).is_ok() {
            continue;
        }
        link_or_copy(shim_exe, &shim_name)?;
        let shim = Shim {
            target: from.clone(),
            path: vec![dir.to_path_buf()],
            env: Vec::new(),
        };
        let sidecar = shim_sidecar(&shim_name);
        fs::write(&sidecar, shim.to_text()).map_err(at(&sidecar))?;
    }
    Ok(())
}

/// Merge the package tree `src` into `dest` with hard links, leaving out
/// `bin` (shims are made for it) and `neki-support`. Existing files win.
fn merge(src: &Path, dest: &Path) -> Result<()> {
    for entry in fs::read_dir(src).map_err(at(src))? {
        let entry = entry.map_err(at(src))?;
        let name = entry.file_name();
        if dest.parent().is_some() && (name == "neki-support" || name == "bin") && is_root(src) {
            continue;
        }
        merge_entry(&entry.path(), &dest.join(&name))?;
    }
    Ok(())
}

/// Whether `dir` is a package root (a store path), not a directory in one.
fn is_root(dir: &Path) -> bool {
    dir.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| neki_hash::StorePath::parse(n).is_ok())
}

fn merge_entry(from: &Path, to: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(from).map_err(at(from))?;
    if meta.is_dir() {
        fs::create_dir_all(to).map_err(at(to))?;
        for entry in fs::read_dir(from).map_err(at(from))? {
            let entry = entry.map_err(at(from))?;
            merge_entry(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else if fs::symlink_metadata(to).is_err() {
        if meta.file_type().is_symlink() {
            let target = fs::read_link(from).map_err(at(from))?;
            neki_platform::fs::symlink(&target, to).map_err(at(to))?;
        } else {
            link_or_copy(from, to)?;
        }
    }
    Ok(())
}

/// Hard-link `from` to `to`, or copy it where hard links aren't possible.
fn link_or_copy(from: &Path, to: &Path) -> Result<()> {
    if neki_platform::fs::hard_link(from, to).is_err() {
        fs::copy(from, to).map_err(at(to))?;
    }
    Ok(())
}
