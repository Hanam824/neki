#!/usr/bin/env python3
"""Regenerate pkgs/sources.json: the newest release of every package in the
package set, with the URL and SHA-256 of its archive for each system.

Only checksums published by upstream are used (GitHub release asset
digests, SHASUMS files, release JSON), so nothing has to be downloaded.
GitHub requests go through `gh api` (authenticated) when `gh` is
installed, otherwise to the public API (60 requests an hour).

Usage: scripts/update-pkgs.py [package ...]   (default: all)
"""

import json
import re
import shutil
import subprocess
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "pkgs" / "sources.json"
SYSTEMS = ["aarch64-darwin", "x86_64-darwin", "x86_64-linux", "aarch64-linux", "x86_64-windows"]


def get(url, accept=None):
    request = urllib.request.Request(url, headers={"User-Agent": "neki-update-pkgs"})
    if accept:
        request.add_header("Accept", accept)
    with urllib.request.urlopen(request, timeout=60) as response:
        return response.read().decode()


def github(path):
    if shutil.which("gh"):
        return json.loads(subprocess.run(["gh", "api", path], check=True, capture_output=True, text=True).stdout)
    return json.loads(get(f"https://api.github.com/{path}", "application/vnd.github+json"))


def sha256_digest(asset):
    digest = asset.get("digest") or ""
    if not digest.startswith("sha256:"):
        raise SystemExit(f"{asset['name']}: no sha256 digest")
    return digest[len("sha256:"):]


# GitHub-released packages: repo, then a regex per system matching the asset
# name. Options: `install` (the asset is a bare program, installed as
# bin/<install>), `tag` (a regex the release tag must match; default the
# latest release), `version` (regex extracting the version from the tag).
def rust_triples(prefix=r"", gz=r"\.tar\.gz", win=r"\.zip", linux="musl", arm_linux="(gnu|musl)", win_target="msvc"):
    return {
        "aarch64-darwin": rf"{prefix}aarch64-apple-darwin{gz}$",
        "x86_64-darwin": rf"{prefix}x86_64-apple-darwin{gz}$",
        "x86_64-linux": rf"{prefix}x86_64-unknown-linux-{linux}{gz}$",
        "aarch64-linux": rf"{prefix}aarch64-unknown-linux-{arm_linux}{gz}$",
        "x86_64-windows": rf"{prefix}x86_64-pc-windows-{win_target}{win}$",
    }


def go_style(name, gz=r"\.tar\.gz", win=r"\.zip", darwin="darwin", amd64="amd64", arm64="arm64", sep="_"):
    return {
        "aarch64-darwin": rf"{name}{sep}{darwin}{sep}{arm64}{gz}$",
        "x86_64-darwin": rf"{name}{sep}{darwin}{sep}{amd64}{gz}$",
        "x86_64-linux": rf"{name}{sep}linux{sep}{amd64}{gz}$",
        "aarch64-linux": rf"{name}{sep}linux{sep}{arm64}{gz}$",
        "x86_64-windows": rf"{name}{sep}windows{sep}{amd64}{win}$",
    }


GITHUB = {
    "ripgrep": ("BurntSushi/ripgrep", rust_triples(linux="musl", arm_linux="gnu")),
    "fd": ("sharkdp/fd", rust_triples(linux="musl", arm_linux="(gnu|musl)")),
    "bat": ("sharkdp/bat", rust_triples(linux="musl", arm_linux="(gnu|musl)")),
    "hyperfine": ("sharkdp/hyperfine", rust_triples(linux="musl", arm_linux="(gnu|musl)")),
    "delta": ("dandavison/delta", rust_triples(linux="musl", arm_linux="gnu")),
    "just": ("casey/just", rust_triples(linux="musl", arm_linux="musl")),
    "uv": ("astral-sh/uv", rust_triples(prefix=r"uv-", linux="musl", arm_linux="musl")),
    "ruff": ("astral-sh/ruff", rust_triples(prefix=r"ruff-", linux="musl", arm_linux="musl")),
    "starship": ("starship/starship", rust_triples(prefix=r"starship-", linux="musl", arm_linux="musl")),
    "zoxide": ("ajeetdsouza/zoxide", rust_triples(linux="musl", arm_linux="musl")),
    "watchexec": ("watchexec/watchexec", rust_triples(gz=r"\.tar\.xz", linux="musl", arm_linux="musl")),
    "typos": ("crate-ci/typos", rust_triples(linux="musl", arm_linux="musl")),
    "dust": ("bootandy/dust", rust_triples(linux="musl", arm_linux="musl")),
    "sd": ("chmln/sd", rust_triples(linux="musl", arm_linux="musl")),
    "xh": ("ducaale/xh", rust_triples(linux="musl", arm_linux="musl")),
    "deno": ("denoland/deno", {
        "aarch64-darwin": r"^deno-aarch64-apple-darwin\.zip$",
        "x86_64-darwin": r"^deno-x86_64-apple-darwin\.zip$",
        "x86_64-linux": r"^deno-x86_64-unknown-linux-gnu\.zip$",
        "aarch64-linux": r"^deno-aarch64-unknown-linux-gnu\.zip$",
        "x86_64-windows": r"^deno-x86_64-pc-windows-msvc\.zip$",
    }),
    "bun": ("oven-sh/bun", {
        "aarch64-darwin": r"^bun-darwin-aarch64\.zip$",
        "x86_64-darwin": r"^bun-darwin-x64\.zip$",
        "x86_64-linux": r"^bun-linux-x64\.zip$",
        "aarch64-linux": r"^bun-linux-aarch64\.zip$",
        "x86_64-windows": r"^bun-windows-x64\.zip$",
    }),
    "gh": ("cli/cli", {
        "aarch64-darwin": r"^gh_[\d.]+_macOS_arm64\.zip$",
        "x86_64-darwin": r"^gh_[\d.]+_macOS_amd64\.zip$",
        "x86_64-linux": r"^gh_[\d.]+_linux_amd64\.tar\.gz$",
        "aarch64-linux": r"^gh_[\d.]+_linux_arm64\.tar\.gz$",
        "x86_64-windows": r"^gh_[\d.]+_windows_amd64\.zip$",
    }),
    "fzf": ("junegunn/fzf", {
        "aarch64-darwin": r"^fzf-[\d.]+-darwin_arm64\.(tar\.gz|zip)$",
        "x86_64-darwin": r"^fzf-[\d.]+-darwin_amd64\.(tar\.gz|zip)$",
        "x86_64-linux": r"^fzf-[\d.]+-linux_amd64\.tar\.gz$",
        "aarch64-linux": r"^fzf-[\d.]+-linux_arm64\.tar\.gz$",
        "x86_64-windows": r"^fzf-[\d.]+-windows_amd64\.zip$",
    }),
    "lazygit": ("jesseduffield/lazygit", {
        "aarch64-darwin": r"_[Dd]arwin_arm64\.tar\.gz$",
        "x86_64-darwin": r"_[Dd]arwin_x86_64\.tar\.gz$",
        "x86_64-linux": r"_[Ll]inux_x86_64\.tar\.gz$",
        "aarch64-linux": r"_[Ll]inux_arm64\.tar\.gz$",
        "x86_64-windows": r"_[Ww]indows_x86_64\.zip$",
    }),
    "golangci-lint": ("golangci/golangci-lint", go_style(r"golangci-lint-[\d.]+", sep="-")),
    "k9s": ("derailed/k9s", {
        "aarch64-darwin": r"^k9s_Darwin_arm64\.tar\.gz$",
        "x86_64-darwin": r"^k9s_Darwin_amd64\.tar\.gz$",
        "x86_64-linux": r"^k9s_Linux_amd64\.tar\.gz$",
        "aarch64-linux": r"^k9s_Linux_arm64\.tar\.gz$",
        "x86_64-windows": r"^k9s_Windows_amd64\.zip$",
    }),
    "neovim": ("neovim/neovim", {
        "aarch64-darwin": r"^nvim-macos-arm64\.tar\.gz$",
        "x86_64-darwin": r"^nvim-macos-x86_64\.tar\.gz$",
        "x86_64-linux": r"^nvim-linux-x86_64\.tar\.gz$",
        "aarch64-linux": r"^nvim-linux-arm64\.tar\.gz$",
        "x86_64-windows": r"^nvim-win64\.zip$",
    }),
    "shellcheck": ("koalaman/shellcheck", {
        "aarch64-darwin": r"\.darwin\.aarch64\.tar\.xz$",
        "x86_64-darwin": r"\.darwin\.x86_64\.tar\.xz$",
        "x86_64-linux": r"\.linux\.x86_64\.tar\.xz$",
        "aarch64-linux": r"\.linux\.aarch64\.tar\.xz$",
        "x86_64-windows": r"\.zip$",
    }),
    "protoc": ("protocolbuffers/protobuf", {
        "aarch64-darwin": r"^protoc-[\d.]+-osx-aarch_64\.zip$",
        "x86_64-darwin": r"^protoc-[\d.]+-osx-x86_64\.zip$",
        "x86_64-linux": r"^protoc-[\d.]+-linux-x86_64\.zip$",
        "aarch64-linux": r"^protoc-[\d.]+-linux-aarch_64\.zip$",
        "x86_64-windows": r"^protoc-[\d.]+-win64\.zip$",
    }),
    "cmake": ("Kitware/CMake", {
        "aarch64-darwin": r"^cmake-[\d.]+-macos-universal\.tar\.gz$",
        "x86_64-darwin": r"^cmake-[\d.]+-macos-universal\.tar\.gz$",
        "x86_64-linux": r"^cmake-[\d.]+-linux-x86_64\.tar\.gz$",
        "aarch64-linux": r"^cmake-[\d.]+-linux-aarch64\.tar\.gz$",
        "x86_64-windows": r"^cmake-[\d.]+-windows-x86_64\.zip$",
    }),
    "ninja": ("ninja-build/ninja", {
        "aarch64-darwin": r"^ninja-mac\.zip$",
        "x86_64-darwin": r"^ninja-mac\.zip$",
        "x86_64-linux": r"^ninja-linux\.zip$",
        "aarch64-linux": r"^ninja-linux-aarch64\.zip$",
        "x86_64-windows": r"^ninja-win\.zip$",
    }),
    "jq": ("jqlang/jq", {
        "aarch64-darwin": r"^jq-macos-arm64$",
        "x86_64-darwin": r"^jq-macos-amd64$",
        "x86_64-linux": r"^jq-linux-amd64$",
        "aarch64-linux": r"^jq-linux-arm64$",
        "x86_64-windows": r"^jq-windows-amd64\.exe$",
    }, {"install": "jq"}),
    "yq": ("mikefarah/yq", {
        "aarch64-darwin": r"^yq_darwin_arm64$",
        "x86_64-darwin": r"^yq_darwin_amd64$",
        "x86_64-linux": r"^yq_linux_amd64$",
        "aarch64-linux": r"^yq_linux_arm64$",
        "x86_64-windows": r"^yq_windows_amd64\.exe$",
    }, {"install": "yq"}),
    "shfmt": ("mvdan/sh", {
        "aarch64-darwin": r"_darwin_arm64$",
        "x86_64-darwin": r"_darwin_amd64$",
        "x86_64-linux": r"_linux_amd64$",
        "aarch64-linux": r"_linux_arm64$",
        "x86_64-windows": r"_windows_amd64\.exe$",
    }, {"install": "shfmt"}),
    "actionlint": ("rhysd/actionlint", go_style(r"actionlint_[\d.]+")),
    "direnv": ("direnv/direnv", {
        "aarch64-darwin": r"^direnv\.darwin-arm64$",
        "x86_64-darwin": r"^direnv\.darwin-amd64$",
        "x86_64-linux": r"^direnv\.linux-amd64$",
        "aarch64-linux": r"^direnv\.linux-arm64$",
        "x86_64-windows": r"^direnv\.windows-amd64\.exe$",
    }, {"install": "direnv"}),
}


def from_github(name, repo, patterns, options):
    release = github(f"repos/{repo}/releases/latest")
    tag = release["tag_name"]
    version = re.sub(r"^(bun-v|v|jq-|release-)", "", tag)
    systems = {}
    for system, pattern in patterns.items():
        assets = [a for a in release["assets"] if re.search(pattern, a["name"]) and not a["name"].endswith((".sha256", ".sig", ".asc", ".sbom", ".json"))]
        if not assets:
            print(f"  {name}: no asset for {system} ({pattern})", file=sys.stderr)
            continue
        asset = sorted(assets, key=lambda a: len(a["name"]))[0]
        if not (asset.get("digest") or "").startswith("sha256:"):
            print(f"  {name}: {asset['name']} has no published checksum; skipped", file=sys.stderr)
            continue
        systems[system] = {"url": asset["browser_download_url"], "sha256": sha256_digest(asset)}
    return {"version": version, "systems": systems, **options}


def checksums(text):
    """`<hex>  <file>` lines → {file: hex}."""
    out = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) == 2:
            out[parts[1].lstrip("*")] = parts[0]
    return out


def nodejs():
    index = json.loads(get("https://nodejs.org/dist/index.json"))
    version = next(r["version"] for r in index if r["lts"])
    sums = checksums(get(f"https://nodejs.org/dist/{version}/SHASUMS256.txt"))
    files = {
        "aarch64-darwin": f"node-{version}-darwin-arm64.tar.gz",
        "x86_64-darwin": f"node-{version}-darwin-x64.tar.gz",
        "x86_64-linux": f"node-{version}-linux-x64.tar.gz",
        "aarch64-linux": f"node-{version}-linux-arm64.tar.gz",
        "x86_64-windows": f"node-{version}-win-x64.zip",
    }
    return {
        "version": version.lstrip("v"),
        "systems": {s: {"url": f"https://nodejs.org/dist/{version}/{f}", "sha256": sums[f]} for s, f in files.items()},
    }


def go():
    release = json.loads(get("https://go.dev/dl/?mode=json"))[0]
    want = {
        "aarch64-darwin": ("darwin", "arm64"),
        "x86_64-darwin": ("darwin", "amd64"),
        "x86_64-linux": ("linux", "amd64"),
        "aarch64-linux": ("linux", "arm64"),
        "x86_64-windows": ("windows", "amd64"),
    }
    systems = {}
    for system, (os_, arch) in want.items():
        f = next(f for f in release["files"] if f["kind"] == "archive" and f["os"] == os_ and f["arch"] == arch)
        systems[system] = {"url": f"https://go.dev/dl/{f['filename']}", "sha256": f["sha256"]}
    return {"version": release["version"].removeprefix("go"), "systems": systems}


def python():
    release = github("repos/astral-sh/python-build-standalone/releases/latest")
    triples = {
        "aarch64-darwin": "aarch64-apple-darwin",
        "x86_64-darwin": "x86_64-apple-darwin",
        "x86_64-linux": "x86_64-unknown-linux-gnu",
        "aarch64-linux": "aarch64-unknown-linux-gnu",
        "x86_64-windows": "x86_64-pc-windows-msvc",
    }
    # The newest 3.13 release.
    names = [a["name"] for a in release["assets"]]
    version = sorted(
        {m.group(1) for n in names if (m := re.match(r"cpython-(3\.13\.\d+)\+\d+-", n))},
        key=lambda v: [int(x) for x in v.split(".")],
    )[-1]
    systems = {}
    for system, triple in triples.items():
        name = f"cpython-{version}+{release['tag_name']}-{triple}-install_only.tar.gz"
        asset = next(a for a in release["assets"] if a["name"] == name)
        systems[system] = {"url": asset["browser_download_url"], "sha256": sha256_digest(asset)}
    return {"version": version, "systems": systems}


def rust():
    """The stable toolchain's components; Windows uses the GNU target, whose
    rust-mingw component brings its own linker."""
    channel = get("https://static.rust-lang.org/dist/channel-rust-stable.toml")
    version = re.search(r'\[pkg\.rustc\]\nversion = "([\d.]+)', channel).group(1)
    triples = {
        "aarch64-darwin": "aarch64-apple-darwin",
        "x86_64-darwin": "x86_64-apple-darwin",
        "x86_64-linux": "x86_64-unknown-linux-gnu",
        "aarch64-linux": "aarch64-unknown-linux-gnu",
        "x86_64-windows": "x86_64-pc-windows-gnu",
    }
    systems = {}
    for system, triple in triples.items():
        components = ["rustc", "cargo", "rust-std"] + (["rust-mingw"] if "windows" in triple else [])
        parts = []
        for component in components:
            name = f"{component}-{version}-{triple}.tar.gz"
            url = f"https://static.rust-lang.org/dist/{name}"
            sha = get(url + ".sha256").split()[0]
            parts.append({"component": component, "url": url, "sha256": sha})
        systems[system] = {"target": triple, "components": parts}
    return {"version": version, "systems": systems}


def zig():
    index = json.loads(get("https://ziglang.org/download/index.json"))
    version = next(k for k in index if k != "master")
    keys = {
        "aarch64-darwin": "aarch64-macos",
        "x86_64-darwin": "x86_64-macos",
        "x86_64-linux": "x86_64-linux",
        "aarch64-linux": "aarch64-linux",
        "x86_64-windows": "x86_64-windows",
    }
    return {
        "version": version,
        "systems": {s: {"url": index[version][k]["tarball"], "sha256": index[version][k]["shasum"]} for s, k in keys.items()},
    }


def hashicorp(product):
    index = json.loads(get(f"https://api.releases.hashicorp.com/v1/releases/{product}?limit=10"))
    release = next(r for r in index if not r.get("is_prerelease"))
    version = release["version"]
    sums = checksums(get(f"https://releases.hashicorp.com/{product}/{version}/{product}_{version}_SHA256SUMS"))
    files = {
        "aarch64-darwin": "darwin_arm64",
        "x86_64-darwin": "darwin_amd64",
        "x86_64-linux": "linux_amd64",
        "aarch64-linux": "linux_arm64",
        "x86_64-windows": "windows_amd64",
    }
    systems = {}
    for system, suffix in files.items():
        f = f"{product}_{version}_{suffix}.zip"
        systems[system] = {"url": f"https://releases.hashicorp.com/{product}/{version}/{f}", "sha256": sums[f]}
    return {"version": version, "systems": systems}


def kubectl():
    version = get("https://dl.k8s.io/release/stable.txt").strip()
    paths = {
        "aarch64-darwin": "darwin/arm64/kubectl",
        "x86_64-darwin": "darwin/amd64/kubectl",
        "x86_64-linux": "linux/amd64/kubectl",
        "aarch64-linux": "linux/arm64/kubectl",
        "x86_64-windows": "windows/amd64/kubectl.exe",
    }
    systems = {}
    for system, path in paths.items():
        url = f"https://dl.k8s.io/release/{version}/bin/{path}"
        systems[system] = {"url": url, "sha256": get(url + ".sha256").split()[0]}
    return {"version": version.lstrip("v"), "systems": systems, "install": "kubectl"}


def helm():
    version = github("repos/helm/helm/releases/latest")["tag_name"]
    files = {
        "aarch64-darwin": "darwin-arm64.tar.gz",
        "x86_64-darwin": "darwin-amd64.tar.gz",
        "x86_64-linux": "linux-amd64.tar.gz",
        "aarch64-linux": "linux-arm64.tar.gz",
        "x86_64-windows": "windows-amd64.zip",
    }
    systems = {}
    for system, suffix in files.items():
        url = f"https://get.helm.sh/helm-{version}-{suffix}"
        systems[system] = {"url": url, "sha256": get(url + ".sha256sum").split()[0]}
    return {"version": version.lstrip("v"), "systems": systems}


def temurin():
    """Eclipse Temurin JDK 21 (LTS), from the Adoptium API."""
    systems = {}
    version = None
    want = {
        "aarch64-darwin": ("mac", "aarch64"),
        "x86_64-darwin": ("mac", "x64"),
        "x86_64-linux": ("linux", "x64"),
        "aarch64-linux": ("linux", "aarch64"),
        "x86_64-windows": ("windows", "x64"),
    }
    for system, (os_, arch) in want.items():
        assets = json.loads(get(
            f"https://api.adoptium.net/v3/assets/latest/21/hotspot?architecture={arch}&image_type=jdk&os={os_}&vendor=eclipse"
        ))
        package = assets[0]["binary"]["package"]
        version = assets[0]["version"]["openjdk_version"]
        systems[system] = {"url": package["link"], "sha256": package["checksum"]}
    return {"version": version, "systems": systems}


SPECIAL = {
    "nodejs": nodejs,
    "go": go,
    "python3": python,
    "rust": rust,
    "zig": zig,
    "terraform": lambda: hashicorp("terraform"),
    "packer": lambda: hashicorp("packer"),
    "kubectl": kubectl,
    "helm": helm,
    "jdk": temurin,
}


def main():
    wanted = sys.argv[1:] or sorted(list(SPECIAL) + list(GITHUB))
    sources = json.loads(OUT.read_text()) if OUT.exists() else {}
    for name in wanted:
        print(f"{name}...", file=sys.stderr)
        if name in SPECIAL:
            sources[name] = SPECIAL[name]()
        else:
            repo, patterns, *options = GITHUB[name]
            sources[name] = from_github(name, repo, patterns, options[0] if options else {})
    sources = {k: v for k, v in sources.items() if v["systems"]}
    OUT.write_text(json.dumps(dict(sorted(sources.items())), indent=2) + "\n")
    print(f"wrote {OUT.relative_to(ROOT)}", file=sys.stderr)


if __name__ == "__main__":
    main()
