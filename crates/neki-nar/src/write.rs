use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;

use crate::{Error, MAGIC, Result, at, check_name};

/// Serialize the tree at `path` (a file, directory or symlink) as a NAR into `out`.
pub fn dump(path: &Path, out: &mut impl Write) -> Result<()> {
    let mut writer = Writer(out);
    writer.str(MAGIC)?;
    dump_node(path, &mut writer)
}

fn dump_node(path: &Path, w: &mut Writer<impl Write>) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(at(path))?;
    let file_type = meta.file_type();
    w.strs(&[b"(", b"type"])?;
    if file_type.is_symlink() {
        let target = fs::read_link(path).map_err(at(path))?;
        let target = target
            .to_str()
            .ok_or_else(|| Error::NonUtf8(path.to_path_buf()))?;
        w.strs(&[b"symlink", b"target", target.as_bytes()])?;
    } else if file_type.is_dir() {
        w.str(b"directory")?;
        let mut names = Vec::new();
        for entry in fs::read_dir(path).map_err(at(path))? {
            let name = entry.map_err(at(path))?.file_name();
            names.push(
                name.into_string()
                    .map_err(|name| Error::NonUtf8(path.join(name)))?,
            );
        }
        // String order is byte order for UTF-8.
        names.sort();
        let mut seen = HashSet::new();
        for name in &names {
            check_name(name, &mut seen)?;
            w.strs(&[b"entry", b"(", b"name", name.as_bytes(), b"node"])?;
            dump_node(&path.join(name), w)?;
            w.str(b")")?;
        }
    } else if file_type.is_file() {
        w.str(b"regular")?;
        if neki_platform::fs::is_executable(&meta) {
            w.strs(&[b"executable", b""])?;
        }
        w.str(b"contents")?;
        let len = meta.len();
        w.u64(len)?;
        let file = File::open(path).map_err(at(path))?;
        let copied = io::copy(&mut file.take(len), w.0).map_err(at(path))?;
        if copied != len {
            return Err(Error::Invalid(format!(
                "{} changed size while it was archived",
                path.display()
            )));
        }
        w.pad(len)?;
    } else {
        return Err(Error::UnsupportedFileType(path.to_path_buf()));
    }
    w.str(b")")
}

struct Writer<'a, W: Write>(&'a mut W);

impl<W: Write> Writer<'_, W> {
    fn u64(&mut self, n: u64) -> Result<()> {
        Ok(self.0.write_all(&n.to_le_bytes())?)
    }

    fn pad(&mut self, len: u64) -> Result<()> {
        let padding = (8 - len % 8) % 8;
        Ok(self.0.write_all(&[0; 8][..padding as usize])?)
    }

    fn str(&mut self, s: &[u8]) -> Result<()> {
        self.u64(s.len() as u64)?;
        self.0.write_all(s)?;
        self.pad(s.len() as u64)
    }

    fn strs(&mut self, strs: &[&[u8]]) -> Result<()> {
        strs.iter().try_for_each(|s| self.str(s))
    }
}
