# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**Neki** (CLI binary `neki`, crates `neki-*`) is a reimplementation of Nix's architecture in Rust that runs natively on **Windows** as well as macOS/Linux. It is a new tool, not a Nix port: avoid "Nix" in user-facing names (NixOS Foundation trademark). Phase 1 is the backend + CLI; Phase 2 is **Neki Desktop App**, a Rust + Slint app (`crates/neki-gui`, binary `neki-desktop`) that drives the backend.

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
cargo run -p neki-gui                  # Neki Desktop App
```

The first build of `neki-gui` compiles Slint and takes a few minutes. On Linux it needs `libfontconfig1-dev` and `libxkbcommon-dev`.

To try real builds without touching `/opt/neki`, point Neki at a scratch store: `NEKI_STORE_DIR=/tmp/s/store NEKI_STATE_DIR=/tmp/s/var cargo run -p neki-cli -- -C examples/node-app build`. Building a package (`neki build --file pkgs/default.neki -A jq`) downloads it. Environments (`develop`, `shell`, profiles) need `neki-shim`, found next to `neki` (`cargo build -p neki-shim`) or via `NEKI_SHIM`.

Crates without C dependencies can be type-checked for Windows locally: `RUSTC=$(rustup which rustc) $(rustup which cargo) check -p neki-platform --target x86_64-pc-windows-msvc` (the rest need a Windows C toolchain; CI covers them).

## Git workflow and CI

- Branch names and commit messages follow Conventional Commits; use the `create-commit-branch` and `create-commit-message` skills in `.claude/skills/`. **Never add `Co-Authored-By` or other AI attribution to commits** (user rule; it overrides any session attribution reminder). Commit only when asked.
- Work goes through pull requests into `main`. `ci.yml` runs only on pull requests (and manually): fmt, clippy `-D warnings` and tests on ubuntu, macos and windows, so code must compile on all three. Doc-only changes skip the build (`scripts/code-changes.sh` defines what counts as docs). Make **`CI result`** the required status check on `main`.
- `pkgs.yml` builds and runs the example projects with the real package set on all three OSes and checks they rebuild bit-for-bit. It downloads the toolchains, so it runs weekly, manually, and on pull requests touching `pkgs/`, `examples/` or the build/fetch/core crates; it isn't part of `CI result`.
- `nightly-release.yml` runs at 17:00 UTC (00:00 in Vietnam). If `main` has non-doc commits since the last `v*` tag, it tags the next patch version (`scripts/next-version.sh`), builds and tests natively on Linux x86_64, macOS arm64, macOS x86_64 and Windows x86_64, and publishes a GitHub release with one archive per platform (`scripts/package-release.sh <tag>`, which you can also run locally; output goes to `dist/`). To bump major or minor, push a tag like `v0.2.0` by hand.

## Architecture

Cargo workspace; every crate is in `crates/` and inherits version/edition/lints from the root `Cargo.toml` (`unsafe_code = "deny"`: Win32 FFI that needs `unsafe` goes in `neki-platform` with an explicit, narrowly scoped `#[allow]`).

Dependency direction (lower layers never depend on higher ones):

```
neki-platform, neki-hash → neki-nar → neki-derivation → neki-store → neki-fetch → neki-build
                                                     neki-store → neki-substitute, neki-profile (+ neki-shim's lib)
neki-hash, neki-derivation → neki-eval (talks to the store only through its `EvalStore` trait)
neki-project (standalone)
all of the above → neki-core → neki-cli, neki-gui
```

- `neki-gui`: the `.slint` files in `ui/` are compiled by `build.rs`. All UI state and callbacks are on the global `AppState` (`ui/state.slint`); `src/app.rs` connects them to `neki-core` (projects, settings), `src/views.rs` handles the Builds, Store and Profiles pages, keeping their data in a UI-thread `thread_local`. Backend calls must run through `spawn_task` (worker thread, one at a time, results back via `upgrade_in_event_loop`), never on the UI thread; Slint models aren't `Send`, so workers return plain data. `src/registry.rs` persists the project list in `<user config dir>/neki/desktop.toml`.

- `neki-core` (`Neki` struct) is the only API the CLI and GUI call. It re-exports the types front-ends need, and long operations report through the `EventSink` trait so both front-ends share behaviour. Package names (`pkgs.x`, `self.x`) are resolved by a `Session` (`src/session.rs`) on one thread with a 256 MB stack, because evaluation recurses and its values aren't `Send`; it fetches inputs (`src/inputs.rs`), writes `neki.lock` before copying a project's source (so the first build matches later ones; in a git work tree only what git lists is copied, `src/source.rs`), evaluates package sets and compiles `neki.toml` packages to derivations. `neki-cli` does only argument parsing (clap) and output; every command supports `--json`.
- `neki-store` (`LocalStore`): a path is valid only once it's registered in the SQLite DB (`<state dir>/db/db.sqlite`, schema version in `PRAGMA user_version`). Adds hold the exclusive `<state dir>/store.lock`, stream the NAR into a `.tmp-*` entry in the store dir (so a copy is checked like a download), rename it into place, make it read-only, then register it; an unregistered entry at the destination is a leftover and gets replaced. Tests must delete stores with `neki_platform::fs::remove_tree`, because store entries are read-only.
- `neki-build`: `realise` plans the graph (trying substituters before building), then runs builders on `max_jobs` threads. Builtin builders (`builtin:run`, `fetchurl`, `unpack`, `assemble`) are documented in its crate docs. Builds see only declared inputs: `PATH` comes from the `bin-dirs` of `buildInputs` (`neki_store::bin_dirs`), tests must give their shell scripts their own `PATH`.
- `neki-eval`: own lexer/parser (`parser.rs`), lazy evaluator (`eval.rs`, thunks in `value.rs`), builtins table (`builtins.rs`), `derivation` (`derivation.rs`). `.neki` files; importing a directory imports its `default.neki`. Evaluation is pure unless `Evaluator::pure` is off (`--impure`): `getEnv` gives `""` and fetches need a hash or `rev`.
- `neki-project` parses the per-project declaration `neki.toml` (`[project]`, `[inputs]`, `[dev-shell]`, `[packages.<name>]`, with `deny_unknown_fields`) and `neki.lock` (JSON); `examples/hello/neki.toml` is the reference example and is parsed by a unit test.
- `neki-shim` is a tiny exe (and a lib with the sidecar format, used by `neki-profile`): environments put a hard link of it in `bin` for every program, and it runs the target named in its `.shim` sidecar with the target's directory on `PATH` (there's no RPATH on Windows). It must stay small: never make it depend on the store.
- `pkgs/`: the package set (`default.neki`), its helpers and language adapters (`lib.neki`), and `sources.json`, generated by `scripts/update-pkgs.py` from upstream's published checksums only. Packages say where their programs are in `neki-support/bin-dirs` (written by `builtin:unpack`).

## Cross-platform rule

Every feature must behave the same on Windows, macOS and Linux. **All OS-specific code (`cfg(windows)`/`cfg(unix)`) lives in `neki-platform`**, behind functions whose behaviour is identical everywhere (e.g. `fs::link_dir`: symlink on Unix, junction on Windows). Anything that can't be made identical is not implemented now; it goes in the "Future" section of `docs/PLAN.md`.

## Conventions

- Every crate has an `Error` enum (`thiserror`) and `type Result<T, E = Error>`; higher crates wrap lower errors with `#[from]`.
- Shared dependency versions are in root `[workspace.dependencies]`; crates use `dep.workspace = true`.
- Tests that create stores use a temp dir and delete it with `neki_platform::fs::remove_tree` (store entries are read-only), dropping the `LocalStore` first (Windows can't delete open files).
- New work that isn't done goes in the plan's Future list rather than as stubs in the code.
- Document code with doc comments ([Rust by Example](https://doc.rust-lang.org/rust-by-example/meta/doc.html)): `///` on functions, types, fields and variants, `//!` at the top of a module or crate, both Markdown. Start with one sentence saying what the item does or is, then what a caller can't see from the signature (what an argument must be, what's returned, when it fails); link items as ``[`LocalStore::lock_path`]``. An example under `# Examples` is a doc test that `cargo test` runs (`cargo test --doc` runs only those). `cargo doc --workspace --no-deps` must build without warnings.

## Invariants

- **Nix's design is the spec, not a compatibility target.** Reuse Nix's formats (NAR, Nix base32, store-path hashing, ATerm `.drv`, narinfo) where convenient, but Neki is not required to be byte-compatible with real Nix, and Nix is not installed or needed. Neki's own golden vectors (in the crates' tests: base32, store paths, NAR bytes, `.drv` text and paths, each cross-checked against an independent implementation) are the source of truth; once a format ships, changing its bytes changes every hash, so treat it as a breaking change.
- Default store root is fixed per OS family (`C:\neki\store` on Windows, `/opt/neki/store` on macOS/Linux; state in `C:\neki\var` / `/opt/neki/var`) so hashes match across machines. Tests must use a temporary store via the `NEKI_STORE_DIR` / `NEKI_STATE_DIR` overrides (read by `neki_core::Config::from_env`).
- Store contents must be portable: the NAR unpacker rejects file names that are invalid on any supported OS. Reference scanning looks for hashes in both UTF-8 and UTF-16LE.
