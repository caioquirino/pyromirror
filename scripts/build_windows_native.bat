@echo off
setlocal
echo ==============================================
echo  Building PyroMirror on Windows (MSVC Release)
echo ==============================================
echo Run this from a "x64 Native Tools Command Prompt for VS 2022" so that
echo cl.exe is available to CMake and Cargo.
echo.

for %%T in (cargo git cmake cl) do (
    where %%T >nul 2>nul
    if errorlevel 1 (
        echo Error: %%T was not found in PATH.
        echo   cargo: https://rustup.rs/
        echo   git:   winget install Git.Git
        echo   cmake: winget install Kitware.CMake   ^(Ninja is optional: winget install Ninja-build.Ninja^)
        echo   cl:    Visual Studio 2022 Build Tools with the "Desktop development with C++" workload
        exit /b 1
    )
)

cd /d "%~dp0\.."
git submodule update --init submodules/pyrowave
if errorlevel 1 exit /b 1

cargo build --release
if errorlevel 1 exit /b 1

echo.
echo Build complete! Windows release binaries in target\release:
echo   - pyromirror-server.exe
echo   - pyromirror-client.exe
echo   - libpyrowave-shared-0.dll  (must stay next to the executables)
