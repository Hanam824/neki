use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use neki_platform::shell::Shell;

/// Environment of a dev shell or profile.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShellEnv {
    pub vars: BTreeMap<String, String>,
    /// Directories put in front of the existing `PATH`.
    pub path_prepend: Vec<PathBuf>,
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

/// Script that applies `env` in the current session of `shell`.
pub fn render_activation(env: &ShellEnv, shell: Shell) -> String {
    todo!("M5: POSIX and PowerShell activation scripts")
}
