//! Unpacking zip and tar archives (optionally gzip- or xz-compressed).

use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek};
use std::path::{Component, Path, PathBuf};

use crate::{Error, Result, at};

/// Unpack `archive` into the new directory `dest`. The format is detected
/// from the file's first bytes. With `strip_root`, the archive must hold a
/// single top-level directory, whose contents become `dest`.
///
/// An entry whose path differs only by case from an earlier entry's (say
/// `terminfo/2/2621A` after `terminfo/2/2621a`) would collide on Windows
/// and macOS, so it is left out; the paths left out are returned.
pub fn unpack(archive: &Path, dest: &Path, strip_root: bool) -> Result<Vec<String>> {
    let fail = |reason: String| Error::Unpack {
        archive: archive.to_path_buf(),
        reason,
    };
    let staging = sibling(dest, "unpack");
    let _ = neki_platform::fs::remove_tree(&staging);
    fs::create_dir(&staging).map_err(at(&staging))?;
    let mut names = Names::default();
    let result = unpack_into(archive, &staging, &mut names).map_err(|err| match err {
        Unpacking::Io(err) => fail(err.to_string()),
        Unpacking::Bad(reason) => fail(reason),
    });
    let result = result.and_then(|()| {
        let root = if strip_root {
            let mut entries = fs::read_dir(&staging)
                .map_err(at(&staging))?
                .collect::<io::Result<Vec<_>>>()
                .map_err(at(&staging))?;
            match (entries.pop(), entries.is_empty()) {
                (Some(entry), true) if entry.file_type().is_ok_and(|t| t.is_dir()) => entry.path(),
                _ => return Err(fail("expected a single top-level directory".into())),
            }
        } else {
            staging.clone()
        };
        fs::rename(&root, dest).map_err(at(dest))
    });
    let _ = neki_platform::fs::remove_tree(&staging);
    result.map(|()| names.skipped)
}

/// The entry paths seen so far, to leave out ones that differ from an
/// earlier one only by case.
#[derive(Default)]
struct Names {
    /// Every path and parent path seen, lowercased → as spelled.
    seen: std::collections::HashMap<String, String>,
    skipped: Vec<String>,
}

impl Names {
    /// Whether to unpack `name`; false (and recorded) if it or a parent
    /// differs only by case from an earlier entry's.
    fn accept(&mut self, name: &Path) -> bool {
        let mut prefix = String::new();
        for component in name.components() {
            if let Component::Normal(part) = component {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(&part.to_string_lossy());
                match self.seen.get(&prefix.to_lowercase()) {
                    Some(spelled) if *spelled != prefix => {
                        self.skipped.push(name.display().to_string());
                        return false;
                    }
                    Some(_) => {}
                    None => {
                        self.seen.insert(prefix.to_lowercase(), prefix.clone());
                    }
                }
            }
        }
        true
    }
}

enum Unpacking {
    Io(io::Error),
    Bad(String),
}

impl From<io::Error> for Unpacking {
    fn from(err: io::Error) -> Self {
        Unpacking::Io(err)
    }
}

fn unpack_into(archive: &Path, dest: &Path, names: &mut Names) -> Result<(), Unpacking> {
    let mut file = File::open(archive)?;
    let mut magic = [0u8; 6];
    let n = read_up_to(&mut file, &mut magic)?;
    file.rewind()?;
    let magic = &magic[..n];
    if magic.starts_with(b"PK\x03\x04") || magic.starts_with(b"PK\x05\x06") {
        unzip(file, dest, names)
    } else if magic.starts_with(&[0x1f, 0x8b]) {
        untar(
            flate2::read::MultiGzDecoder::new(BufReader::new(file)),
            dest,
            names,
        )
    } else if magic.starts_with(b"\xfd7zXZ\0") {
        // lzma-rs only decompresses whole streams, so go through a file.
        let tar = sibling(dest, "tar");
        let result = (|| {
            let mut out = io::BufWriter::new(File::create_new(&tar)?);
            lzma_rs::xz_decompress(&mut BufReader::new(file), &mut out)
                .map_err(|err| Unpacking::Bad(format!("xz: {err}")))?;
            drop(out);
            untar(BufReader::new(File::open(&tar)?), dest, names)
        })();
        let _ = fs::remove_file(&tar);
        result
    } else {
        untar(BufReader::new(file), dest, names)
    }
}

fn read_up_to(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// `<dest>.<suffix>`, a scratch location next to `dest`.
fn sibling(dest: &Path, suffix: &str) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{suffix}"));
    dest.with_file_name(name)
}

/// Check an entry path from an archive and return where it goes under
/// `dest`: only plain, portable components, and no component may be a
/// symlink created by an earlier entry (which could point outside `dest`).
fn entry_path(dest: &Path, name: &Path) -> Result<PathBuf, Unpacking> {
    let bad = |why: &str| Unpacking::Bad(format!("entry {}: {why}", name.display()));
    let mut out = dest.to_path_buf();
    let mut components = name.components().peekable();
    while let Some(component) = components.next() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| bad("name is not UTF-8"))?;
                if !neki_platform::fs::is_portable_file_name(part) {
                    return Err(bad("name is not valid on every supported OS"));
                }
                out.push(part);
                if components.peek().is_some()
                    && fs::symlink_metadata(&out).is_ok_and(|m| m.file_type().is_symlink())
                {
                    return Err(bad("goes through a symlink"));
                }
            }
            Component::CurDir => {}
            _ => return Err(bad("must be a relative path inside the archive")),
        }
    }
    Ok(out)
}

fn prepare(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // A later entry with the same name replaces the earlier one.
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(_) => Ok(()),
    }
}

fn untar(reader: impl Read, dest: &Path, names: &mut Names) -> Result<(), Unpacking> {
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if matches!(
            kind,
            tar::EntryType::XGlobalHeader
                | tar::EntryType::XHeader
                | tar::EntryType::GNULongName
                | tar::EntryType::GNULongLink
        ) {
            continue;
        }
        let name = entry.path()?.into_owned();
        if name.as_os_str().is_empty() || name == Path::new(".") || name == Path::new("./") {
            continue;
        }
        let path = entry_path(dest, &name)?;
        if !names.accept(&name) {
            continue;
        }
        match kind {
            tar::EntryType::Directory => fs::create_dir_all(&path)?,
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                prepare(&path)?;
                io::copy(&mut entry, &mut File::create_new(&path)?)?;
                if entry.header().mode()? & 0o111 != 0 {
                    neki_platform::fs::set_executable(&path)?;
                }
            }
            tar::EntryType::Symlink => {
                let target = entry.link_name()?.ok_or_else(|| {
                    Unpacking::Bad(format!("symlink {} has no target", name.display()))
                })?;
                prepare(&path)?;
                neki_platform::fs::symlink(&target, &path)?;
            }
            tar::EntryType::Link => {
                let target = entry.link_name()?.ok_or_else(|| {
                    Unpacking::Bad(format!("hard link {} has no target", name.display()))
                })?;
                let source = entry_path(dest, &target)?;
                prepare(&path)?;
                fs::copy(&source, &path)?;
            }
            other => {
                return Err(Unpacking::Bad(format!(
                    "entry {}: unsupported type {other:?}",
                    name.display()
                )));
            }
        }
    }
    Ok(())
}

fn unzip(file: File, dest: &Path, names: &mut Names) -> Result<(), Unpacking> {
    let mut archive = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|err| Unpacking::Bad(err.to_string()))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|err| Unpacking::Bad(err.to_string()))?;
        let name = PathBuf::from(
            entry
                .name()
                .map_err(|err| Unpacking::Bad(err.to_string()))?
                .into_owned(),
        );
        let path = entry_path(dest, &name)?;
        if !names.accept(&name) {
            continue;
        }
        let mode = entry.unix_mode().unwrap_or(0);
        if entry.is_dir() {
            fs::create_dir_all(&path)?;
        } else if mode & 0o170000 == 0o120000 {
            let mut target = String::new();
            entry.read_to_string(&mut target)?;
            prepare(&path)?;
            neki_platform::fs::symlink(Path::new(&target), &path)?;
        } else {
            prepare(&path)?;
            io::copy(&mut entry, &mut File::create_new(&path)?)?;
            if mode & 0o111 != 0 {
                neki_platform::fs::set_executable(&path)?;
            }
        }
    }
    Ok(())
}
