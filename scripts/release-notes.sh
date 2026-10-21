#!/usr/bin/env bash
# Print the part of a GitHub release's notes that says which archive to download.
#
#   ./scripts/release-notes.sh v0.2.0
#
# Used by release.yml and nightly-release.yml; the archive names match package-release.sh.
set -euo pipefail

version="${1:?usage: $0 <version, e.g. v0.2.0>}"

cat <<EOF
Download the archive for your system:
- Windows (x86_64): \`neki-$version-windows-x86_64.zip\`
- macOS, Apple Silicon: \`neki-$version-macos-arm64.tar.gz\`
- macOS, Intel: \`neki-$version-macos-x86_64.tar.gz\`
- Linux (x86_64): \`neki-$version-linux-x86_64.tar.gz\`

Each archive has \`neki\` (the CLI), \`neki-desktop\` (Neki Desktop App) and \`neki-shim\`.
On Linux, Neki Desktop App needs fontconfig and libxkbcommon installed.
The builds aren't signed or notarized: on macOS, run \`xattr -dr com.apple.quarantine <unpacked folder>\` once if it's blocked; on Windows, SmartScreen may warn on first launch.
EOF
