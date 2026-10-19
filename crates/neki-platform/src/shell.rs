//! The user's interactive shell, used by `neki develop` and `neki shell`.

use std::path::PathBuf;

/// Shell syntax families Neki can generate activation scripts for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// sh, bash, zsh and other POSIX shells.
    Posix,
    /// PowerShell (Windows PowerShell or cross-platform `pwsh`).
    PowerShell,
}

impl Shell {
    /// The shell to launch: `$SHELL` on Unix, PowerShell on Windows.
    pub fn detect() -> (Shell, PathBuf) {
        if cfg!(windows) {
            (Shell::PowerShell, PathBuf::from("powershell.exe"))
        } else {
            let path = std::env::var_os("SHELL").map_or_else(|| "/bin/sh".into(), PathBuf::from);
            (Shell::Posix, path)
        }
    }
}
