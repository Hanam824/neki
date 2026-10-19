# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**Neki** (CLI binary `neki`, crates `neki-*`) is a reimplementation of Nix's architecture in Rust that runs natively on **Windows** as well as macOS/Linux. It is a new tool, not a Nix port: avoid "Nix" in user-facing names (NixOS Foundation trademark). Phase 1 is the backend + CLI; Phase 2 is a Rust + Slint desktop app (`crates/neki-gui`, not created yet) that drives the backend.

The full design, the list of Windows problems with chosen workarounds (P1–P16), and the milestone roadmap (M0–M9) are in `docs/PLAN.md` — read it before changing core formats or starting a milestone.

## Commands

```sh
cargo build --workspace
cargo test --workspace
cargo test -p neki-nar                 # one crate
cargo test -p neki-nar some_test_name  # one test
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo run -p neki-cli -- <args>        # the `neki` binary
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy `-D warnings` and tests on windows, macos and ubuntu — code must compile on all three.

## Architecture

Cargo workspace; every crate is in `crates/` and inherits version/edition/lints from the root `Cargo.toml` (`unsafe_code = "deny"`: Win32 FFI that needs `unsafe` goes in `neki-platform` with an explicit, narrowly scoped `#[allow]`).

Dependency direction (lower layers never depend on higher ones):

```
neki-platform, neki-hash → neki-nar → neki-derivation → neki-store → neki-fetch / neki-build / neki-substitute / neki-profile
neki-eval, neki-project (standalone)
all of the above → neki-core → neki-cli (and later neki-gui)
```

- `neki-core` (`Neki` struct) is the only API the CLI and GUI call. It re-exports the types front-ends need, and long operations report through the `EventSink` trait so both front-ends share behaviour. `neki-cli` does only argument parsing (clap) and output.
- `neki-project` parses the per-project declaration `neki.toml` (`[project]`, `[inputs]`, `[dev-shell]`, `[packages.<name>]`, with `deny_unknown_fields`) and `neki.lock` (JSON); `examples/hello/neki.toml` is the reference example and is parsed by a unit test.
- `neki-shim` is a separate tiny exe used as a launcher for store binaries: it reads a `.shim` sidecar, sets PATH/env and runs the target (there's no RPATH on Windows).
- `pkgs/` will hold the package set; `tests/fixtures/` holds golden vectors.

## Cross-platform rule

Every feature must behave the same on Windows, macOS and Linux. **All OS-specific code (`cfg(windows)`/`cfg(unix)`) lives in `neki-platform`**, behind functions whose behaviour is identical everywhere (e.g. `fs::link_dir`: symlink on Unix, junction on Windows). Anything that can't be made identical is not implemented now; it goes in the "Future" section of `docs/PLAN.md`.

## Skeleton conventions

- Every crate has an `Error` enum (`thiserror`) and `type Result<T, E = Error>`; higher crates wrap lower errors with `#[from]`.
- Unimplemented bodies are `todo!("M<n>: <what>")`, where `M<n>` is the milestone in `docs/PLAN.md`. `grep -rn 'todo!' crates/` lists the remaining work. Crates with stubs have `#![allow(unused_variables)]`; remove it once a crate has no stubs left.
- `neki-core` ops that aren't ready return `Error::NotImplemented("M<n>")` instead of calling stubs, so the CLI prints an error rather than panicking. When implementing a milestone, replace both the `todo!`s and the matching `planned("M<n>")` calls.
- Shared dependency versions are in root `[workspace.dependencies]`; crates use `dep.workspace = true`. Add heavy dependencies (reqwest, rusqlite, gix, …) only in the milestone that needs them.

## Invariants

- **Nix's design is the spec, not a compatibility target.** Reuse Nix's formats (NAR, Nix base32, store-path hashing, ATerm `.drv`, narinfo) where convenient, but Neki is not required to be byte-compatible with real Nix, and Nix is not installed or needed. Neki's own committed golden vectors in `tests/fixtures/` are the source of truth; once a format ships, changing its bytes changes every hash, so treat it as a breaking change.
- Default store root is fixed per OS family (`C:\neki\store` on Windows, `/opt/neki/store` on macOS/Linux; state in `C:\neki\var` / `/opt/neki/var`) so hashes match across machines. Tests must use a temporary store via the `NEKI_STORE_DIR` / `NEKI_STATE_DIR` overrides (read by `neki_core::Config::from_env`).
- Store contents must be portable: the NAR unpacker rejects file names that are invalid on any supported OS. Reference scanning looks for hashes in both UTF-8 and UTF-16LE.
