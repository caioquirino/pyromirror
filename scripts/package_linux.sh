#!/usr/bin/env bash
# Turns dist/linux-x86_64 (from build_linux_x86_64.sh) into .deb, .rpm and Arch packages plus the
# AUR recipe, in dist/packages/. Needs nfpm (https://nfpm.goreleaser.com) in PATH or as $NFPM.
# Usage: scripts/package_linux.sh <version>
set -euo pipefail

VERSION="${1:?usage: package_linux.sh <version>}"
NFPM="${NFPM:-nfpm}"

cd "$(dirname "${BASH_SOURCE[0]}")/.."
[ -x dist/linux-x86_64/pyromirror ] || { echo "error: run scripts/build_linux_x86_64.sh first" >&2; exit 1; }

OUT=dist/packages
mkdir -p "$OUT"
export VERSION

for packager in deb rpm archlinux; do
    "$NFPM" package --config packaging/nfpm.yaml --packager "$packager" --target "$OUT/"
done

# AUR recipe for this version; it installs the release tarball published on GitHub.
TARBALL="pyromirror-$VERSION-linux-x86_64.tar.gz"
if [ -f "$TARBALL" ]; then
    SHA256="$(sha256sum "$TARBALL" | cut -d' ' -f1)"
    # Arch versions cannot contain a hyphen.
    sed -e "s/@PKGVER@/${VERSION//-/_}/" -e "s/@VERSION@/$VERSION/" -e "s/@SHA256@/$SHA256/" \
        packaging/aur/PKGBUILD.in > "$OUT/PKGBUILD"
fi

ls -lh "$OUT"
