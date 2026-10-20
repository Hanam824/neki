//! Process trees for builders, so a build and everything it started can be
//! stopped together.

use std::io;
use std::process::{ChildStderr, ChildStdout, Command, ExitStatus};

use process_wrap::std::{ChildWrapper, CommandWrap};

/// A child process in its own process group (Unix) or job object (Windows),
/// together with every process it starts.
#[derive(Debug)]
pub struct ProcessTree(Box<dyn ChildWrapper>);

/// Spawn `cmd` as the root of a new [`ProcessTree`].
pub fn spawn_tree(cmd: Command) -> io::Result<ProcessTree> {
    let mut cmd = CommandWrap::from(cmd);
    #[cfg(unix)]
    cmd.wrap(process_wrap::std::ProcessGroup::leader());
    #[cfg(windows)]
    cmd.wrap(process_wrap::std::JobObject);
    cmd.spawn().map(ProcessTree)
}

impl ProcessTree {
    pub fn id(&self) -> u32 {
        self.0.id()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.0.stdout().take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.0.stderr().take()
    }

    /// Wait for the root process to exit, then kill whatever it left
    /// running, so a stray background process can't outlive the build.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.0.inner_mut().wait()?;
        self.kill();
        Ok(status)
    }

    /// Whether the root process has exited, without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.0.inner_mut().try_wait()
    }

    /// Kill every process in the tree and reap them. Processes that already
    /// exited are fine, so this never fails.
    pub fn kill(&mut self) {
        let _ = self.0.start_kill();
        let _ = self.0.wait();
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        if !matches!(self.0.inner_mut().try_wait(), Ok(Some(_))) {
            self.kill();
        }
    }
}

/// Environment variables a process needs just to start on this OS, passed
/// to builders whose environment is otherwise scrubbed. Empty on Unix; on
/// Windows, `SystemRoot` and friends, without which many system DLLs fail.
pub fn essential_env() -> Vec<(String, String)> {
    const NAMES: &[&str] = if cfg!(windows) {
        &[
            "SystemRoot",
            "SystemDrive",
            "windir",
            "ComSpec",
            "PATHEXT",
            "NUMBER_OF_PROCESSORS",
            "PROCESSOR_ARCHITECTURE",
        ]
    } else {
        &[]
    };
    NAMES
        .iter()
        .filter_map(|name| Some((name.to_string(), std::env::var(name).ok()?)))
        .collect()
}

/// Run `cmd` in place of the current process: on Unix the process image is
/// replaced, on Windows the command runs as a child and the current process
/// exits with its exit code. Returns only if the command can't be started.
pub fn exec_replace(mut cmd: Command) -> io::Error {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.exec()
    }
    #[cfg(not(unix))]
    {
        match cmd.status() {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(err) => err,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    /// A command that starts a long-running background child and exits.
    fn leaves_background_child() -> Command {
        if cfg!(windows) {
            let mut cmd = Command::new("powershell.exe");
            cmd.args([
                "-NoProfile",
                "-Command",
                "Start-Process -NoNewWindow ping -ArgumentList '-n','30','127.0.0.1'; exit 3",
            ]);
            cmd
        } else {
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", "sleep 30 & exit 3"]);
            cmd
        }
    }

    #[test]
    fn wait_returns_when_root_exits_and_kills_the_rest() {
        let mut cmd = leaves_background_child();
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        let start = Instant::now();
        let mut tree = spawn_tree(cmd).unwrap();
        assert_eq!(tree.wait().unwrap().code(), Some(3));
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "waited for the background child"
        );
    }
}
