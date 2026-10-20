//! The user's interactive shell, used by `neki develop` and `neki shell`.

use std::path::PathBuf;

/// Shell syntax families Neki can generate activation scripts for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// sh, bash, zsh and other POSIX shells.
    Posix,
    /// PowerShell (Windows PowerShell or cross-platform `pwsh`).
    PowerShell,
    /// Nushell (`nu`), which runs on every OS.
    Nushell,
}

impl Shell {
    /// The shell to launch: Nushell if `$SHELL` names `nu` (on any OS),
    /// otherwise `$SHELL` on Unix and PowerShell on Windows.
    pub fn detect() -> (Shell, PathBuf) {
        if let Some(path) = std::env::var_os("SHELL").map(PathBuf::from)
            && path.file_stem().is_some_and(|stem| stem == "nu")
        {
            (Shell::Nushell, path)
        } else if cfg!(windows) {
            (Shell::PowerShell, PathBuf::from("powershell.exe"))
        } else {
            let path = std::env::var_os("SHELL").map_or_else(|| "/bin/sh".into(), PathBuf::from);
            (Shell::Posix, path)
        }
    }
}

/// Open a new terminal window running an interactive shell after `script`:
/// a POSIX `sh` script on macOS and Linux, a PowerShell script on Windows
/// (pass both; the one for this OS is used). The script is written to a
/// new file in `dir`. On macOS this uses Terminal; on Linux the first of
/// `x-terminal-emulator`, `gnome-terminal`, `konsole` and `xterm` found;
/// on Windows a PowerShell console.
pub fn open_terminal(dir: &std::path::Path, posix: &str, powershell: &str) -> std::io::Result<()> {
    use std::process::Command;
    std::fs::create_dir_all(dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    if cfg!(windows) {
        let file = dir.join(format!("neki-shell-{nanos}.ps1"));
        std::fs::write(&file, powershell)?;
        // `start` opens a new console window.
        Command::new("cmd")
            .args([
                "/c",
                "start",
                "Neki shell",
                "powershell",
                "-NoExit",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&file)
            .spawn()?;
        return Ok(());
    }
    let file = dir.join(format!("neki-shell-{nanos}.command"));
    let user_shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    std::fs::write(
        &file,
        format!("#!/bin/sh\n{posix}\nexec \"{user_shell}\" -i\n"),
    )?;
    crate::fs::set_executable(&file)?;
    if cfg!(target_os = "macos") {
        Command::new("open")
            .args(["-a", "Terminal"])
            .arg(&file)
            .spawn()?;
        return Ok(());
    }
    for terminal in ["x-terminal-emulator", "gnome-terminal", "konsole", "xterm"] {
        let mut cmd = Command::new(terminal);
        if terminal == "gnome-terminal" {
            cmd.arg("--");
        } else {
            cmd.arg("-e");
        }
        if cmd.arg(&file).spawn().is_ok() {
            return Ok(());
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no terminal program found (tried x-terminal-emulator, gnome-terminal, konsole, xterm)",
    ))
}
