use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use neki_platform::shell::Shell;

/// Environment of a dev shell or profile.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShellEnv {
    pub vars: BTreeMap<String, String>,
    /// Directories put in front of the existing `PATH`.
    pub path_prepend: Vec<PathBuf>,
    /// Commands run when an interactive shell starts.
    pub hook: Option<String>,
}

impl ShellEnv {
    /// Apply this environment on top of the current one for a child process.
    pub fn apply_to(&self, cmd: &mut Command) {
        cmd.envs(&self.vars);
        let current = std::env::var_os("PATH").unwrap_or_default();
        let paths = self
            .path_prepend
            .iter()
            .cloned()
            .chain(std::env::split_paths(&current));
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }
}

/// Script that applies `env` in the current session of `shell`, e.g. for
/// `eval "$(neki develop --print-env posix)"` or
/// `neki develop --print-env powershell | Invoke-Expression`, or, in
/// Nushell, `neki develop --print-env nushell | save -f env.nu; source env.nu`.
pub fn render_activation(env: &ShellEnv, shell: Shell) -> String {
    let mut out = String::new();
    match shell {
        Shell::Posix => {
            let quote = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
            for (name, value) in &env.vars {
                out.push_str(&format!("export {name}={}\n", quote(value)));
            }
            if !env.path_prepend.is_empty() {
                let sep = if cfg!(windows) { ";" } else { ":" };
                let dirs: Vec<String> = env
                    .path_prepend
                    .iter()
                    .map(|p| quote(&p.display().to_string()))
                    .collect();
                out.push_str(&format!("export PATH={}{sep}\"$PATH\"\n", dirs.join(sep)));
            }
        }
        Shell::PowerShell => {
            let quote = |s: &str| format!("'{}'", s.replace('\'', "''"));
            for (name, value) in &env.vars {
                out.push_str(&format!("$env:{name} = {}\n", quote(value)));
            }
            if !env.path_prepend.is_empty() {
                let mut parts: Vec<String> = env
                    .path_prepend
                    .iter()
                    .map(|p| quote(&p.display().to_string()))
                    .collect();
                parts.push("$env:PATH".into());
                out.push_str(&format!(
                    "$env:PATH = {}\n",
                    parts.join(" + [IO.Path]::PathSeparator + ")
                ));
            }
        }
        Shell::Nushell => {
            for (name, value) in &env.vars {
                out.push_str(&format!("$env.{name} = {}\n", nu_quote(value)));
            }
            if !env.path_prepend.is_empty() {
                let dirs: Vec<String> = env
                    .path_prepend
                    .iter()
                    .map(|p| nu_quote(&p.display().to_string()))
                    .collect();
                // `PATH` is a list in an interactive Nushell and a string
                // before its conversion runs; `split row` handles both.
                out.push_str(&format!(
                    "$env.PATH = ($env.PATH | split row (char esep) | prepend [{}])\n",
                    dirs.join(" ")
                ));
            }
        }
    }
    if let Some(hook) = &env.hook {
        out.push_str(hook);
        if !hook.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// `s` as a Nushell raw string (`r#'…'#`), which has no escapes.
fn nu_quote(s: &str) -> String {
    let mut hashes = "#".to_string();
    while s.contains(&format!("'{hashes}")) {
        hashes.push('#');
    }
    format!("r{hashes}'{s}'{hashes}")
}

/// The command that starts `program` (a `shell`) interactively, running
/// `hook` first.
pub fn shell_command(shell: Shell, program: &Path, hook: Option<&str>) -> Command {
    let mut cmd = Command::new(program);
    match (shell, hook) {
        (_, None) => {}
        (Shell::Posix, Some(hook)) => {
            // Run the hook, then replace the shell with an interactive one.
            cmd.arg("-c")
                .arg(format!("{hook}\nexec \"$0\""))
                .arg(program);
        }
        (Shell::PowerShell, Some(hook)) => {
            cmd.args(["-NoExit", "-Command", hook]);
        }
        (Shell::Nushell, Some(hook)) => {
            // Runs the commands, then stays interactive.
            cmd.args(["--execute", hook]);
        }
    }
    cmd
}
