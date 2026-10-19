# Neki

Neki is a declarative, reproducible package manager, written in Rust, that runs natively on **Windows, macOS and Linux**.

It follows [Nix](https://nixos.org/guides/how-nix-works/)'s design: packages live in an immutable, content-addressed store; builds are described as derivations; and each project declares its tools and dependencies in one file, pinned by a lock file. Nix itself only runs on Linux and macOS, so Neki rebuilds that architecture in Rust, choosing solutions that behave the same on all three operating systems. Neki is a separate tool. It isn't compatible with Nix, nixpkgs or Nix binary caches.

> **Status: early development.** The architecture and the command line are in place, but most operations aren't implemented yet. Commands that aren't ready say which milestone will implement them, for example `error: not implemented yet (planned for M3)`. See [`docs/PLAN.md`](docs/PLAN.md) for the roadmap.

## What works today

- `neki init`: create a project with a starter `neki.toml`
- `neki info`: show the configuration and the current project's declaration (`--json` for machine-readable output)
- Parsing and validating `neki.toml` and `neki.lock`
- **Neki Desktop App**: register projects, view and edit their `neki.toml` (validated before saving), and browse settings

## Build from source

You need a stable [Rust toolchain](https://rustup.rs/). On Linux, the desktop app also needs `libfontconfig1-dev` and `libxkbcommon-dev` (Debian/Ubuntu package names).

```sh
git clone <this repository>
cd Nix-design
cargo build --release
```

This builds three programs in `target/release/`:

| Program | What it is |
|---|---|
| `neki` | The command-line interface |
| `neki-desktop` | Neki Desktop App |
| `neki-shim` | A small launcher Neki puts in front of installed programs (used once environments are implemented) |

To put `neki` on your `PATH`, run `cargo install --path crates/neki-cli`.

## Quick start

```sh
neki init -C my-app        # creates my-app/neki.toml
neki info -C my-app        # shows the store location, your system and the project
cargo run -p neki-gui      # opens Neki Desktop App
```

Without `-C`, `neki` looks for a `neki.toml` in the current folder and its parents.

## Declaring a project

Each project has a `neki.toml` (see [`examples/hello`](examples/hello/neki.toml)):

```toml
[project]
name = "hello"
description = "Example Neki project"

[inputs]
pkgs = { url = "github:neki-pkgs/pkgs" }   # package source, pinned in neki.lock

[dev-shell]
packages = ["pkgs.rustc", "pkgs.cargo"]
env = { RUST_LOG = "info" }

[packages.default]
src = "."
build-inputs = ["pkgs.rustc", "pkgs.cargo"]
build = [["cargo", "install", "--path", ".", "--root", "${out}"]]
```

- `inputs` are the sources the project uses. `neki lock` pins each to an exact revision and hash in `neki.lock`.
- `dev-shell` is the environment `neki develop` opens.
- `packages` are what `neki build` produces. Build steps are argument lists run without a shell, so they behave the same on every OS; `${out}` is the output folder in the store.

There's no Neki package set yet, so the `pkgs` input above is a placeholder.

## Commands

| Command | Purpose | Status |
|---|---|---|
| `neki init` | Create a project | ✅ |
| `neki info [--json]` | Show configuration and project | ✅ |
| `neki hash path\|file` | Hash files and folders | M1 |
| `neki store add\|path-info\|verify` | Use the store directly | M2 |
| `neki build [package]` | Build a package | M3 |
| `neki develop`, `neki shell`, `neki run`, `neki lock` | Environments and lock files | M5 |
| `neki profile install\|remove\|rollback\|history`, `neki store gc\|optimise` | Profiles and garbage collection | M6 |
| `neki cache push` | Upload to a binary cache | M7 |

Run `neki --help` or `neki <command> --help` for details.

## Where Neki keeps things

| | Windows | macOS and Linux |
|---|---|---|
| Store | `C:\neki\store` | `/opt/neki/store` |
| State (database, profiles, logs) | `C:\neki\var` | `/opt/neki/var` |

The store location is fixed so that store paths are identical on every machine. To use a different store, for example in tests, set `NEKI_STORE_DIR` and `NEKI_STATE_DIR`.

## Downloads

Once the repository is on GitHub, a nightly workflow publishes a release whenever `main` has changed. Each release has archives for Windows (x86_64), macOS (Apple Silicon and Intel) and Linux (x86_64). The builds aren't signed. On macOS, run `xattr -dr com.apple.quarantine <unpacked folder>` once if macOS blocks them.

## Repository layout

```
crates/
  neki-platform     the only OS-specific code (paths, links, processes, shells)
  neki-hash         hashes and store paths
  neki-nar          archive format for store contents
  neki-derivation   build recipes
  neki-store        the content-addressed store
  neki-fetch        downloading sources
  neki-build        running builds
  neki-substitute   binary caches
  neki-eval         expression language evaluator
  neki-project      neki.toml and neki.lock
  neki-profile      profiles and shell environments
  neki-core         the API shared by the CLI and the desktop app
  neki-cli          the `neki` command
  neki-gui          Neki Desktop App (Slint)
  neki-shim         launcher for installed programs
docs/PLAN.md        design, cross-platform decisions and roadmap
examples/           sample projects
scripts/            release and CI helpers
```

## Contributing

- Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` before opening a pull request. CI runs the same checks on Windows, macOS and Linux.
- Branch names and commit messages follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/), e.g. branch `feat/nar-dump-restore`, commit `feat(nar): serialize and restore file trees as NAR`.
- Every feature must behave the same on all three operating systems. OS-specific code belongs only in `neki-platform`; anything that can't be made to work everywhere goes in the "Future" section of `docs/PLAN.md`.

## License

[MIT](LICENSE) © 2026 Hanam824
