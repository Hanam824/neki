# Neki

Neki is a declarative, reproducible package manager, written in Rust, that runs natively on **Windows, macOS and Linux**.

It follows [Nix](https://nixos.org/guides/how-nix-works/)'s design: packages live in an immutable, content-addressed store; builds are described as derivations; and each project declares its tools and dependencies in one file, pinned by a lock file. Nix itself only runs on Linux and macOS, so Neki rebuilds that architecture in Rust, choosing solutions that behave the same on all three operating systems. Neki is a separate tool. It isn't compatible with Nix, nixpkgs or Nix binary caches.

> **Status: early.** Every milestone of the [plan](docs/PLAN.md) is implemented and tested, and the example projects build, run and rebuild bit-for-bit on Linux, macOS and Windows (`.github/workflows/pkgs.yml`). Things that can't work the same on every OS yet are listed under [Future](docs/PLAN.md#future-not-cross-platform-yet).

## What it does

- **Per-project environments.** A project's `neki.toml` names the tools it needs; `neki develop` opens a shell with exactly those versions, and `neki.lock` pins them.
- **Reproducible builds.** Builds run with a scrubbed environment, only declared inputs on `PATH`, and every download checked against a hash. Building again gives the same bytes.
- **A package set** of 51 prebuilt tools (Node.js, Python, Go, Rust, Zig, Java, Deno, Bun, ripgrep, jq, gh, kubectl, Terraform, …) for macOS, Linux and Windows, every hash taken from upstream's published checksums.
- **Language adapters** that build Cargo, npm and Python projects offline from their lock files.
- **Profiles** with generations and rollback, for tools you want everywhere.
- **Binary caches**: sign and push builds, serve a store over HTTP, and download instead of building.
- **Neki Desktop App** to manage and edit projects, watch builds and their dependency graph, browse the store and manage profiles.

## Build from source

You need a stable [Rust toolchain](https://rustup.rs/) (1.89 or newer). On Linux, the desktop app also needs `libfontconfig1-dev` and `libxkbcommon-dev` (Debian/Ubuntu package names).

```sh
git clone https://github.com/Hanam824/neki.git
cd neki
cargo build --release
```

This builds three programs in `target/release/`:

| Program | What it is |
|---|---|
| `neki` | The command-line interface |
| `neki-desktop` | Neki Desktop App |
| `neki-shim` | The launcher Neki puts in environments for each program; keep it next to `neki` |

## Where Neki keeps things

| | Windows | macOS and Linux |
|---|---|---|
| Store | `C:\neki\store` | `/opt/neki/store` |
| State (database, profiles, logs) | `C:\neki\var` | `/opt/neki/var` |

The store location is fixed so that store paths are identical on every machine. On macOS and Linux, `/opt` belongs to root, so create the folder once and give it to your user:

```sh
sudo mkdir -p /opt/neki && sudo chown "$USER" /opt/neki
```

To use a different store, for example in tests, set `NEKI_STORE_DIR` and `NEKI_STATE_DIR`. Store paths then differ from those on other machines, because the store location is part of every path's hash.

## Quick start

```sh
neki init -C my-app            # creates my-app/neki.toml
cd my-app
neki develop                   # a shell with the dev-shell packages
neki build                     # builds the default package; ./result points at it
neki run -- --help             # builds and runs it
```

Try the examples in this repository, which use the package set in `pkgs/`:

```sh
neki -C examples/node-app build && examples/node-app/result/bin/node-app
neki -C examples/python-tool build && examples/python-tool/result/bin/python-tool
neki -C examples/rust-hello build && examples/rust-hello/result/bin/rust-hello
```

## Declaring a project

Each project has a `neki.toml` (see [`examples/hello`](examples/hello/neki.toml)):

```toml
[project]
name = "hello"

[inputs]
pkgs = { url = "github:Hanam824/neki?dir=pkgs" }   # pinned in neki.lock

[dev-shell]
packages = ["pkgs.jq", "pkgs.ripgrep", "pkgs.python"]
env = { GREETING = "hello" }
shell-hook = "echo ready"

[packages.default]
src = "."
build-inputs = ["pkgs.python"]
build = [["python", "build.py", "${out}"]]
```

- `inputs` are package sources: `github:<owner>/<repo>[/<ref>]`, `git+<url>`, `path:<dir>` or an archive URL, each optionally with `?dir=<subdir>`. The first time an input is used, it's pinned in `neki.lock`; `neki lock --update <name>` moves it to the newest version.
- In a git repository, a project's source is what git lists: tracked files and untracked ones that `.gitignore` doesn't exclude, so `target/` or `node_modules/` don't change it. Elsewhere, everything except `.git`, `result*` and the package's `exclude` list is copied.
- `dev-shell` is the environment `neki develop` opens (`neki develop --print-env posix`, `powershell` or `nushell` prints a script to load it into the current shell instead). If `$SHELL` names `nu`, `neki develop` and `neki shell` start Nushell, on any OS.
- `packages` are what `neki build` produces. Build steps are argument lists run without a shell, so they behave the same on every OS; `${out}` is the output folder in the store, and `${NAME}` any other build variable. A package can instead name a `.neki` file (`file = "package.neki"`) to use the language adapters, as the examples do.

Packages are named `<input>.<package>`. Outside a project, `neki shell pkgs.jq` and profiles use the registry in your settings, where `pkgs` is this repository's package set.

## Commands

Every command accepts `--json` (the result as JSON) and `--log-format json` (progress as JSON lines on stderr).

Evaluation is pure: `builtins.getEnv` returns `""`, and a `fetchurl` or `fetchTarball` needs a hash and a `fetchGit` a `rev`. `--impure` (or `impure = true` in `config.toml`) allows them.

| Command | Purpose |
|---|---|
| `neki init`, `neki info` | Create a project; show the configuration and project |
| `neki build [package]` | Build a package (`--file x.neki -A attr` for an expression, `--drv` for derivations) |
| `neki run [package] -- args` | Build and run a package's program |
| `neki develop`, `neki shell <pkgs…>` | Open a shell with a project's or the given packages |
| `neki lock [--update input]` | Pin the project's inputs in `neki.lock` |
| `neki eval <expr>` | Evaluate an expression |
| `neki profile install\|remove\|upgrade\|rollback\|switch\|history\|list` | Manage profiles (add `<state>/profiles/default/current/bin` to `PATH`) |
| `neki store add\|path-info\|ls\|closure\|why-depends\|roots\|verify\|repair\|gc\|optimise` | Inspect and maintain the store (`gc --dry-run`, `--delete-older-than 30d`; `repair` fixes modified paths from a cache or by rebuilding) |
| `neki store serve` | Serve the store as a binary cache over HTTP (`--allow-push` accepts uploads signed by a trusted key) |
| `neki cache keygen\|push\|fetch` | Sign and upload to a `file://` cache or a server started with `--allow-push`, or download from any cache |
| `neki hash path\|file` | Hash a file tree (as a NAR) or a file's contents |

Run `neki --help` or `neki <command> --help` for details.

## Settings

Optional, in `config.toml` in your config folder (`~/.config/neki` on Linux, `~/Library/Application Support/neki` on macOS, `%APPDATA%\neki` on Windows):

```toml
substituters = ["https://cache.example.org"]
trusted-public-keys = ["cache.example.org-1:…"]   # from `neki cache keygen`
secret-key-file = "/path/to/secret-key"            # for `neki cache push`
max-jobs = 4

[registry]
pkgs = "github:Hanam824/neki?dir=pkgs"
```

## The package set

`pkgs/default.neki` holds the packages; `pkgs/sources.json` their versions, URLs and SHA-256 hashes for `aarch64-darwin`, `x86_64-darwin`, `x86_64-linux`, `aarch64-linux` and `x86_64-windows`. To update it, run `scripts/update-pkgs.py` (it uses `gh` if installed). `pkgs/lib.neki` has the helpers and adapters: `buildRustPackage` (from `Cargo.lock`, including `git` dependencies), `buildNpmPackage` (from `package-lock.json`, with the platform-specific packages for the current system) and `buildPythonApplication` (from wheels). Wheels with native code differ per system; `scripts/python-wheels.py name==version …` picks one per system from PyPI and prints them as a `.neki` file, as in `examples/python-tool`.

## Downloads

Versions are published as [GitHub releases](https://github.com/Hanam824/neki/releases); the latest one is the current version. Every night that `main` has changed, it's also built as the `nightly` pre-release, which the next night's build replaces. Each release has archives for Windows (x86_64), macOS (Apple Silicon and Intel) and Linux (x86_64). The builds aren't signed. On macOS, run `xattr -dr com.apple.quarantine <unpacked folder>` once if macOS blocks them.

## Repository layout

```
crates/
  neki-platform     the only OS-specific code (paths, links, processes, shells)
  neki-hash         hashes and store paths
  neki-nar          archive format for store contents
  neki-derivation   build recipes (.drv)
  neki-store        the store: database, GC, optimise
  neki-fetch        downloading and unpacking sources
  neki-build        running builds
  neki-substitute   binary caches
  neki-eval         the expression language
  neki-project      neki.toml and neki.lock
  neki-profile      profiles and environments
  neki-shim         launcher for installed programs
  neki-core         the API shared by the CLI and the desktop app
  neki-cli          the `neki` command
  neki-gui          Neki Desktop App (Slint)
pkgs/               the package set
examples/           sample projects
docs/PLAN.md        design, cross-platform decisions and roadmap
scripts/            release, CI and package-set helpers
```

## Contributing

- Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` before opening a pull request. CI runs the same checks on Windows, macOS and Linux.
- Branch names and commit messages follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/), e.g. branch `feat/nar-dump-restore`, commit `feat(nar): serialize and restore file trees as NAR`.
- Every feature must behave the same on all three operating systems. OS-specific code belongs only in `neki-platform`; anything that can't be made to work everywhere goes in the "Future" section of `docs/PLAN.md`.

## License

[MIT](LICENSE) © 2026 Hanam824
