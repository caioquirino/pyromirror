# PyroMirror (🔥🪞)

> **Next-generation, ultra-low-latency, high-bandwidth remote desktop and display streaming system built with [PyroWave](https://github.com/Themaister/pyrowave) and Rust.**

PyroMirror is designed from the ground up for **local Ethernet (1GbE / 2.5GbE / 10GbE)** and **USB4 / Thunderbolt 4 point-to-point networks (20–40 Gbps)**. By pairing Hans-Kristian Arntzen's Vulkan compute wavelet codec (PyroWave) with a zero-copy capture pipeline, PyroMirror achieves **sub-millisecond encode/decode latency**, pristine **4:4:4 color**, and zero reference-frame packet drop stalls.

For deep technical architecture, performance targets, and design specifications, see [PRODUCT.md](PRODUCT.md).

---

## Supported Platforms

| Platform | Host (Server) | Client (Viewer) | Architecture |
| :--- | :---: | :---: | :--- |
| **Windows 10 / 11** | ✅ | ✅ | `x86_64` |
| **Linux (Wayland)** | ✅ | ✅ | `x86_64`, `aarch64` |
| **Android 10+** | Planned | 🚧 (In Progress) | `arm64-v8a` |

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
2. Install the **[Vulkan SDK](https://vulkan.lunarg.com/)** (v1.3.260 or newer).
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

### 2. Fetch PyroWave Granite Dependencies
PyroWave requires a small portion of the Granite framework for Vulkan memory management and Volk loaders:
```bash
cd submodules/pyrowave
bash checkout_granite.sh
cd ../..
```

### 3. Build PyroMirror (Host & Client)
```bash
# Build release binaries with optimizations
cargo build --release
```

Binaries will be placed in `target/release/`:
* `pyromirror-server`: The streaming host daemon (screen & audio capture, encoder, input receiver).
* `pyromirror-client`: The viewer application (SDL3 window, decoder, audio playback, input capture).

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
  Produces `target/x86_64-pc-windows-gnu/release/pyromirror-client.exe`.

* **Windows x86_64 (Native MSVC on Windows machine):**
  ```cmd
  scripts\build_windows_native.bat
  ```

* **Android arm64-v8a (Client library):**
  ```bash
  ./scripts/build_android.sh
  ```

---

## Running PyroMirror

### 1. Starting the Host (Server)

```bash
# Launch server on default UDP/TCP port 9000 with 1080p60 at 250 Mbps
cargo run --release --bin pyromirror-server -- \
    --port 9000 \
    --bitrate-mbps 250 \
    --fps 60
```

#### Server Command-Line Options:
* `--port <PORT>`: Base port for TCP control and UDP video streaming (default: `9000`).
* `--bitrate-mbps <MBPS>`: Target PyroWave bitrate in Mbps (default: `250`).
* `--chroma <420|444>`: Chroma subsampling mode (default: `444` for sharp text; `420` for bandwidth saving).
* `--fps <FPS>`: Target framerate (e.g. `60`, `120`, `144`).
* `--mtu <BYTES>`: Network MTU (default: `1400` for standard Ethernet; `8900` for Jumbo frames).

### 2. Starting the Client (Viewer)

```bash
# Connect to the host IP
cargo run --release --bin pyromirror-client -- pyro://192.168.1.100:9000
```

#### Viewer Controls:
* **`Ctrl + Alt + G`**: Toggle Keyboard & Mouse Grab (captures `Alt+Tab`, `Windows/Super` key for remote host).
* **`Ctrl + Alt + F`**: Toggle Fullscreen.
* **`Ctrl + Alt + M`**: Toggle between **Desktop Mode** (absolute cursor) and **Immersive / Game Mode** (relative mouse lock).
* **`F11`**: Toggle Performance Overlay HUD (latency, bitrate, FPS, packet loss).

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

## License

* **PyroMirror:** MIT License.
* **PyroWave:** MIT License (Copyright (c) Hans-Kristian Arntzen).
* **Granite:** Apache-2.0 / MIT.
