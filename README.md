# PyroMirror (🔥🪞)

> **Next-generation, ultra-low-latency, high-bandwidth remote desktop and display streaming system built with [PyroWave](https://github.com/Themaister/pyrowave) and Rust.**

PyroMirror is designed from the ground up for **local Ethernet (1GbE / 2.5GbE / 10GbE)** and **USB4 / Thunderbolt 4 point-to-point networks (20–40 Gbps)**. By pairing Hans-Kristian Arntzen's Vulkan compute wavelet codec (PyroWave) with a zero-copy capture pipeline, PyroMirror achieves **sub-millisecond encode/decode latency**, pristine **4:4:4 color**, and zero reference-frame packet drop stalls.

For deep technical architecture, performance targets, and design specifications, see [PRODUCT.md](PRODUCT.md).

---

## Current Status

What works today:

* **Video, end to end:** desktop capture → PyroWave encode (Vulkan compute) → UDP → PyroWave decode → SDL3 window, on Windows and Linux, in 4:4:4 or 4:2:0.
* **Capture backends:** DXGI Desktop Duplication on Windows; xdg-desktop-portal ScreenCast + PipeWire on Linux (Wayland, and X11 sessions on desktops that ship a portal backend).
* **Mouse and keyboard:** injected with `SendInput` on Windows and through the RemoteDesktop portal on Linux (GNOME, KDE; wlroots desktops have no such portal and stay view-only). Keys are sent as physical key positions, so the host's keyboard layout applies.
* **Audio:** whatever the host plays (WASAPI loopback on Windows, the default sink's monitor via PipeWire on Linux) is sent as uncompressed 16-bit stereo and played by the client.
* **Loss handling:** every datagram carries one independently decodable PyroWave packet, so lost packets blur a few blocks of one frame instead of stalling the stream.

Not implemented yet:

* **Zero-copy capture.** Frames take a CPU round trip (colour conversion + upload) on both ends; the D3D11 shared-texture / DMA-BUF paths of PyroWave are not wired up yet.
* **HDR.** With HDR enabled on Windows the capture is an SDR conversion that looks washed out.
* **Encryption.** Pairing controls who may connect, but video, audio and input travel unencrypted.
* **Mouse pointer on Windows** (Desktop Duplication delivers it separately; the viewer's own pointer shows the position), **gamepads**, **clipboard**, **FEC**, **resolution changes while streaming**, and the **Android client**.

---

## Supported Platforms

| Platform | Host (Server) | Client (Viewer) | Architecture |
| :--- | :---: | :---: | :--- |
| **Windows 10 / 11** | ✅ | ✅ | `x86_64` |
| **Linux (Wayland)** | ✅ | ✅ | `x86_64`, `aarch64` |
| **Android 10+** | Planned | Planned | `arm64-v8a` |

---

## Prerequisites & Requirements

### 1. Hardware Requirements
* **GPU:** Vulkan 1.3 capable GPU supporting compute shaders and subgroup operations.
  * Desktop: NVIDIA (GeForce GTX 900+ / RTX), AMD (GCN 4+ / RDNA), Intel (Skylake+ / Arc).
  * Mobile: Qualcomm Adreno (600+), ARM Mali (Bifrost/Valhall) — uses PyroWave's fragment iDWT path.
* **Network:** Local Ethernet (1 Gbps minimum recommended; 2.5G/10G preferred) or USB4 / Thunderbolt 3/4 cable connection.

### 2. Toolchain Requirements
* **Rust:** 1.80+ (Rust 2024 edition ready)
* **C++ Compiler:**
  * Linux: GCC 11+ or Clang 14+
  * Windows: MSVC (Visual Studio 2022 or Build Tools v17.0+)
* **CMake:** Version 3.27+
* **Ninja:** Version 1.10+
* **Git:** With submodule support

---

## Platform Dependencies & Installation

### Linux

#### Arch Linux / Manjaro
```bash
# Core build tools, Vulkan headers, PipeWire, and SDL3
sudo pacman -S --needed \
    base-devel cmake ninja git \
    vulkan-devel vulkan-loader \
    pipewire libpipewire \
    sdl3
```

#### Ubuntu 24.04 / Debian 13
```bash
sudo apt update
sudo apt install -y \
    build-essential cmake ninja-build git \
    libvulkan-dev vulkan-tools \
    libpipewire-0.3-dev libspa-0.2-dev \
    libsdl3-dev
```

#### Fedora 40+
```bash
sudo dnf install -y \
    gcc gcc-c++ cmake ninja-build git \
    vulkan-headers vulkan-loader-devel \
    pipewire-devel \
    sdl3-devel
```

### Windows (10 / 11)
1. Install **Visual Studio 2022** (Desktop development with C++ workload enabled) or [Build Tools for Visual Studio](https://visualstudio.microsoft.com/downloads/).
2. Install **Git for Windows** (the build clones PyroWave's Granite dependency). A Vulkan SDK is not needed; up-to-date GPU drivers provide the Vulkan runtime.
3. Install **CMake** and **Ninja** via `winget`:
   ```powershell
   winget install Kitware.CMake Ninja-build.Ninja
   ```
4. Install **Rust**:
   ```powershell
   winget install Rustlang.Rustup
   rustup default stable-x86_64-pc-windows-msvc
   ```

---

## Toolchain Management via `mise` (Recommended)

If you use [`mise`](https://mise.jdx.dev/) (or `asdf`), you can install Rust, CMake, and Ninja in one command:

```bash
# Install toolchain versions specified in mise.toml
mise install

# Or activate manually:
mise use rust@latest
mise use cmake@latest
mise use ninja@latest
```

---

## Cloning & Building

### 1. Clone the Repository with Submodules
```bash
git clone --recurse-submodules https://github.com/caioquirino/pyromirror.git
cd pyromirror
```

*(If you already cloned without submodules, run `git submodule update --init --recursive`)*.

### 2. Build PyroMirror (Host & Client)
```bash
# Build release binaries with optimizations
cargo build --release
```

The first build clones the Granite revision PyroWave is pinned to (into `submodules/pyrowave/Granite`, so it needs `git` and network access) and compiles PyroWave with CMake. `libpyrowave-shared` is copied next to the binaries; keep it there when moving them elsewhere.

Binaries will be placed in `target/release/`:
* `pyromirror`: The launcher window. Pick **Connect** or **Share**, adjust the settings, press the button; it starts the two programs below for you and remembers your settings.
* `pyromirror-server`: The streaming host (screen capture, encoder).
* `pyromirror-client`: The viewer (SDL3 window, decoder, input capture).

---

## Automated Build Scripts

Ready-to-use scripts are located in `scripts/`:

* **Linux x86_64 (Release):**
  ```bash
  ./scripts/build_linux_x86_64.sh
  ```
  Produces `target/release/pyromirror-server` and `target/release/pyromirror-client`.

* **Linux aarch64 / ARM64 (Cross-compile):**
  ```bash
  ./scripts/build_linux_aarch64.sh
  ```
  Produces `target/aarch64-unknown-linux-gnu/release/pyromirror-client`.

* **Windows x86_64 (Cross-compile from Linux via MinGW):**
  ```bash
  ./scripts/build_windows_cross.sh
  ```
  Produces a self-contained folder, `dist/windows-x86_64/` (the three programs plus `libpyrowave-shared-0.dll`); copy that folder to the Windows machine.

* **Windows x86_64 (Native MSVC on Windows machine):**
  ```cmd
  scripts\build_windows_native.bat
  ```

* **Android arm64-v8a (Client library):**
  ```bash
  ./scripts/build_android.sh
  ```

---

## Installing a Release

Each [release](https://github.com/caioquirino/pyromirror/releases) carries ready-made packages:

| System | File | Install with |
| :--- | :--- | :--- |
| Windows | `pyromirror-<version>-windows-x86_64.msi` | Double-click; adds a Start menu entry |
| Windows (portable) | `pyromirror-<version>-windows-x86_64.zip` | Unzip anywhere, run `pyromirror.exe` |
| Debian / Ubuntu 24.04+ | `pyromirror_<version>-1_amd64.deb` | `sudo apt install ./pyromirror_*.deb` |
| Fedora 40+ | `pyromirror-<version>-1.x86_64.rpm` | `sudo dnf install ./pyromirror-*.rpm` |
| Arch Linux | `pyromirror-<version>-1-x86_64.pkg.tar.zst` | `sudo pacman -U pyromirror-*.pkg.tar.zst` |
| Arch Linux (AUR) | `PKGBUILD` (`pyromirror-bin`) | `makepkg -si` next to the file |
| Other Linux | `pyromirror-<version>-linux-x86_64.tar.gz` | Unpack, run `./pyromirror` |

The Linux packages need PipeWire, a Vulkan driver and, on the host, `xdg-desktop-portal` for your desktop. Windows builds are not code-signed yet, so Windows may warn about or block them (see Code Signing Policy).

---

## Running PyroMirror

The easiest way is the launcher: run `pyromirror` (`pyromirror.exe` on Windows) on both computers, press **Share this computer** on the host, and enter the address it shows in the **Connect** tab on the other one. It looks and works the same on Windows and Linux. The command-line programs it drives are described below.

While connected, move the pointer to the top edge of the viewer for a toolbar with fullscreen, keyboard grab, mouse lock, mute, disconnect and live frame rate / bitrate.

### 1. Starting the Host (Server)

```bash
# Launch server on default UDP/TCP port 9000 with 1080p60 at 250 Mbps
cargo run --release --bin pyromirror-server -- \
    --port 9000 \
    --bitrate-mbps 250 \
    --fps 60
```

#### Server Command-Line Options:
* `--bind <ADDR>`: Address to listen on (default: `0.0.0.0`).
* `--port <PORT>`: Port for TCP control and UDP video (default: `9000`).
* `--bitrate-mbps <MBPS>`: Video bitrate in Mbps; each frame is capped to bitrate / fps (default: `250`).
* `--chroma <420|444>`: Chroma subsampling (default: `444` for sharp text; `420` saves bandwidth).
* `--fps <FPS>`: Maximum framerate (default: `60`).
* `--mtu <BYTES>`: UDP datagram size (default: `1400` for standard Ethernet; `8900` for jumbo frames).
* `--scale <N>`: Shrink the picture by an integer factor before encoding (default: `1`; `2` turns a 4K desktop into a 1080p stream).
* `--pace-factor <X>`: Release datagrams at X times the bitrate (default: `2`). Lower values, down to `1.1`, smooth out bursts on Wi-Fi at the cost of a few milliseconds of latency.
* `--pairing-code <CODE>`: Use this pairing code instead of a random one (see Pairing below).
* `--no-pairing`: Let anyone who can reach the port connect.
* `--no-audio`: Do not capture or send audio.
* `--no-input`: Ignore the client's mouse and keyboard (view-only).
* `--monitor <INDEX>`: Windows only, monitor to capture (default: primary). On Linux the portal dialog picks the monitor.
* `--test-pattern <WxH>`: Stream a generated pattern instead of the desktop, to test codec and network without capture.

The stream has the resolution of the captured monitor divided by `--scale`. Set `RUST_LOG=debug` on either side for per-second fps / bitrate / timing statistics.

On Linux the first start shows your desktop's remote control / screen sharing dialog. The grant is remembered in `~/.local/state/pyromirror/`; delete the token files there to be asked again.

On Windows, input cannot reach elevated (administrator) windows or UAC prompts unless the server itself runs as administrator.

On Windows, allow the server through the firewall when prompted (TCP and UDP on the chosen port).

#### Pairing

The server prints a 6-digit pairing code when it starts (the launcher shows it while sharing is on). A computer connecting for the first time must supply it: type it into the launcher's Connect tab, or pass `--pairing-code` to the client. After that the two machines remember each other and no code is needed, even if addresses change. Pairings are stored in `%APPDATA%\pyromirror` / `~/.config/pyromirror` (`paired-clients` on the host, `paired-hosts` on the viewer); delete those files to forget them.

Pairing keeps strangers from connecting. It does not encrypt anything: the stream and your keystrokes are still readable by others on the same network, and someone recording a first-time pairing could work out the code. Pair on a network you trust.

### 2. Starting the Client (Viewer)

```bash
# Connect to the host (the port defaults to 9000)
cargo run --release --bin pyromirror-client -- pyro://192.168.1.100:9000
```

#### Viewer Controls:
* **`Ctrl + Alt + G`**: Toggle keyboard grab (so `Alt+Tab`, `Super` etc. reach the viewer).
* **`Ctrl + Alt + F`**: Toggle fullscreen.
* **`Ctrl + Alt + L`**: Toggle mouse lock (keeps the pointer inside the viewer window; also `--lock-mouse`).
* **`Ctrl + Alt + M`**: Toggle relative mouse mode.
* **`Ctrl + Alt + Q`**: Quit.

`--no-audio` on the client mutes the host's audio.

---

## Network & Performance Tuning

### A. USB4 & Thunderbolt 4 Direct Networking (20–40 Gbps)
Connecting two machines via a USB4 or Thunderbolt cable creates a high-speed point-to-point network adapter (`thunderbolt-net` on Linux, Thunderbolt Networking on Windows):
1. Configure static IP addresses on both machines (e.g. `10.0.0.1/24` and `10.0.0.2/24`).
2. **Enable Jumbo Frames / Large MTU:**
   ```bash
   # On Linux:
   sudo ip link set dev thunderbolt0 mtu 9000
   ```
3. Set PyroMirror bitrate to **800–1500 Mbps** with `--mtu 8900` for uncompressed-quality 4K at 120/144Hz.

### B. Linux REALTIME GPU Priority (`CAP_SYS_NICE`)
PyroWave achieves sub-0.1ms encoding by requesting high-priority compute queues on the GPU. On Linux, this requires granting the capability:
```bash
sudo setcap cap_sys_nice+eip target/release/pyromirror-server
```

### C. Socket Buffer Tuning
For multi-hundred-megabit streams, expand the OS UDP socket receive/send buffers to prevent micro-burst drops:
```bash
# On Linux host & client:
sudo sysctl -w net.core.rmem_max=16777216
sudo sysctl -w net.core.wmem_max=16777216
```

---

## Cross-Compiling for ARM64 Linux & Android

### Linux ARM64 (`aarch64-unknown-linux-gnu`)
1. Install the ARM64 cross-compiler:
   ```bash
   # Arch Linux:
   sudo pacman -S aarch64-linux-gnu-gcc
   # Ubuntu / Debian:
   sudo apt install gcc-aarch64-linux-gnu g++-aarch64-linux-gnu
   ```
2. Add the Rust target:
   ```bash
   rustup target add aarch64-unknown-linux-gnu
   ```
3. Build with Cargo:
   ```bash
   cargo build --target aarch64-unknown-linux-gnu --release
   ```

### Android (`arm64-v8a`)
The client can be built for Android devices using `cargo-ndk` and the Android NDK:
```bash
cargo install cargo-ndk
rustup target add aarch64-linux-android

cargo ndk -t arm64-v8a -o ./android/app/src/main/jniLibs build --release -p pyromirror-client
```

---

## Code Signing Policy

Windows releases are intended to be signed through [SignPath](https://signpath.io): free code signing provided by SignPath.io, certificate by SignPath Foundation. *(Application pending; until it is accepted, releases are unsigned.)*

* **What is signed:** only the files produced by the [release workflow](.github/workflows/release.yml) from the source in this repository: `pyromirror.exe`, `pyromirror-server.exe`, `pyromirror-client.exe` and `libpyrowave-shared-0.dll` (PyroWave, built from its pinned source). No prebuilt third-party binaries are included.
* **Committers and reviewers:** [Caio Quirino](https://github.com/caioquirino)
* **Approvers:** [Caio Quirino](https://github.com/caioquirino). Every signing request is approved manually.

### Privacy

PyroMirror does not collect or transmit any data to its authors or to third parties. It only communicates with the computers you connect it to: the screen contents, audio and input of a session travel directly between the two machines. Settings and pairing data are stored locally (`%APPDATA%\pyromirror` or `~/.config/pyromirror`).

---

## License

PyroMirror is licensed under the [Apache License 2.0](LICENSE). Third-party components and their licenses are listed in [NOTICE](NOTICE); the main ones:

* **PyroWave** and **Granite:** MIT License (Copyright (c) Hans-Kristian Arntzen).
* **SDL3:** zlib License.
* **egui:** MIT or Apache-2.0.
