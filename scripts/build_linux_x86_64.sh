#!/usr/bin/env bash
set -euo pipefail

echo "=============================================="
echo " Building PyroMirror for Linux x86_64 (Release)"
echo "=============================================="

cargo build --release

echo ""
echo "Build complete! Release binaries available at:"
echo "  - target/release/pyromirror-server"
echo "  - target/release/pyromirror-client"
ls -lh target/release/pyromirror-server target/release/pyromirror-client
