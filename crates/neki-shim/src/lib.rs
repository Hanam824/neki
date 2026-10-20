//! The format of the `.shim` sidecars that the `neki-shim` launcher reads.
//!
//! A shim is a copy (hard link) of the `neki-shim` program placed in an
//! environment's `bin` under a program's name. It reads the sidecar next to
//! itself and runs the target with the same arguments, so the target keeps
//! finding the files next to it (DLLs on Windows) wherever the environment
//! is on `PATH`. The same launcher is used on every OS.

use std::path::{Path, PathBuf};

/// What a shim runs, as written in its `.shim` sidecar:
///
/// ```text
/// target = <program to run>
/// path = <directory put in front of PATH>     (any number)
/// env.<NAME> = <value>                        (any number)
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shim {
    pub target: PathBuf,
    pub path: Vec<PathBuf>,
    pub env: Vec<(String, String)>,
}

impl Shim {
    pub fn to_text(&self) -> String {
        let mut out = format!("target = {}\n", self.target.display());
        for dir in &self.path {
            out.push_str(&format!("path = {}\n", dir.display()));
        }
        for (name, value) in &self.env {
            out.push_str(&format!("env.{name} = {value}\n"));
        }
        out
    }

    pub fn parse(text: &str) -> Option<Shim> {
        let mut shim = Shim::default();
        let mut target = None;
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once(" = ")?;
            match key {
                "target" => target = Some(PathBuf::from(value)),
                "path" => shim.path.push(PathBuf::from(value)),
                _ => shim
                    .env
                    .push((key.strip_prefix("env.")?.to_string(), value.to_string())),
            }
        }
        shim.target = target?;
        Some(shim)
    }
}

/// The sidecar that the shim at `exe` reads: its file name without a
/// trailing `.exe`, plus `.shim` (`node.exe` → `node.shim`, `node` →
/// `node.shim`), next to it.
pub fn shim_sidecar(exe: &Path) -> PathBuf {
    let name = exe.file_name().unwrap_or_default().to_string_lossy();
    let stem = match name.len().checked_sub(4) {
        Some(i) if name.is_char_boundary(i) && name[i..].eq_ignore_ascii_case(".exe") => &name[..i],
        _ => &name[..],
    };
    exe.with_file_name(format!("{stem}.shim"))
}
