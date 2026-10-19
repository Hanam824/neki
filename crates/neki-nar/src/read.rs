use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read};
use std::path::Path;

use crate::{Error, MAGIC, MAX_NAME, Result, at, check_name};

/// Unpack a NAR from `input` into `dest`, which must not exist. The archive
/// is validated as it is read: entries must be sorted, unique and portable,
/// and nothing may follow the root node.
pub fn restore(input: &mut impl Read, dest: &Path) -> Result<()> {
    let mut reader = Reader(input);
    reader.expect(MAGIC)?;
    restore_node(&mut reader, dest)?;
    let mut extra = [0u8; 1];
    if reader.0.read(&mut extra)? != 0 {
        return Err(Error::Invalid("data after the end of the archive".into()));
    }
    Ok(())
}

fn restore_node(r: &mut Reader<impl Read>, path: &Path) -> Result<()> {
    r.expect(b"(")?;
    r.expect(b"type")?;
    match r.string(MAX_NAME)?.as_slice() {
        b"regular" => {
            let mut tag = r.string(MAX_NAME)?;
            let executable = tag == b"executable";
            if executable {
                r.expect(b"")?;
                tag = r.string(MAX_NAME)?;
            }
            if tag != b"contents" {
                return Err(unexpected("contents", &tag));
            }
            let len = r.u64()?;
            let mut file = File::create_new(path).map_err(at(path))?;
            let copied = io::copy(&mut (&mut *r.0).take(len), &mut file).map_err(at(path))?;
            if copied != len {
                return Err(truncated());
            }
            r.padding(len)?;
            if executable {
                neki_platform::fs::set_executable(path).map_err(at(path))?;
            }
        }
        b"symlink" => {
            r.expect(b"target")?;
            let target = String::from_utf8(r.string(MAX_NAME)?)
                .map_err(|_| Error::NonUtf8(path.to_path_buf()))?;
            neki_platform::fs::symlink(Path::new(&target), path).map_err(at(path))?;
        }
        b"directory" => {
            fs::create_dir(path).map_err(at(path))?;
            let mut previous: Option<String> = None;
            let mut seen = HashSet::new();
            loop {
                match r.string(MAX_NAME)?.as_slice() {
                    // The directory's ")" also closes its node.
                    b")" => return Ok(()),
                    b"entry" => {}
                    other => return Err(unexpected("entry", other)),
                }
                r.expect(b"(")?;
                r.expect(b"name")?;
                let name = String::from_utf8(r.string(MAX_NAME)?)
                    .map_err(|_| Error::NonUtf8(path.to_path_buf()))?;
                // Checked before anything is created, so a bad name never touches the filesystem.
                check_name(&name, &mut seen)?;
                if previous.as_ref().is_some_and(|previous| name <= *previous) {
                    return Err(Error::Invalid(format!(
                        "entry {name:?} is out of order or duplicated"
                    )));
                }
                r.expect(b"node")?;
                restore_node(r, &path.join(&name))?;
                r.expect(b")")?;
                previous = Some(name);
            }
        }
        other => return Err(unexpected("regular, symlink or directory", other)),
    }
    r.expect(b")")
}

fn truncated() -> Error {
    Error::Invalid("unexpected end of archive".into())
}

fn unexpected(expected: &str, got: &[u8]) -> Error {
    Error::Invalid(format!(
        "expected {expected}, got {:?}",
        String::from_utf8_lossy(got)
    ))
}

struct Reader<'a, R: Read>(&'a mut R);

impl<R: Read> Reader<'_, R> {
    fn exact(&mut self, buf: &mut [u8]) -> Result<()> {
        self.0.read_exact(buf).map_err(|err| match err.kind() {
            ErrorKind::UnexpectedEof => truncated(),
            _ => Error::Stream(err),
        })
    }

    fn u64(&mut self) -> Result<u64> {
        let mut buf = [0; 8];
        self.exact(&mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn padding(&mut self, len: u64) -> Result<()> {
        let mut buf = [0; 8];
        let padding = &mut buf[..((8 - len % 8) % 8) as usize];
        self.exact(padding)?;
        if padding.iter().any(|&b| b != 0) {
            return Err(Error::Invalid("non-zero padding".into()));
        }
        Ok(())
    }

    fn string(&mut self, max: u64) -> Result<Vec<u8>> {
        let len = self.u64()?;
        if len > max {
            return Err(Error::Invalid(format!("string of {len} bytes is too long")));
        }
        let mut buf = vec![0; len as usize];
        self.exact(&mut buf)?;
        self.padding(len)?;
        Ok(buf)
    }

    fn expect(&mut self, expected: &[u8]) -> Result<()> {
        let got = self.string(MAX_NAME)?;
        if got != expected {
            return Err(unexpected(&String::from_utf8_lossy(expected), &got));
        }
        Ok(())
    }
}
