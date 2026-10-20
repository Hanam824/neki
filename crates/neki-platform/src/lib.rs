//! OS abstraction layer. Every OS-specific call in Neki goes through this
//! crate, so the rest of the workspace behaves the same on Windows, macOS and
//! Linux. A function belongs here only if it can offer identical behaviour on
//! all three; anything else is out of scope (see `docs/PLAN.md`, Future).

pub mod fs;
pub mod paths;
pub mod process;
pub mod shell;
