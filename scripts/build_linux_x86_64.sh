#!/usr/bin/env bash
# Builds the Linux release and collects it in dist/linux-x86_64/.
set -euo pipefail

echo "=============================================="
echo " Building PyroMirror for Linux x86_64 (Release)"
echo "=============================================="

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"

# Pass --bundled-sdl to link SDL3 statically (for machines whose distribution has no SDL3).
FEATURES=()
if [ "${1:-}" = "--bundled-sdl" ]; then
    FEATURES=(--features pyromirror-client/bundled-sdl)
fi

cargo build --release "${FEATURES[@]}"

DIST_DIR="$PROJECT_ROOT/dist/linux-x86_64"
rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"
# The programs look for the PyroWave library next to themselves.
cp target/release/pyromirror target/release/pyromirror-server target/release/pyromirror-client \
   target/release/libpyrowave-shared.so.0 LICENSE NOTICE "$DIST_DIR/"

echo ""
echo "Build complete! Release files:"
ls -lh "$DIST_DIR"
