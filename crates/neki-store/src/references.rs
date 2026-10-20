//! Finding which store paths a file tree refers to, by scanning it for their
//! hash parts.

use std::collections::{BTreeSet, HashMap};
use std::io::{self, Write};
use std::path::Path;

use neki_hash::{HASH_LEN, StorePath, base32};

use crate::Result;

/// The candidates whose hash part occurs anywhere in the NAR serialization of
/// `path` (file contents, names and symlink targets), encoded as UTF-8 or as
/// UTF-16LE (common in Windows binaries and resources).
pub fn scan_references<'a>(
    path: &Path,
    candidates: impl IntoIterator<Item = &'a StorePath>,
) -> Result<BTreeSet<StorePath>> {
    let mut scanner = Scanner::new(candidates);
    neki_nar::dump(path, &mut io::BufWriter::new(&mut scanner))?;
    Ok(scanner.found)
}

/// Characters in a hash part.
const HASH_CHARS: usize = 32;
const _: () = assert!(HASH_CHARS == base32::encoded_len(HASH_LEN));

const IS_BASE32: [bool; 256] = {
    let mut table = [false; 256];
    let mut i = 0;
    while i < base32::ALPHABET.len() {
        table[base32::ALPHABET[i] as usize] = true;
        i += 1;
    }
    table
};

/// A byte sink that looks for hash parts in what is written to it.
struct Scanner {
    candidates: HashMap<Vec<u8>, StorePath>,
    found: BTreeSet<StorePath>,
    /// The current run of consecutive base32 characters.
    utf8: Vec<u8>,
    /// The current runs of `<base32 char> 0x00` pairs, for pairs starting at
    /// even and at odd offsets.
    utf16: [Vec<u8>; 2],
    prev: Option<u8>,
    offset: usize,
}

impl Scanner {
    fn new<'a>(candidates: impl IntoIterator<Item = &'a StorePath>) -> Self {
        Self {
            candidates: candidates
                .into_iter()
                .map(|path| (path.hash_part().into_bytes(), path.clone()))
                .collect(),
            found: BTreeSet::new(),
            utf8: Vec::new(),
            utf16: [Vec::new(), Vec::new()],
            prev: None,
            offset: 0,
        }
    }

    fn byte(&mut self, byte: u8) {
        if IS_BASE32[byte as usize] {
            extend(&mut self.utf8, byte, &self.candidates, &mut self.found);
        } else {
            self.utf8.clear();
        }
        // The pair that started at the previous byte is complete.
        if let Some(prev) = self.prev {
            let run = &mut self.utf16[(self.offset - 1) % 2];
            if IS_BASE32[prev as usize] && byte == 0 {
                extend(run, prev, &self.candidates, &mut self.found);
            } else {
                run.clear();
            }
        }
        self.prev = Some(byte);
        self.offset += 1;
    }
}

/// Append `c` to `run` and record a candidate if the run now ends with its
/// hash part.
fn extend(
    run: &mut Vec<u8>,
    c: u8,
    candidates: &HashMap<Vec<u8>, StorePath>,
    found: &mut BTreeSet<StorePath>,
) {
    run.push(c);
    if run.len() >= HASH_CHARS {
        if let Some(path) = candidates.get(&run[run.len() - HASH_CHARS..]) {
            found.insert(path.clone());
        }
        // Only the last HASH_CHARS - 1 characters can start a later match.
        if run.len() == 2 * HASH_CHARS {
            run.drain(..HASH_CHARS);
        }
    }
}

impl Write for Scanner {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for &byte in buf {
            self.byte(byte);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> StorePath {
        let hash = neki_hash::Hash::of_bytes(neki_hash::HashAlgo::Sha256, name.as_bytes());
        neki_hash::StoreDir::new("/opt/neki/store")
            .make_path("source", &hash, name)
            .unwrap()
    }

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn finds_utf8_and_utf16_references() {
        let [a, b, c, d] = ["a", "b", "c", "d"].map(path);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("script"), format!("exec /x/{a}/bin/a\n")).unwrap();
        // Odd offset, so the pairs don't line up with the start of the file.
        let mut wide = b"\x01".to_vec();
        wide.extend(utf16(&format!("C:\\x\\{b}\\b.dll")));
        std::fs::write(dir.path().join("wide.bin"), wide).unwrap();
        // A file named after a path.
        std::fs::write(dir.path().join(c.to_string()), "").unwrap();
        // Only part of d's hash.
        std::fs::write(dir.path().join("partial"), &d.hash_part()[..31]).unwrap();

        let found = scan_references(dir.path(), [&a, &b, &c, &d]).unwrap();
        assert_eq!(found, BTreeSet::from([a, b, c]));
    }

    #[test]
    fn finds_hashes_inside_longer_runs() {
        let a = path("a");
        // A hash part preceded by more base32 characters than fit in one run.
        let text = format!("{}{}", "0".repeat(70), a.hash_part());
        let mut scanner = Scanner::new([&a]);
        // Byte by byte, as if split across many writes.
        for byte in text.bytes() {
            scanner.write_all(&[byte]).unwrap();
        }
        assert!(scanner.found.contains(&a));

        let mut scanner = Scanner::new([&a]);
        scanner.write_all(&utf16(&text)).unwrap();
        assert!(scanner.found.contains(&a));
    }
}
