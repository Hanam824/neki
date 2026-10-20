# Plan: a cross-platform, Nix-style package manager written in Rust (Windows-first)

**Name: Neki** (binary `neki`, crates `neki-*`; decided 2026-10-19). **Nix compatibility:** Nix is the design spec only; byte-compatibility with real Nix is *not* a goal and Nix is not needed for development (decided 2026-10-19). Store root is `C:\neki\store` (Windows) / `/opt/neki/store` (macOS, Linux). **Cross-platform rule** (decided 2026-10-19): every feature behaves the same on Windows, macOS and Linux; anything that can't is moved to [Future](#future-not-cross-platform-yet).

## Context

Nix gives reproducible, declarative, per-project environments. It does this with an immutable, content-addressed store (`/nix/store/<hash>-name`), derivations (pure build recipes), and closures. Officially, though, it only runs on Linux and macOS. Its design depends on Unix features that Windows doesn't have: build sandboxing (user namespaces), free symlinks, RPATH, and a POSIX shell. The goal is to rebuild Nix's architecture in Rust so it also works natively on Windows, using the same concepts:

- **Phase 1:** the core backend. This covers the store, derivations, declarative per-project files, the builder, binary caches, profiles, garbage collection and a CLI.
- **Phase 2:** a desktop app (Rust + Slint) to inspect and control every project's declaration, edit it, build, update locks and track the store.

The repo started empty, so this is a greenfield project. The first implementation step also writes `CLAUDE.md`, which finishes the earlier `/init` request.

---

## Part A: Cross-platform policy

Rule: one behaviour on every OS. Where the OS mechanism differs but the behaviour can be identical, it lives behind one function in **`neki-platform`** (the only crate with `cfg(windows)`/`cfg(unix)` code). Everything else goes to [Future](#future-not-cross-platform-yet).

| # | Topic | Now (same on all OSes) | Future |
|---|---|---|---|
| P1 | Build isolation | Scrubbed environment, private temp/build dir, `PATH` = declared inputs only, the whole process tree killed on exit (`platform::process`: process group on Unix, Job Object on Windows) | Real sandboxes (Linux namespaces, macOS sandbox-exec, Windows AppContainer), network blocking |
| P2 | Package source | Our own package set (nixpkgs can't run on Windows); generic "prebuilt binary archive" recipe (url + hash + bin list) | Scoop/winget manifest importers, Homebrew importer |
| P3 | Language | `neki.toml` declarative front end; own evaluator for a Nix-language subset (no nixpkgs compatibility; `rnix-parser` is MIT, `snix-eval` is GPL-3 so it's avoided) | — |
| P4 | Finding libraries at runtime | Launcher **shim** (the same Rust exe on every OS) reads a `.shim` sidecar, sets `PATH`/env and runs the target | Windows DLL hardlinking next to exes, RPATH patching on Unix |
| P5 | Links (profiles, `result`, roots) | GC roots in the store DB (no links). Profiles = merged dirs of **hardlinks** + shims. `result` = `platform::fs::link_dir` (symlink on Unix, junction on Windows) | Real symlinks on Windows with Developer Mode |
| P6 | MSI/EXE installers, registry, services | — | MSI extraction, "impure: system" flag |
| P7 | Toolchains | Compilers are normal inputs; no toolchain bundled yet | LLVM-mingw stdenv, MSVC as an impure system input (it can't be redistributed) |
| P8 | Store immutability | `std` read-only permission on registered files, plus `neki store verify` | Windows deny-write ACL, `chattr +i` |
| P9 | Store root | Fixed per OS family: `C:\neki\store` (Windows) or `/opt/neki/store` (macOS + Linux; `/opt` needs no synthetic APFS volume on macOS); state in `C:\neki\var` / `/opt/neki/var`; overridable with `NEKI_STORE_DIR` / `NEKI_STATE_DIR` | Relocatable stores |
| P10 | File names | The NAR unpacker rejects names invalid on **any** supported OS (Windows reserved names/chars, trailing dot/space) so store contents are portable. Archives are unpacked leaving out entries that differ only by case from an earlier one (logged), e.g. ncurses terminfo in Linux releases. The exec bit is stored in the NAR and applied only on Unix | Escaping instead of rejection |
| P11 | Daemon / multi-user | Single-user, in-process | Daemon (Unix socket / named pipe), multi-user |
| P12 | Shells | `neki develop`/`neki shell` spawn a child shell with the env applied (`$SHELL` on Unix, PowerShell on Windows). Activation scripts for POSIX sh and PowerShell (pwsh runs on all 3 OSes) | cmd, fish and Nushell scripts, direnv-style auto-activation |
| P13 | Reference scanning | Scan outputs for hashes in UTF-8 **and** UTF-16LE on every OS | — |
| P14 | Locked files / GC | Clear read-only flags and retry removal (`platform::fs::remove_tree`) | Windows delete-on-reboot, Defender exclusion helper |
| P15 | NixOS / nix-darwin | — | User-environment module |
| P16 | Line endings | Git fetches never convert CRLF; path imports hash raw bytes | — |

---

## Part B: Nix features we can support

**Store and formats (same design as Nix):**
- Store paths: `sha256` → truncated to 160 bits → Nix base32, `<hash>-<name>`
- NAR serialization, `nix-hash`, the SRI hash format (`sha256-…`)
- `.drv` files in ATerm format, with derivation hashing modulo fixed-output derivations
- Input-addressed, fixed-output and (later) content-addressed derivations
- A SQLite metadata DB (`ValidPaths`, `Refs`, `DerivationOutputs`), like Nix's `db.sqlite`
- Closures, references, deriver, `path-info`, `why-depends`
- Store optimise (hardlink dedup), `verify`, `repair`

**Language and evaluation:**
- Nix-language subset: let/rec/with/inherit, lambdas with patterns, lazy attrsets, string context, `import`, `derivation`, and core builtins (`fetchurl`, `fetchTarball`, `fetchGit`, `path`, `toFile`, `readFile`, `map`, `filter`, `attrNames`, `toJSON`, `fromJSON`, `hashString`, …)
- `neki.toml` simple front end, which compiles to the same derivations
- Overlays/overrides (`override`, `overrideAttrs`) in our own package set library

**Projects (flake-like):**
- A per-project `neki.toml` (`[project]`, `[inputs]`, `[dev-shell]`, `[packages.<name>]`; see `examples/hello`), later also a Nix-language `flake.neki`
- A lock file (`neki.lock`, JSON) and `neki lock --update <input>`
- Dev shells: `neki develop` and `neki shell pkgA pkgB` (ad hoc)
- `neki build` → a `result` link, `neki run`

**Fetchers / builders:**
- `fetchurl`, `fetchzip` (zip/tar.gz/tar.xz), `fetchgit`, `fetchFromGitHub`
- Builders: commands as argv lists run without a shell (identical on every OS), `mkDerivation`-style phases later
- **Foreign package manager adapters** (the "control all package managers" goal): pinned npm/pip/cargo/go dependencies as fixed-output derivations, generated from their lock files (like `buildNpmPackage`/`buildRustPackage`)

**Distribution:**
- Binary caches: `file://` and `https://` substituters, `.narinfo`, `nar.xz`/`nar.zst`, ed25519 signatures, `trusted-public-keys`
- `neki copy --to`, `neki store serve` (a simple HTTP cache server)

**User environments:**
- Profiles with generations, `profile install/remove/upgrade/rollback/history`
- GC roots (per project, profile and `result`), `neki gc`, `--delete-older-than`

**Later (cross-platform):** remote builders over SSH, CA derivations.

---

## Part C: Architecture (Cargo workspace)

```
Nix-design/
├─ Cargo.toml                 # workspace
├─ crates/
│  ├─ neki-platform/          # the only OS-specific code: default dirs, links, read-only, process groups, shell detection
│  ├─ neki-hash/              # sha256, nix-base32, SRI, store-path computation
│  ├─ neki-nar/               # NAR read/write (streaming), portable-name validation
│  ├─ neki-derivation/        # Derivation model, ATerm (de)serialize, hashDerivationModulo
│  ├─ neki-store/             # LocalStore: layout, SQLite DB, locks, add/query, refs scan, GC, optimise, read-only
│  ├─ neki-fetch/             # http (reqwest), git (gix), unpack zip/tar, hash verify
│  ├─ neki-build/             # build scheduler (DAG, parallel jobs), executor, env scrubbing, process-tree kill, logs
│  ├─ neki-substitute/        # binary-cache client/server, narinfo, signatures (ed25519-dalek)
│  ├─ neki-eval/              # rnix-parser → AST → lazy evaluator, builtins, string context
│  ├─ neki-project/           # neki.toml / neki.lock loading, inputs resolution
│  ├─ neki-profile/           # profiles, generations, buildEnv merge, shell activation scripts
│  ├─ neki-shim/              # tiny launcher exe (reads .shim sidecar, sets PATH/env, runs target)
│  ├─ neki-core/              # facade API used by CLI + GUI (one place for high-level ops + events)
│  ├─ neki-cli/               # `neki` binary (clap)
│  └─ neki-gui/               # Phase 2 – Slint app
├─ pkgs/                      # our package set
├─ examples/                  # sample projects (examples/hello/neki.toml)
└─ tests/                     # integration fixtures (golden NAR/drv/hash vectors)
```

**Platform abstraction:** all OS-specific code is in `neki-platform`, behind functions with identical behaviour on every OS: `link_dir` (symlink vs junction), `spawn_tree`/`ProcessTree::kill` (process group vs Job Object), `file_id` (inode vs file index), `is_program` (executable bit vs `.exe`/`.cmd`), `exec_replace` (exec vs run-and-exit), `shell::open_terminal` (Terminal, an X terminal, or a PowerShell console).

**Conventions:** every crate has `Error` (`thiserror`) + `Result`. APIs are sync; builds use threads, evaluation runs on one thread with a big stack. The skeleton's stubs (`todo!`, `NotImplemented`) are all gone.

**Crates used:** `sha2`, `data-encoding`, `rusqlite` (bundled SQLite), `ureq` (rustls), `zip`, `tar`, `flate2`, `lzma-rs`, `zstd`, `ed25519-dalek`, `getrandom`, `tiny_http`, `regex`, `process-wrap` (process groups/Job Objects), `junction` and `windows-sys` (inside `neki-platform`), `rfd` (folder picker) and `slint` (Phase 2), plus `thiserror`, `serde`, `serde_json`, `toml`, `clap`, `anyhow`, `dirs`. Changed from the first plan: the expression parser is Neki's own (better error positions than `rnix-parser`), git sources use the `git` program instead of `gix`, file locking uses `std` (Rust 1.89) instead of `fs4`, and HTTP uses `ureq` instead of `reqwest`.

**Format stability:** Neki's own golden vectors (in the crates' tests: base32, store paths, NAR bytes, `.drv` text and paths) define the formats; changing them is a breaking change.

---

## Part D: Phase 1 milestones (each ends in a working, tested slice)

| M | Deliverable | Exit criteria |
|---|---|---|
| **M0** | Workspace scaffold, CI (GitHub Actions: windows/macos/ubuntu), `CLAUDE.md`, lint (`clippy -D warnings`, `fmt`) | `cargo test --workspace` green on all 3 OSes |
| **M0.5** | Cross-platform skeleton: all crate APIs, `neki-platform`, the full CLI, `neki.toml`/`neki.lock` parsing, `neki info` | Builds clippy-clean; `neki info` shows config + project; other commands fail with "planned for M<n>" ✅ |
| **M1** | `neki-hash` + `neki-nar` | NAR + base32 + store path match committed golden vectors; `neki hash path/file` ✅ (base32 matches Nix's own vectors; hashes, NAR bytes and store paths cross-checked against openssl and an independent Python implementation) |
| **M2** | `neki-store` (local store, DB, `add-to-store`, refs, read-only, locks) | `neki store add <dir>` → `C:\neki\store\<hash>-name`, re-adding is idempotent, `path-info`/`verify` work ✅ (SQLite DB in `<state>/db/db.sqlite`; adds stream the NAR into a temporary entry, then rename, make read-only and register under `<state>/store.lock`; reference scanner for UTF-8 and UTF-16LE ready for M3; `link_dir` uses junctions on Windows) |
| **M3** | `neki-derivation` + `neki-build` (executor, process-tree kill, scheduler, logs) + `neki-fetch` (fetchurl/zip) | Hand-written `.drv` builds; a fixed-output fetch verifies the hash; deps build in DAG order, in parallel ✅ (ATerm golden vector cross-checked in Python; builtins `run`, `fetchurl`, `unpack`, `assemble`; scrubbed environment; cancel kills the process tree; zip/tar/tar.gz/tar.xz unpacking rejects path traversal and non-portable names; `neki build --drv`) |
| **M4** | `neki-eval` v1 (core language + `derivation` + fetch builtins) and `neki.toml` front end | `neki build` from a `.neki` expression produces a deterministic `.drv` path (golden-tested) ✅ (own parser; lazy evaluation, `with`/`rec`/`inherit`, patterns, string context, ~75 builtins incl. `fromTOML`; golden `.drv` path cross-checked in Python; `neki build --file`, `neki eval`) |
| **M5** | Input resolution + lock file writing + `develop`/`shell`/`run` + POSIX/PowerShell activation + shims | In a sample project, `neki develop` gives a shell with pinned tools (e.g. Node, Python, Go from prebuilt-archive recipes) on all 3 OSes ✅ on macOS (inputs `github:`, `git+`, `path:`, archive URLs with `?dir=`; `neki.lock` written on first use; shims via `neki-shim`; `--print-env posix\|powershell`); Windows and Linux are covered by `.github/workflows/pkgs.yml`, not yet run |
| **M6** | `neki-profile` (profiles, generations, rollback) + GC (roots, `--delete-older-than`) + optimise | Install → upgrade → rollback works; GC removes only unreachable paths ✅ (roots in the DB, indirect roots for `result` links, builds hold a shared GC lock; optimise hard-links identical files via `.links`) |
| **M7** | `neki-substitute` (file/http cache, signing, `copy`, `store serve`) | Machine A builds and pushes, machine B substitutes with a verified signature ✅ (narinfo + ed25519 in Nix's formats; `neki cache push/fetch/keygen`, `neki store serve`; builds try substituters first; tested with two stores sharing a store dir, over `file://` and HTTP) |
| **M8** | `pkgs/` starter set (~50 packages: prebuilt-archive recipes per OS + Cargo/npm/pip adapters) | Example projects for Rust, Node and Python build reproducibly on all 3 OSes ✅ on macOS: 42 packages for 5 systems, every hash from upstream's published checksums (`scripts/update-pkgs.py`); adapters `buildRustPackage` (Cargo.lock, zig as linker), `buildNpmPackage` (package-lock.json), `buildPythonApplication` (wheels); the examples rebuild bit-for-bit after deleting their outputs. Windows and Linux: `pkgs.yml`, not yet run |
| **M9** | `neki-core` facade with an event stream (progress, logs) + JSON output for every CLI command | The GUI can drive every operation through the API, without the CLI ✅ (`--json` on every command, `--log-format json` event stream; the desktop app uses only `neki-core`) |

## Part E: Phase 2, the desktop app (Rust + Slint)

**Name: Neki Desktop App** (crate `neki-gui`, binary `neki-desktop`).

- **Architecture:** `neki-gui` links `neki-core` directly. The UI state is the Slint global `AppState` (`crates/neki-gui/ui/state.slint`); `src/app.rs` wires its callbacks to `neki-core`. Backend operations run one at a time on a worker thread (`spawn_task`), and events and results come back through `upgrade_in_event_loop`. The project list is saved in `<user config dir>/neki/desktop.toml`. Later, when the daemon exists (Future), the app connects to it so builds keep running after the window closes.
- **Screens:**
  1. **Projects:** register or discover project folders, with status for each (lock outdated, build state, last build).
  2. **Project detail:** a declaration editor (syntax-highlighted text plus a structured form for `neki.toml`), inputs with lock revisions, "Update input", "Build", "Open dev shell", and outputs.
  3. **Builds:** a live queue and DAG view, streaming logs, cancel/retry.
  4. **Store browser:** search paths, closure size, a dependency graph, `why-depends`, verify/repair.
  5. **Profiles and generations:** a list, diff between generations, rollback.
  6. **Garbage collection:** a preview of what would be freed, the roots list, run GC/optimise.
  7. **Caches and settings:** substituters, keys, store root, parallelism.
- **Extras:** tray icon with build notifications; a file watcher on project declarations that marks them as "needs rebuild".

| D | Deliverable | Status |
|---|---|---|
| **D1** | Skeleton app: sidebar (Projects, Store, Profiles, Settings), activity log, project registry (add existing / create new / remove), detail view (lock status, inputs, packages, dev shell), `neki.toml` editor that validates before saving, Build / Update lock / store / profile actions wired to `neki-core` | ✅ (actions show "planned for M<n>" until the backend milestones land) |
| **D2** | Native folder picker (e.g. `rfd`), unsaved-changes prompt when switching projects | ✅ |
| **D3** | Builds page: live queue, per-derivation logs, cancel (needs M3) | ✅ |
| **D4** | Store browser: path search, closure size, references graph, `why-depends` (needs M2) | ✅ (the references graph is a dependency tree) |
| **D5** | Generations list and diff, GC preview (needs M6) | ✅ (also install/remove/switch) |
| **D6** | "Open dev shell" in a terminal window (needs M5; launching a terminal is OS-specific, so it goes through `neki-platform`) | ✅ |

Of the extras above, the project list shows each project's build state ("not built", "built", "changed since the last build"), re-checked every few seconds. Not done (see Future): the tray icon, a syntax-highlighted / structured `neki.toml` editor, a graphical DAG view and retrying builds, and `repair`.

---

## Future (not cross-platform yet)

Parked until a solution exists that behaves the same on Windows, macOS and Linux (or a deliberate per-OS exception is accepted):

- Real build sandboxes (namespaces / sandbox-exec / AppContainer) and network blocking for builds (P1)
- Scoop / winget / Homebrew manifest importers (P2)
- DLL hardlinking (Windows), RPATH patching (Unix) (P4)
- Windows symlinks via Developer Mode (P5). Until then, unpacking a NAR that contains a symlink fails on Windows with a clear error
- Executable bit on Windows (P10): Windows has no such bit, so NARs made there mark nothing executable, and a tree with executables hashes differently than on Unix. Fix: carry the bit from archive metadata (tar/zip) straight into the NAR in the unpacker. (Packages are per-system anyway, so this only matters for sources shared between OSes)
- Tray icon with build notifications: needs `libappindicator`/StatusNotifier on Linux, which isn't available everywhere
- Syntax-highlighted or form-based `neki.toml` editor, a graphical build DAG, retrying a failed build from the Builds page: Slint has no code editor widget; the rest is UI work for later
- `neki store repair` (re-fetch or rebuild modified paths in place)
- Uploading to HTTP caches (only `file://` caches are written; serve one with `neki store serve`)
- Ad-hoc `neki shell` environments have no GC root, so a garbage collection while the shell is open can delete them
- Language adapters: npm packages with install scripts, optional/platform-specific npm dependencies, Python dependencies with native code (sdists), and Cargo `git` dependencies aren't supported
- MSI/EXE installers, registry, services, drivers (P6)
- LLVM-mingw stdenv, MSVC as an impure input (P7)
- ACL / `chattr` hard immutability (P8)
- Relocatable store root (P9)
- Escaping non-portable file names (P10)
- Daemon and multi-user mode (P11), so the GUI can keep builds running after its window closes
- cmd / fish / Nushell activation, direnv-style auto-activation (P12)
- Windows delete-on-reboot and a Defender exclusion helper (P14)
- OS-level declarative configuration (P15)

---

## Verification

- **Golden-vector tests** (per crate, in the crates' tests): Nix base32, store paths, NAR bytes, `.drv` text and the `.drv` path of a `.neki` expression, each cross-checked against an independent implementation (openssl, Python).
- **Integration tests** (`neki-core`): a temporary store per test for build → GC → rebuild, the lock file, environments, profiles, and push → substitute between two stores sharing a store dir.
- **CI matrix** (`ci.yml`): windows-latest, macos-latest, ubuntu-latest run every test, including the junction, Job Object and `cmd.exe` paths on Windows.
- **End-to-end** (`pkgs.yml`): the example projects build with the real package set, run, and rebuild to the same NAR hashes after their outputs are deleted, on each OS. Done by hand on macOS (Intel); the workflow hasn't run yet. Two different machines of the same OS haven't been compared yet.
- **Phase 2:** a headless smoke test of the real window (`i-slint-backend-testing`), plus a manual run of `neki-desktop` against the example projects (every page, on macOS).

