//! Process groups for builders, so a whole build process tree can be killed.

use std::io;
use std::process::{Child, Command};

/// Spawn `cmd` in its own process group (Unix) or job object (Windows).
pub fn spawn_group(cmd: &mut Command) -> io::Result<Child> {
    todo!("M3: process group / job object")
}

/// Terminate `child` and every process it started.
pub fn kill_tree(child: &mut Child) -> io::Result<()> {
    todo!("M3: kill process group / job object")
}
