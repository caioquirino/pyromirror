#!/usr/bin/env bash
# Cross-compiles the Windows release with MinGW and collects it in dist/windows-x86_64/.
set -euo pipefail

echo "=============================================="
echo " Cross-Compiling PyroMirror for Windows x86_64"
echo "=============================================="

if ! command -v x86_64-w64-mingw32-g++ &> /dev/null; then
    echo "MinGW (x86_64-w64-mingw32-g++) was not found in PATH. Install it via:"
    echo "  Arch Linux:    sudo pacman -S mingw-w64-gcc"
    echo "  Ubuntu/Debian: sudo apt install mingw-w64"
    echo "  Fedora:        sudo dnf install mingw64-gcc mingw64-gcc-c++"
    exit 1
fi

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"

if command -v rustup &> /dev/null; then
    rustup target add x86_64-pc-windows-gnu
fi

# The linker and static-runtime flags live in .cargo/config.toml.
export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
export CXX_x86_64_pc_windows_gnu=x86_64-w64-mingw32-g++

cargo build --target x86_64-pc-windows-gnu --release

TARGET_DIR="$PROJECT_ROOT/target/x86_64-pc-windows-gnu/release"
DIST_DIR="$PROJECT_ROOT/dist/windows-x86_64"

# Everything a Windows machine needs: our three programs and the PyroWave codec library.
FILES=(pyromirror.exe pyromirror-server.exe pyromirror-client.exe libpyrowave-shared-0.dll)

rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"
for file in "${FILES[@]}"; do
    cp "$TARGET_DIR/$file" "$DIST_DIR/"
done
cp LICENSE NOTICE "$DIST_DIR/"

# Fail if anything still depends on a DLL that we do not ship and Windows does not provide.
for file in "${FILES[@]}"; do
    if x86_64-w64-mingw32-objdump -p "$DIST_DIR/$file" | grep -iE "DLL Name: (libstdc\+\+|libgcc|libwinpthread|SDL3)"; then
        echo "error: $file depends on a runtime DLL that is not part of the release" >&2
        exit 1
    fi
done

echo ""
echo "Build complete! Copy this folder to the Windows machine:"
ls -lh "$DIST_DIR"
