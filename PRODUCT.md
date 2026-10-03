# PyroMirror (🔥🪞)
### Ultra-Low-Latency, High-Bandwidth Remote Desktop & Display Extension

PyroMirror is a next-generation, high-performance remote desktop and game streaming system designed specifically for **local Ethernet (1GbE / 2.5GbE / 10GbE)** and **USB4 / Thunderbolt 4 point-to-point networks (20–40 Gbps)**.

Powered by [PyroWave](https://github.com/Themaister/pyrowave) — the groundbreaking Vulkan compute shader video codec developed by Hans-Kristian Arntzen (Themaister) — PyroMirror delivers **sub-millisecond GPU encode/decode latency**, pristine **4:4:4 chroma text clarity**, and true **zero-copy GPU-to-GPU streaming**.

---

## 1. Executive Summary & Vision

Existing remote desktop and streaming tools were designed around bandwidth scarcity over the internet or Wi-Fi:
* **Traditional RDP / VNC:** Sluggish (30–60 FPS), high latency, CPU-intensive software tile compression, lack of modern GPU acceleration.
* **Moonlight / Sunshine / Parsec:** Exceptional for internet streaming, but fundamentally constrained by hardware inter-frame codecs (H.264, HEVC, AV1). Hardware encoders (NVENC/AMF/VAAPI) add 4–12 ms of pipeline delay, depend on motion estimation and GOP structures, and stall visual feedback whenever a packet drop forces an IDR keyframe request.

**PyroMirror fundamentally flips this paradigm:**
In modern homes, offices, and studios, local networks provide **abundant, gigabit-speed bandwidth**. Through local Ethernet and USB4 / Thunderbolt 4 links, bandwidth is virtually free. PyroMirror exchanges high bitrates (200 Mbps to 2 Gbps) for **absolute minimum latency** and **instantaneous fault recovery**.

### Key Performance Targets
* **Encode Time:** `< 0.1 ms` at 1080p, `< 0.2 ms` at 4K (running on Vulkan compute shaders).
* **Decode Time:** `< 0.1 ms` at 1080p, `< 0.2 ms` at 4K.
* **Glass-to-Glass Pipeline Delay:** `1.5 – 3.5 ms` total over local Ethernet / Thunderbolt 4.
* **Pixel Fidelity:** Full YCbCr 4:4:4 or 4:2:0 at high bitrates (visually lossless, sharp desktop text and high-framerate 120–144Hz fluidity).
* **Zero Recovery Stalls:** 100% intra-only wavelet frames; dropped packets produce zero reference-frame corruption or keyframe delays.

---

## 2. Core Technological Breakthrough: Why PyroWave?

PyroWave replaces the traditional Discrete Cosine Transform (DCT) and motion estimation pipelines with an **intra-only Discrete Wavelet Transform (DWT)** using the CDF 9/7 wavelet filter (conceptually similar to JPEG 2000, but redesigned from scratch for Vulkan compute parallelization).

```
Traditional Codecs (H.264 / HEVC / AV1):
[Frame 0: I-Frame] -> [Frame 1: P-Frame] -> [Frame 2: P-Frame] -> [Frame 3: P-Frame]
                       ▲ Missing packet? Pipeline stalls until new I-Frame!
                       (Heavy motion search, 3-8ms encoder buffer delay)

PyroWave Codec:
[Frame 0: Wavelet]    [Frame 1: Wavelet]    [Frame 2: Wavelet]    [Frame 3: Wavelet]
  └─ 64x64 Tiles        └─ 64x64 Tiles        └─ 64x64 Tiles        └─ 64x64 Tiles
     ▲ 100% independent. A dropped packet only softens a 64x64 tile for 1 frame.
     Zero reference frames, zero motion estimation, <0.1ms compute pass.
```

### Why PyroWave is Uniquely Suited for Remote Desktop
1. **Single-Pass Exact Rate Control:** PyroWave calculates exact bitplane quantization budgets on the GPU, guaranteeing the encoded frame strictly fits the network MTU budget without multi-pass encoding latency.
2. **Graceful Packet Loss Degradation:** Wavelet coefficients are encoded in independent 64×64 spatial blocks across discrete sub-bands. Dropping high-frequency packets merely reduces local sharpness for a single frame without distorting subsequent frames.
3. **Pristine 4:4:4 Chroma Subsampling:** Conventional hardware video encoders often blur colored fine text (e.g., code in IDEs or terminal text) due to 4:2:0 subsampling. PyroWave provides native 4:4:4 full chroma support.
4. **Desktop & Mobile Compute Modes:** Supports standard high-throughput compute shaders for desktop GPUs, as well as a specialized **fragment shader iDWT decoding path** for mobile and embedded GPUs (Qualcomm Adreno, ARM Mali).

---

## 3. System Architecture & End-to-End Pipeline

```
+-------------------------------------------------------------------------------------------------+
|                                        HOST / SERVER                                            |
|                                                                                                 |
|   +-----------------------+      +-------------------------------------------+   |
|   |  Windows: DXGI Output |      |  Linux: Wayland PipeWire (DMA-BUF)        |   |
|   |  Duplication (D3D11)  |      |  & Direct DRM / KMS Screen Plane          |   |
|   +-----------+-----------+      +---------------------+---------------------+   |
|               |                                        |                         |
|               +----------------------------------------+                         |
|                                              |                                                  |
|                                              v (Zero-Copy GPU Texture Handle)                   |
|                              +-------------------------------+                                  |
|                              |  PyroWave Vulkan GPU Encoder  |                                  |
|                              |  - Color Space Convert (sRGB) |                                  |
|                              |  - Forward CDF 9/7 Wavelets   |                                  |
|                              |  - Bitplane Quantization      |                                  |
|                              +---------------+---------------+                                  |
|                                              |                                                  |
|                                              v (Vulkan-mapped Packet Ringbuffer)                |
|                              +-------------------------------+                                  |
|                              |   Rust High-Speed UDP Pacer   |                                  |
|                              |   - Jumbo MTU adaptation      |                                  |
|                              |   - Micro-burst token bucket  |                                  |
|                              +---------------+---------------+                                  |
+----------------------------------------------|--------------------------------------------------+
                                               |
                   1GbE / 2.5GbE / 10GbE or USB4 / Thunderbolt 4 (20-40 Gbps)
                                               |
+----------------------------------------------|--------------------------------------------------+
|                                              v                                                  |
|                                       CLIENT / VIEWER                                           |
|                                                                                                 |
|                              +-------------------------------+                                  |
|                              |   Rust UDP Assembly Receiver  |                                  |
|                              |   - recvmmsg / batch socket   |                                  |
|                              |   - Zero-copy slice parsing   |                                  |
|                              +---------------+---------------+                                  |
|                                              |                                                  |
|                                              v (Packet Buffers)                                 |
|                              +-------------------------------+                                  |
|                              |  PyroWave Vulkan GPU Decoder  |                                  |
|                              |  - Inverse DWT (Compute/Frag) |                                  |
|                              |  - YUV444/420 to RGB Blit     |                                  |
|                              +---------------+---------------+                                  |
|                                              |                                                  |
|                                              v (Direct Swapchain Presentation)                  |
|                              +-------------------------------+                                  |
|                              |   SDL3 Presentation Window    |                                  |
|                              |   - Immediate / Mailbox WSI   |                                  |
|                              |   - Raw mouse / Keyboard grab |                                  |
|                              +-------------------------------+                                  |
+-------------------------------------------------------------------------------------------------+
```

### The Three Pipeline Stages

#### 1. Zero-Copy Host Capture
* **Windows Host:** Uses DXGI Desktop Duplication (`IDXGIOutputDuplication`) into a Direct3D 11 texture. The texture's shared NT handle (`HANDLE`) is imported directly into Vulkan via `VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_BIT`, synchronized via `ID3D11Fence` <-> Vulkan timeline semaphores.
* **Linux Host:** Interacts with the Desktop Portal (`org.freedesktop.portal.ScreenCast`) to stream video via PipeWire. PipeWire delivers DMA-BUF file descriptors (`SPA_DATA_DmaBuf`) which are imported directly into Vulkan using `VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT` with DRM format modifiers.
* **Zero Host Copies:** Pixels never leave VRAM; no CPU memory roundtrip occurs.

#### 2. Network Transport & Pacing
* **Dynamic MTU & Jumbo Frames:**
  * Standard Ethernet: MTU ~1500 bytes (payload ~1400 bytes).
  * Jumbo Frames: MTU ~9000 bytes (payload ~8900 bytes).
  * USB4 / Thunderbolt 4: Point-to-point networking interfaces support MTUs up to 64 KB, drastically slashing packet header overhead and CPU interrupts.
* **Token-Bucket Micro-Pacer:** High-bitrate 4K frames (e.g. 500–1000 Mbps) generate dense bursts of packets. PyroMirror uses a lock-free token bucket pacer in Rust to spread bursts over sub-millisecond intervals, preventing network switch dropouts.

#### 3. Client Display & Input Forwarding
* **SDL3 Windowing Engine:** Provides cross-platform Wayland, Windows, and Android surface management.
* **System Key Interception:** Uses `SDL_SetWindowKeyboardGrab()` to capture and forward OS-level hotkeys (e.g. `Alt+Tab`, `Super / Windows` key, `Ctrl+Esc`) directly to the remote session.
* **Dual Mouse Handling:**
  * *Desktop Mode (Absolute):* Local client-side cursor rendering to guarantee 0 ms perceived cursor responsiveness, accompanied by absolute position synchronization.
  * *Immersive / Gaming Mode (Relative):* Mouse pointer locking (`SDL_SetWindowRelativeMouseMode`) forwarding raw sub-pixel delta movement at 1000 Hz.
* **Low-Latency Audio:** Stereo 48 kHz 24-bit audio captured via WASAPI Loopback (Windows) or PipeWire Monitor (Linux) streamed via raw uncompressed PCM (or 2.5ms Opus) directly into SDL3's lock-free `SDL_AudioStream`.

---

## 4. Platform & Architecture Support Matrix

| Role | Platform | Architecture | Capture / Render Backend | Input / Audio Backend | Status |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Host (Server)** | **Windows 10 / 11** | `x86_64` | DXGI Output Duplication + D3D11/Vulkan Interop | `SendInput`, ViGEmBus, WASAPI Loopback | **Tier 1** |
| **Host (Server)** | **Linux (Wayland)** | `x86_64`<br>`aarch64` | PipeWire DMA-BUF + DRM KMS / Vulkan Interop | `/dev/uinput`, PipeWire Monitor Source | **Tier 1** |
| **Client (Viewer)**| **Windows 10 / 11** | `x86_64` | SDL3 + PyroWave Vulkan Compute Decoder | SDL3 Input, WASAPI Audio Stream | **Tier 1** |
| **Client (Viewer)**| **Linux (Wayland)** | `x86_64`<br>`aarch64` | SDL3 + PyroWave Vulkan Compute Decoder | SDL3 Input, PulseAudio/PipeWire Audio Stream | **Tier 1** |
| **Client (Viewer)**| **Android 10+** | `arm64-v8a` | SDL3 Activity + PyroWave Fragment iDWT Decoder | Touchscreen virtual controls, AAudio | **Forward-Looking (Tier 2)** |

---

## 5. Network Topologies: From 1GbE to Thunderbolt 4

PyroMirror is purpose-built to scale effortlessly across local network topologies:

### A. Standard Local Ethernet (1 Gbps)
* **Target Bitrate:** 150 – 350 Mbps.
* **Packet Size:** 1400 bytes (standard 1500 MTU).
* **Ideal For:** 1080p 60/120Hz or 1440p 60Hz visually lossless desktop productivity and gaming.

### B. Multi-Gigabit Ethernet (2.5 GbE & 10 GbE)
* **Target Bitrate:** 400 – 800 Mbps.
* **Packet Size:** 8900 bytes (Jumbo frames enabled).
* **Ideal For:** 1440p 144Hz or 4K 60/120Hz with 4:4:4 color fidelity.

### C. USB4 & Thunderbolt 4 Point-to-Point Networking (20 – 40 Gbps)
* **Target Bitrate:** 800 Mbps – 2+ Gbps.
* **Packet Size:** 8900 to 65520 bytes (Thunderbolt IP adapter MTU).
* **Ideal For:** Uncompressed-quality 4K 120/144Hz multi-monitor extensions, zero CPU interrupt overhead, and absolute minimal pipeline latency (<2 ms end-to-end).

---

## 6. Technology Stack

* **Core Application & Orchestration:** **Rust (2024 Edition)**
  * Thread-safe concurrency, lock-free ringbuffers, zero-allocation packet slicing (`&[u8]`).
  * Asynchronous and threaded high-throughput UDP engine.
* **Video Codec Engine:** **PyroWave (C++ / Vulkan Compute Shaders)**
  * Embedded precompiled SPIR-V shaders (`slangmosh.hpp`), CDF 9/7 wavelets.
  * Direct Vulkan 1.3 compute pipeline.
* **Native Capture Shims:** **Focused C++ (~150 lines per platform)**
  * Windows: DXGI OutputDuplication + D3D11 shared handles.
  * Linux: PipeWire DMA-BUF + SPA POD negotiation.
  * Encapsulated behind a stable C ABI called by Rust.
* **Client Presentation & Windowing:** **SDL3**
  * Modern Wayland and Windows WSI integration, raw mouse input, keyboard grab, game controller mapping, and audio queueing.
* **Build System:** **Cargo** with `build.rs` integrating CMake for the native C++ components.

---

## 7. Project Structure

```
pyromirror/
├── PRODUCT.md                    (Product documentation & architectural blueprint)
├── Cargo.toml                    (Root cargo workspace definition)
├── submodules/
│   ├── pyrowave/                 (Themaister's PyroWave codec submodule)
│   └── pyrofling/                (Themaister's reference streaming tool)
├── crates/
│   ├── pyromirror-proto/         (Pure Rust: Little-endian binary network protocol & packets)
│   ├── pyromirror-net/           (Pure Rust: High-throughput UDP engine, ringbuffers, pacer)
│   ├── pyrowave-sys/             (Safe Rust FFI bindings to libpyrowave-shared)
│   ├── pyromirror-capture/       (Thin C++ capture bridge + Rust safe wrapper)
│   ├── pyromirror-server/        (Host daemon: screen/audio capture, encoder, input injection)
│   └── pyromirror-client/        (Viewer app: SDL3 window, decoder, audio, input capture)
└── scripts/
    ├── build_linux.sh            (Linux x86_64 / aarch64 build helper)
    └── build_windows.bat         (Windows MSVC build helper)
```
