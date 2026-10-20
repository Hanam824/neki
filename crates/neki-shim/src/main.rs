//! Launcher placed in profile and environment `bin` directories, the same on
//! every OS. It reads the `.shim` sidecar next to itself (see
//! [`neki_shim::Shim`]) and runs the target with the same arguments,
//! forwarding its exit code.

use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(never) => match never {},
        Err(message) => {
            eprintln!("neki-shim: {message}");
            ExitCode::from(127)
        }
    }
}

fn run() -> Result<std::convert::Infallible, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find myself: {e}"))?;
    let sidecar = neki_shim::shim_sidecar(&exe);
    let text =
        std::fs::read_to_string(&sidecar).map_err(|e| format!("{}: {e}", sidecar.display()))?;
    let shim = neki_shim::Shim::parse(&text)
        .ok_or_else(|| format!("{}: not a valid shim file", sidecar.display()))?;
    let mut cmd = Command::new(&shim.target);
    cmd.args(std::env::args_os().skip(1));
    cmd.envs(shim.env.iter().map(|(k, v)| (k, v)));
    if !shim.path.is_empty() {
        let current = std::env::var_os("PATH").unwrap_or_default();
        let dirs = shim
            .path
            .iter()
            .cloned()
            .chain(std::env::split_paths(&current));
        let joined = std::env::join_paths(dirs).map_err(|e| format!("PATH: {e}"))?;
        cmd.env("PATH", joined);
    }
    let err = neki_platform::process::exec_replace(cmd);
    Err(format!("cannot run {}: {err}", shim.target.display()))
}
