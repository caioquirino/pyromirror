#!/usr/bin/env bash
set -euo pipefail

echo "=============================================="
echo " Building PyroMirror Client for Android arm64-v8a"
echo "=============================================="

if ! command -v cargo-ndk &> /dev/null; then
    echo "Installing cargo-ndk..."
    cargo install cargo-ndk
fi

rustup target add aarch64-linux-android

if [ -z "${ANDROID_NDK_HOME:-}" ] && [ -z "${NDK_HOME:-}" ]; then
    echo "Warning: ANDROID_NDK_HOME is not set."
    echo "Please export ANDROID_NDK_HOME=/path/to/android-ndk"
fi

mkdir -p target/android/jniLibs/arm64-v8a

cargo ndk -t arm64-v8a -o ./target/android/jniLibs build --release -p pyromirror-client

echo ""
echo "Build complete! Android arm64-v8a libraries generated in target/android/jniLibs/"
