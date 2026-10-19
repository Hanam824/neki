# Plan: a cross-platform, Nix-style package manager written in Rust (Windows-first)

**Name: Neki** (binary `neki`, crates `neki-*`; decided 2026-10-19). **Nix compatibility:** Nix is the design spec only; byte-compatibility with real Nix is *not* a goal and Nix is not needed for development (decided 2026-10-19). Store root is `C:\neki\store` / `/neki/store`.

## Context

Nix gives reproducible, declarative, per-project environments. It does this with an immutable, content-addressed store (`/nix/store/<hash>-name`), derivations (pure build recipes), and closures. Officially, though, it only runs on Linux and macOS. Its design depends on Unix features that Windows doesn't have: build sandboxing (user namespaces), free symlinks, RPATH, and a POSIX shell. The goal is to rebuild Nix's architecture in Rust so it also works natively on Windows, using the same concepts:

- **Phase 1:** the core backend. This covers the store, derivations, declarative per-project files, the builder, binary caches, profiles, garbage collection and a CLI.
- **Phase 2:** a desktop app (Rust + Slint) to inspect and control every project's declaration, edit it, build, update locks and track the store.

The repo started empty, so this is a greenfield project. The first implementation step also writes `CLAUDE.md`, which finishes the earlier `/init` request.

---

## Part A: Problems that can't be implemented as Nix does them (you decide: ignore, or accept the proposed alternative)

| # | Problem | Why it's hard on Windows | Proposed alternative |
|---|---|---|---|
| P1 | **Pure build sandbox** (Linux namespaces, chroot, no network) | Windows has no user namespaces, chroot or bind mounts. AppContainer and Job Objects are partial. Windows Sandbox and Hyper-V are too heavy. | **Best-effort purity**: a scrubbed environment, a private temp dir, a Job Object (kills the process tree, sets limits), a cwd inside the build dir, and a `PATH` that contains only declared inputs. Network is blocked only for non-fixed-output builds, through an opt-in AppContainer later. Mark builds as "impure-capable". |
| P2 | **Reusing nixpkgs and cache.nixos.org** | nixpkgs packages are Linux/macOS ELF/Mach-O binaries and the Windows support in nixpkgs is minimal (mostly mingw cross builds). None of the 100k packages can be reused. | Build **our own package set**. Bootstrap it by importing **Scoop / winget manifests** (they already contain url + sha256 + bin), wrapped as fixed-output derivations. Build from source only for a small core. |
| P3 | **Nix language plus nixpkgs compatibility** | A full lazy Nix evaluator is large, and nixpkgs depends on hundreds of builtins and stdenv/bash. | Write our own evaluator on top of **`rnix-parser` (MIT)**, covering the core language and the important builtins, with **no** nixpkgs compatibility. Alternatively embed `snix-eval`, but it is **GPL-3**, which affects the project's license. Also offer a simple `neki.toml` front end for easy projects. |
| P4 | **No RPATH / hardcoded library paths** | Windows looks up DLLs in the exe dir, then the system dirs, then `PATH`. A binary can't point at `C:\neki\store\...\lib`. | Use **launcher shims** (small Rust exe stubs) that set `PATH`/`SetDllDirectory` and then run the real exe. When building a profile, **hardlink the dependency DLLs next to the exe**. |
| P5 | **Symlinks** (profiles, GC roots, `result` links) | Creating a symlink needs Developer Mode or admin. | **Directory junctions** for dirs, **hardlinks** for files, and shims for exes. Use real symlinks only when the privilege is detected. GC roots go in the DB, not in symlinks. |
| P6 | **MSI/EXE installers, registry, COM, services, drivers** | They write global machine state. That isn't reproducible and doesn't fit in a store path. | **Ignore** registry/services/drivers. Extract MSI with an administrative install (`msiexec /a`) or the `msi` crate, and extract self-extractors with 7z. Packages that need global installation are unsupported, or flagged "impure: system". |
| P7 | **MSVC toolchain in the store** | The Visual Studio Build Tools license forbids redistributing them, so they can't go into a binary cache. | Use **LLVM-mingw / Zig / Rust GNU** as the redistributable toolchains in the store. Treat MSVC as an **impure system input** that's detected and hashed by version. |
| P8 | **Store immutability** (`chattr`, read-only bind mount) | No equivalent. | Set the read-only attribute plus a **deny-write ACL** for the user, and verify by hash on demand (`neki store verify`). |
| P9 | **Path rewriting when the store root moves** (`C:\neki` vs `D:\neki`) | Hashes embed the store path, so a different root means different hashes and no cache sharing. | **Fix the store root** at `C:\neki\store`, so hashes are platform-independent apart from `system`. A custom root is allowed but loses the substituter. |
| P10 | **Windows filesystem quirks** | Case-insensitive, `MAX_PATH`=260, reserved names (`CON`, `NUL`, `AUX`…), illegal chars (`:`), trailing dots, no exec bit. | Use lowercase Nix-base32 hashes (no case collisions). Enable long paths (manifest + `\\?\` prefix). **Reject** NAR entries with reserved names, or escape them. Keep the exec bit as metadata in the NAR only. |
| P11 | **Multi-user daemon** (build users `nixbld1..N`, Unix socket) | No build users. The daemon would have to be a Windows service with named pipes. | **Single-user mode first.** Add a daemon (Windows service + named pipe) as a later milestone; the Phase 2 GUI needs it anyway. |
| P12 | **Shell environments** (`nix-shell`, `nix develop`, direnv) | Depend on bash. Windows `PATH` and the environment block have size limits. | Generate activation scripts for **PowerShell, cmd and Nushell**, plus `neki shell` (spawns a child shell). Keep `PATH` short by using one merged `bin` dir per environment (a profile "buildEnv"). |
| P13 | **Runtime reference scanning** | Nix scans output bytes for store hashes. PE files often contain **UTF-16LE** strings, so a byte scan misses them. | Scan for **both UTF-8 and UTF-16LE** encodings of each candidate hash. |
| P14 | **Locked files and antivirus** | GC can't delete files that are in use, and Defender slows extraction a lot. | Retry/defer deletion (the `MoveFileEx` delay-until-reboot fallback). Document a Defender exclusion for `C:\neki`. |
| P15 | **NixOS / nix-darwin** (OS-level declarative config) | Out of scope on Windows. | **Ignore.** A small "user environment" module (dotfiles, env vars) may come later. |
| P16 | **CRLF / git autocrlf** changing source hashes | The same repo hashes differently. | `fetchgit` always checks out with `core.autocrlf=false`, and path imports hash raw bytes. |

**Recommended defaults** (assumed unless you change them): P1 best-effort, P2 Scoop-manifest import, P3 own evaluator on rnix-parser plus a TOML front end, P4 shims, P5 junctions/hardlinks, P6 ignore, P7 LLVM-mingw, P9 fixed root, P11 single-user first, P15 ignore.

---

## Part B: Nix features we can support

**Store and formats (identical to Nix):**
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
- A per-project `flake.neki` (or `neki.toml`) with `inputs` / `outputs` (`packages`, `devShells`, `apps`)
- A lock file (`neki.lock`, the same JSON shape as `flake.lock`) and `neki lock --update-input`
- Dev shells: `neki develop`, plus a direnv-like auto-activation hook for PowerShell
- `neki build` → a `result` junction, `neki run`, `neki shell pkgA pkgB` (ad hoc)

**Fetchers / builders:**
- `fetchurl`, `fetchzip` (zip/tar.gz/tar.xz/7z), `fetchgit`, `fetchFromGitHub`, `fetchMsi`
- Builders: `runCommand` (PowerShell/cmd/Rust-native), `mkDerivation` with phases (unpack/patch/configure/build/install) for the LLVM-mingw/CMake/Cargo toolchains
- **Foreign package manager adapters** (the "control all package managers" goal): pinned npm/pip/cargo/go dependencies as fixed-output derivations, generated from their lock files (like `buildNpmPackage`/`buildRustPackage`), plus Scoop/winget manifest import into package recipes

**Distribution:**
- Binary caches: `file://` and `https://` substituters, `.narinfo`, `nar.xz`/`nar.zst`, ed25519 signatures, `trusted-public-keys`
- `neki copy --to`, `neki store serve` (a simple HTTP cache server)

**User environments:**
- Profiles with generations, `profile install/remove/upgrade/rollback/history`
- GC roots (per project, profile and `result`), `neki gc`, `--delete-older-than`

**Later:** a daemon (Windows service), remote builders over SSH, CA derivations, cross-platform builds for Linux/macOS hosts (the same Rust core runs natively there too).

---

## Part C: Architecture (Cargo workspace)

```
Nix-design/
├─ Cargo.toml                 # workspace
├─ crates/
│  ├─ neki-hash/              # sha256/sha1/blake3, nix-base32, SRI, store-path computation
│  ├─ neki-nar/               # NAR read/write (streaming), Windows-name validation
│  ├─ neki-derivation/        # Derivation model, ATerm (de)serialize, hashDerivationModulo
│  ├─ neki-store/             # LocalStore: layout, SQLite DB, locks, add/query, refs scan, GC, optimise, ACL read-only
│  ├─ neki-fetch/             # http (reqwest), git (gix), unpack zip/tar/7z/msi, hash verify
│  ├─ neki-build/             # build scheduler (DAG, parallel jobs), executor, Job Object isolation, env scrubbing, logs
│  ├─ neki-substitute/        # binary-cache client/server, narinfo, signatures (ed25519-dalek)
│  ├─ neki-eval/              # rnix-parser → AST → lazy evaluator, builtins, string context
│  ├─ neki-project/           # flake.neki / neki.toml loading, lock file, inputs resolution
│  ├─ neki-profile/           # profiles, generations, buildEnv merge, shims, shell activation scripts
│  ├─ neki-shim/              # tiny launcher exe (sets PATH/DLL dirs, execs target)
│  ├─ neki-core/              # facade API used by CLI + GUI (one place for high-level ops + events)
│  ├─ neki-cli/               # `neki` binary (clap)
│  └─ neki-gui/               # Phase 2 – Slint app
├─ pkgs/                      # our package set (Nix-language), incl. Scoop-imported recipes
└─ tests/                     # integration fixtures (golden NAR/drv/hash vectors)
```

**Platform abstraction:** a `platform` module in `neki-store`/`neki-build` with `windows`, `unix` and `macos` implementations (linking: junction vs symlink, read-only: ACL vs chmod, isolation: Job Object vs namespaces), so the same core runs on all three OSes.

**Key crates:** `sha2`, `data-encoding`, `rusqlite` (bundled), `reqwest` (rustls), `gix`, `zip`, `tar`, `xz2`/`zstd`, `sevenz-rust`, `ed25519-dalek`, `rnix-parser`, `clap`, `tokio`, `tracing`, `windows` (Win32 Job Objects/ACL/junctions), `fs4` (locking), `serde`/`serde_json`/`toml`, and `slint` (Phase 2).

**Format stability:** Neki's own golden vectors (`tests/fixtures/`) define NAR bytes, hashes, `.drv` text and narinfo; changing them is a breaking change.

---

## Part D: Phase 1 milestones (each ends in a working, tested slice)

| M | Deliverable | Exit criteria |
|---|---|---|
| **M0** | Workspace scaffold, CI (GitHub Actions: windows/macos/ubuntu), `CLAUDE.md`, lint (`clippy -D warnings`, `fmt`) | `cargo test --workspace` green on all 3 OSes |
| **M1** | `neki-hash` + `neki-nar` | NAR + base32 + store path match committed golden vectors; `neki hash path/file` |
| **M2** | `neki-store` (local store, DB, `add-to-store`, refs, read-only, locks) | `neki store add <dir>` → `C:\neki\store\<hash>-name`, re-adding is idempotent, `path-info`/`verify` work |
| **M3** | `neki-derivation` + `neki-build` (executor, Job Object, scheduler, logs) + `neki-fetch` (fetchurl/zip) | Hand-written `.drv` builds; a fixed-output fetch verifies the hash; deps build in DAG order, in parallel |
| **M4** | `neki-eval` v1 (core language + `derivation` + fetch builtins) and `neki.toml` front end | `neki build` from a `.neki` expression produces a deterministic `.drv` path (golden-tested) |
| **M5** | `neki-project` (flake-like inputs/outputs, lock file) + `develop`/`shell`/`run` + PowerShell/cmd activation + shims | In a sample project, `neki develop` gives a shell with pinned tools (e.g. Node, Python, Go from Scoop-imported recipes) |
| **M6** | `neki-profile` (profiles, generations, rollback) + GC (roots, `--delete-older-than`) + optimise | Install → upgrade → rollback works; GC removes only unreachable paths |
| **M7** | `neki-substitute` (file/http cache, signing, `copy`, `store serve`) | Machine A builds and pushes, machine B substitutes with a verified signature |
| **M8** | `pkgs/` starter set (~50 packages: Scoop importer + LLVM-mingw stdenv + Cargo/npm/pip adapters) | Example projects for Rust, Node and Python build reproducibly on Windows |
| **M9** | `neki-core` facade with an event stream (progress, logs) + JSON output for every CLI command | The GUI can drive every operation through the API, without the CLI |

## Part E: Phase 2, the desktop app (Rust + Slint)

- **Architecture:** `neki-gui` links `neki-core` directly in v1. When the daemon (named pipe) exists, the GUI connects to it so builds keep running after the window closes. Slint UI runs on the main thread, operations run on a tokio runtime, and events come back through `slint::invoke_from_event_loop`.
- **Screens:**
  1. **Projects:** register or discover project folders, with status for each (lock outdated, build state, last build).
  2. **Project detail:** a declaration editor (syntax-highlighted text plus a structured form for `neki.toml`), inputs with lock revisions, "Update input", "Build", "Open dev shell", and outputs.
  3. **Builds:** a live queue and DAG view, streaming logs, cancel/retry.
  4. **Store browser:** search paths, closure size, a dependency graph, `why-depends`, verify/repair.
  5. **Profiles and generations:** a list, diff between generations, rollback.
  6. **Garbage collection:** a preview of what would be freed, the roots list, run GC/optimise.
  7. **Caches and settings:** substituters, keys, store root, parallelism, Defender-exclusion helper.
- **Extras:** tray icon with build notifications; a file watcher on project declarations that marks them as "needs rebuild".

---

## Verification

- **Golden-vector tests** (per crate): fixtures committed under `tests/fixtures`; `cargo test -p neki-nar` etc.
- **Integration tests:** a temporary store root per test (`NEKI_STORE_DIR` override) for the cycles build → GC → rebuild and push → substitute.
- **CI matrix:** windows-latest, macos-latest, ubuntu-latest. Windows jobs run the junction/ACL/Job Object paths.
- **End-to-end:** the example projects in `examples/` (rust-hello, node-app, python-tool) must `neki build` with identical output hashes on two clean machines.
- **Phase 2:** Slint UI smoke tests (`slint::testing`), plus a manual run of `cargo run -p neki-gui` against the example projects.
