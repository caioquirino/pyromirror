@echo off
echo ==============================================
echo  Building PyroMirror on Windows (MSVC Release)
echo ==============================================

where cargo >nul 2>nul
if %errorlevel% neq 0 (
    echo Error: cargo is not found in PATH.
    echo Please install Rust using rustup from https://rustup.rs/
    exit /b 1
)

cargo build --release

echo.
echo Build complete! Windows Release binaries:
echo   - target\release\pyromirror-server.exe
echo   - target\release\pyromirror-client.exe
