#!/usr/bin/env bash
# Print the version of tonight's nightly build: the patch after the last release, with the date.
#
#   ./scripts/nightly-version.sh     # e.g. v0.1.1-nightly.20261021 (after release v0.1.0)
#
# - No release tag yet: v0.1.0-nightly.<date>
# - Otherwise: the last vMAJOR.MINOR.PATCH tag with the patch number + 1. Nightly builds never
#   create version tags; releases are made by hand with the Release workflow (release.yml).
#
# Exits 1 if there is nothing new to build: no commits, or only doc changes (see
# scripts/code-changes.sh for what counts as docs) since the last nightly build (the `nightly`
# tag) or the last release, whichever is newer.
set -euo pipefail

cd "$(dirname "$0")/.."

# The newest version tag reachable from HEAD (by history, not by string order, so v0.1.10 beats v0.1.9).
last_release="$(git describe --tags --abbrev=0 --match 'v[0-9]*.[0-9]*.[0-9]*' 2>/dev/null || true)"
# Compare with the newer of the last nightly build and the last release.
since="$(git rev-parse -q --verify 'refs/tags/nightly^{commit}' || true)"
if [ -z "$since" ] || { [ -n "$last_release" ] && git merge-base --is-ancestor "$since" "$last_release"; }; then
    since="$last_release"
fi

if [ -n "$since" ]; then
    if [ -z "$(git log --format=%H "$since..HEAD")" ]; then
        echo "no commits since ${since:0:12}, nothing to build" >&2
        exit 1
    fi
    if [ -z "$(./scripts/code-changes.sh "$since" HEAD)" ]; then
        echo "only docs changed since ${since:0:12}, nothing to build" >&2
        exit 1
    fi
fi

if [ -z "$last_release" ]; then
    next="v0.1.0"
else
    IFS=. read -r major minor patch <<< "${last_release#v}"
    next="v$major.$minor.$((patch + 1))"
fi
echo "$next-nightly.$(date -u +%Y%m%d)"
