//! Launcher placed in profile and environment `bin` directories, the same on
//! every OS. It reads `<own file stem>.shim` next to itself — the target
//! executable plus extra environment variables and `PATH` entries — and runs
//! the target with the same arguments, forwarding its exit code.

fn main() {
    todo!("M5: read .shim sidecar and run target")
}
