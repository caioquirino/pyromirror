#!/usr/bin/env bash
# Sets the version of every PyroMirror crate (they all inherit it from the workspace).
# Usage: scripts/set_version.sh 1.2.3
set -euo pipefail

VERSION="${1:?usage: set_version.sh <version>}"

# Semantic Versioning 2.0.0: MAJOR.MINOR.PATCH with optional -prerelease and +build parts.
SEMVER='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$'
if ! [[ "$VERSION" =~ $SEMVER ]]; then
    echo "error: '$VERSION' is not a semantic version (expected e.g. 1.2.3 or 1.2.3-beta.1)" >&2
    exit 1
fi

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# The first `version = "..."` line in the root manifest is [workspace.package]'s.
sed -i -E "0,/^version = \".*\"/s//version = \"$VERSION\"/" Cargo.toml
grep -q "^version = \"$VERSION\"" Cargo.toml || { echo "error: could not set the version in Cargo.toml" >&2; exit 1; }

# Keep Cargo.lock's entries for our own crates in step, without touching any dependency.
cargo update --workspace --quiet

echo "PyroMirror version set to $VERSION"
