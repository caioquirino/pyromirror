#!/usr/bin/env bash
set -euo pipefail

echo "=============================================="
echo " Cross-Compiling PyroMirror for Linux aarch64 (ARM64)"
echo "=============================================="

# Ensure Rust aarch64 target is installed
rustup target add aarch64-unknown-linux-gnu

# Check for cross-compiler
if ! command -v aarch64-linux-gnu-gcc &> /dev/null; then
    echo "Warning: aarch64-linux-gnu-gcc not found in PATH."
    echo "Install it via:"
    echo "  Arch Linux: sudo pacman -S aarch64-linux-gnu-gcc"
    echo "  Ubuntu/Debian: sudo apt install gcc-aarch64-linux-gnu g++-aarch64-linux-gnu"
    echo "  Fedora: sudo dnf install gcc-aarch64-linux-gnu"
    exit 1
fi

export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
export CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++

cargo build --target aarch64-unknown-linux-gnu --release

echo ""
echo "Build complete! ARM64 Release binaries:"
echo "  - target/aarch64-unknown-linux-gnu/release/pyromirror-server"
echo "  - target/aarch64-unknown-linux-gnu/release/pyromirror-client"
