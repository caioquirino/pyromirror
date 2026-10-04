#!/usr/bin/env bash
# Builds the Windows installer (MSI) from a folder with the release files, using wixl from
# msitools (Arch: pacman -S msitools; Debian/Ubuntu: apt install wixl).
# Usage: scripts/package_windows.sh <version> [source-dir] [output.msi]
set -euo pipefail

VERSION="${1:?usage: package_windows.sh <version> [source-dir] [output.msi]}"
cd "$(dirname "${BASH_SOURCE[0]}")/.."
SOURCE="${2:-dist/windows-x86_64}"
OUTPUT="${3:-dist/pyromirror-$VERSION-windows-x86_64.msi}"

[ -f "$SOURCE/pyromirror.exe" ] || { echo "error: $SOURCE has no pyromirror.exe; build first" >&2; exit 1; }

# Windows Installer versions are purely numeric: 1.2.3-beta.1 installs as 1.2.3.
MSI_VERSION="${VERSION%%[-+]*}"

mkdir -p "$(dirname "$OUTPUT")"
wixl -v -a x64 -D "Version=$MSI_VERSION" -D "SourceDir=$SOURCE" -o "$OUTPUT" packaging/windows/pyromirror.wxs
ls -lh "$OUTPUT"
