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

Cargo workspace; every crate is in `crates/` and inherits version/edition/lints from the root `Cargo.toml` (`unsafe_code = "deny"` — Win32 FFI needing `unsafe` should be isolated in a `platform` module with an explicit `#[allow]`).

Dependency direction (lower layers never depend on higher ones):

```
neki-hash → neki-nar → neki-derivation → neki-store → neki-fetch / neki-build / neki-substitute
          → neki-eval → neki-project / neki-profile → neki-core → neki-cli (and later neki-gui)
```

- `neki-core` is the only API the CLI and GUI should call; it emits progress/log events so both front-ends share behaviour.
- `neki-shim` is a separate tiny exe used as a launcher for store binaries (Windows has no RPATH; the shim sets PATH/DLL dirs then execs the target).
- `pkgs/` will hold the package set (Nix-language recipes, incl. ones imported from Scoop/winget manifests); `tests/fixtures/` holds golden vectors.

## Invariants

- **Nix's design is the spec, not a compatibility target.** Reuse Nix's formats (NAR, Nix base32, store-path hashing, ATerm `.drv`, narinfo) where convenient, but Neki is not required to be byte-compatible with real Nix, and Nix is not installed or needed. Neki's own committed golden vectors in `tests/fixtures/` are the source of truth; once a format ships, changing its bytes changes every hash, so treat it as a breaking change.
- Default store root is fixed (`C:\neki\store` on Windows, `/neki/store` elsewhere) so hashes match across machines; tests must use a temporary store via the `NEKI_STORE_DIR` override (to be implemented in M2).
- Windows specifics are handled behind per-OS `platform` modules: junctions/hardlinks instead of symlinks, read-only attribute + ACLs for immutability, Job Objects for build isolation, reference scanning in both UTF-8 and UTF-16LE.
