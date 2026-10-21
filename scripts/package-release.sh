#!/usr/bin/env bash
# Build a release of Neki for this machine's OS and architecture, and archive it for a GitHub release.
#
#   ./scripts/package-release.sh v0.1.0
#     -> dist/neki-v0.1.0-macos-arm64.tar.gz      (macOS, Linux: .tar.gz)
#     -> dist/neki-v0.1.0-windows-x86_64.zip      (Windows, from Git Bash: .zip)
#   ./scripts/package-release.sh v0.1.1-nightly.20261021   (a nightly build's version)
#
# The archive holds the `neki` CLI, Neki Desktop App (`neki-desktop`), the `neki-shim` launcher,
# README.md and LICENSE.
# Builds are native only: each platform is built on a machine of that platform. The last line of
# stdout is the archive path. The release version is written into Cargo.toml for the build (so
# `neki --version` matches the tag) and Cargo.toml and Cargo.lock are restored afterwards.
set -euo pipefail

cd "$(dirname "$0")/.."

version="${1:-}"
case "$version" in
    v[0-9]*.[0-9]*.[0-9]*) ;;
    *)
        echo "usage: $0 <version tag, e.g. v0.1.0>" >&2
        exit 2
        ;;
esac

case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    MINGW* | MSYS* | CYGWIN*) os=windows ;;
    *)
        echo "unsupported OS: $(uname -s)" >&2
        exit 1
        ;;
esac
case "$(uname -m)" in
    arm64 | aarch64) arch=arm64 ;;
    x86_64 | amd64) arch=x86_64 ;;
    *)
        echo "unsupported architecture: $(uname -m)" >&2
        exit 1
        ;;
esac
exe=""
if [ "$os" = windows ]; then exe=".exe"; fi

root="$(pwd)"
backup="$(mktemp -d)"
cp Cargo.toml Cargo.lock "$backup/"
# Absolute destination: the script changes directory before it exits.
trap 'cp "$backup/Cargo.toml" "$backup/Cargo.lock" "$root/" && rm -rf "$backup"' EXIT

# The only line starting with `version =` is in [workspace.package]. Cargo updates the workspace
# crates' versions in Cargo.lock by itself during the build; other dependencies stay locked.
sed -i.bak "s/^version = \".*\"/version = \"${version#v}\"/" Cargo.toml
rm Cargo.toml.bak
cargo build --release -p neki-cli -p neki-gui -p neki-shim >&2

name="neki-$version-$os-$arch"
rm -rf "dist/$name" "dist/$name.zip" "dist/$name.tar.gz"
mkdir -p "dist/$name"
for bin in neki neki-desktop neki-shim; do
    cp "target/release/$bin$exe" "dist/$name/"
done
cp README.md LICENSE "dist/$name/"

cd dist
if [ "$os" = windows ]; then
    powershell -NoProfile -Command "Compress-Archive -Path '$name' -DestinationPath '$name.zip'"
    echo "dist/$name.zip"
else
    tar -czf "$name.tar.gz" "$name"
    echo "dist/$name.tar.gz"
fi
