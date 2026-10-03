#!/usr/bin/env bash
set -euo pipefail

echo "=============================================="
echo " Cross-Compiling PyroMirror for Windows x86_64"
echo "=============================================="

rustup target add x86_64-pc-windows-gnu

if ! command -v x86_64-w64-mingw32-gcc &> /dev/null; then
    echo "Warning: MinGW GCC (x86_64-w64-mingw32-gcc) not found in PATH."
    echo "Install it via:"
    echo "  Arch Linux: sudo pacman -S mingw-w64-gcc"
    echo "  Ubuntu/Debian: sudo apt install mingw-w64"
    echo "  Fedora: sudo dnf install mingw64-gcc mingw64-gcc-c++"
    exit 1
fi

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEPS_DIR="$PROJECT_ROOT/.deps"
SDL3_MINGW_DIR="$DEPS_DIR/sdl3-mingw"

# Auto-download SDL3 MinGW development package if not present
if [ ! -f "$SDL3_MINGW_DIR/x86_64-w64-mingw32/lib/libSDL3.dll.a" ]; then
    echo "Downloading SDL3 MinGW development libraries..."
    mkdir -p "$DEPS_DIR"
    curl -L -s https://github.com/libsdl-org/SDL/releases/download/release-3.4.18/SDL3-devel-3.4.18-mingw.tar.gz | tar -xz -C "$DEPS_DIR"
    rm -rf "$SDL3_MINGW_DIR"
    mv "$DEPS_DIR"/SDL3-* "$SDL3_MINGW_DIR"
fi

SDL3_LIB_DIR="$SDL3_MINGW_DIR/x86_64-w64-mingw32/lib"
SDL3_BIN_DIR="$SDL3_MINGW_DIR/x86_64-w64-mingw32/bin"

export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
export CXX_x86_64_pc_windows_gnu=x86_64-w64-mingw32-g++
export LIBRARY_PATH="$SDL3_LIB_DIR:${LIBRARY_PATH:-}"

# Statically link libstdc++ and libgcc into executables so they don't depend on external MinGW DLLs
STATIC_LINK_FLAGS="-C link-arg=-static-libgcc -C link-arg=-static-libstdc++ -C link-arg=-Wl,-Bstatic -C link-arg=-lstdc++ -C link-arg=-lpthread -C link-arg=-Wl,-Bdynamic"
export RUSTFLAGS="-L native=$SDL3_LIB_DIR $STATIC_LINK_FLAGS ${RUSTFLAGS:-}"

echo "Building Windows release binaries with MinGW..."
cargo build --target x86_64-pc-windows-gnu --release

TARGET_DIR="$PROJECT_ROOT/target/x86_64-pc-windows-gnu/release"

# Copy runtime SDL3.dll
if [ -f "$SDL3_BIN_DIR/SDL3.dll" ]; then
    cp "$SDL3_BIN_DIR/SDL3.dll" "$TARGET_DIR/"
fi

# Copy any companion MinGW runtime DLLs if present on host
MINGW_BIN_DIR="/usr/x86_64-w64-mingw32/bin"
for dll in libstdc++-6.dll libgcc_s_seh-1.dll libwinpthread-1.dll; do
    if [ -f "$MINGW_BIN_DIR/$dll" ]; then
        cp "$MINGW_BIN_DIR/$dll" "$TARGET_DIR/"
        echo "Bundled $dll"
    fi
done

echo ""
echo "Build complete! Windows release distribution in $TARGET_DIR:"
ls -lh "$TARGET_DIR"/*.exe "$TARGET_DIR"/*.dll
