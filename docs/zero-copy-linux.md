# Zero-copy on Linux

Status: both sides work on the one machine they were built on (AMD Radeon 890M, Mesa 26.2
RADV and radeonsi, KDE Plasma on Wayland, PipeWire 1.6). Not yet tried: GNOME, NVIDIA's
proprietary driver, a laptop with two GPUs. See "What exists (Linux)" and "Still open" below;
the two "Part" sections are the original spec, kept for the reasoning.

It cannot be developed in WSL or in `docker/demo`: neither has a GPU that hands out DMA-BUFs.

## What "zero-copy" means here

Without it, every frame takes three trips through memory on each side:

- **Sharing:** the captured image is read back from the GPU, converted from RGB to YCbCr on the
  CPU, and uploaded again for PyroWave to encode.
- **Viewing:** the decoded planes are read back, converted to RGB on the CPU, and uploaded again
  for SDL to draw.

With it, the image never leaves the GPU: PyroWave imports the texture another API owns and does
the conversion (and `--scale`) itself.

## What exists (Windows)

Measured on the development machine, 3840x2160 at 60 fps, 250 Mbps, 4:4:4:

| Side | Before | Zero-copy |
| :--- | :--- | :--- |
| Sharing (capture + convert + encode) | about 8.0 ms per frame | about 1.4 ms |
| Viewing (decode + convert + upload + present) | about 8.7 ms per frame | about 1.0 ms |

How it is built, bottom up:

| Piece | Where | What it does |
| :--- | :--- | :--- |
| Glue | `crates/pyrowave-sys/src/glue.c` | The PyroWave calls that take Vulkan types, behind a Vulkan-free C API (`pm_*`): device on a given adapter, import a texture, encode from it, import a fence, decode into three planes. |
| Codec | `crates/pyromirror-codec/src/lib.rs` | `Device::on_adapter`, `Encoder::import_texture` / `encode_texture`, `Decoder::add_gpu_target` / `set_gpu_fence` / `decode_to_target`. `TextureHandle` names the kind of OS handle; it has one variant, `D3d11`. |
| Capture | `crates/pyromirror-capture` (`capture_win.cpp`, `lib.rs`) | `Capturer::set_gpu_frames`, `export_texture`, `adapter_luid`; `Frame::texture` is set instead of `Frame::data`, and `Frame::prepare` reports the time spent. |
| Server | `crates/pyromirror-server/src/main.rs` | `encode_texture()` imports on first use and falls back to pixels on any failure; `Stats` logs the "Frame cost" line; `--no-zero-copy`. |
| Viewer | `crates/pyromirror-client/src/gpu_present.rs`, `d3d11_planes.c`, `video.rs` | Three sets of Y/Cb/Cr textures on SDL's Direct3D device, drawn by SDL as one "IYUV" texture; `video::Frame::Target(index)`; `--no-zero-copy`. |

Two rules the Windows code follows, which Linux should keep:

1. **The two APIs take turns, with a CPU wait in between.** Capture waits for its GPU copy to
   finish before the frame is handed out; the viewer waits for the decode to finish before the
   set is shown. No semaphores cross the API boundary on the sharing side. It costs a fraction
   of a millisecond and removes a whole class of bugs.
2. **Everything falls back.** If the device, the import or an encode/decode fails, a warning is
   logged and the old path carries on, starting from the same image.

## What exists (Linux)

Measured on the development machine, 1920x1200 at 60 fps, 250 Mbps, 4:4:4:

| Side | Before | Zero-copy |
| :--- | :--- | :--- |
| Sharing (convert + encode; the compositor's readback is not in the "before") | 5.2 to 6.4 ms per frame | about 3.3 ms (0.3 ms waiting for the compositor + 3.0 ms encode) |
| Viewing (decode + convert + upload) | about 8 ms per frame | about 1.3 to 2 ms, no upload |

How it is built:

| Piece | Where | What it does |
| :--- | :--- | :--- |
| Glue | `crates/pyrowave-sys/src/glue.c` | `pm_dmabuf_modifiers` (what the device can import, asked through the Vulkan loader PyroWave already loaded), `pm_gpu_image_import_dmabuf`, `pm_gpu_fence_create`, `pm_device_drm_render_node`. Imported DMA-BUFs are acquired from and released to `VK_QUEUE_FAMILY_FOREIGN_EXT`. |
| Codec | `crates/pyromirror-codec/src/lib.rs` | `TextureHandle::DmaBuf`; the encoder keeps its imported textures in a map by id; `Device::dmabuf_modifiers` / `dmabuf_plane_modifiers` / `drm_render_node`; `Decoder::create_gpu_fence`. |
| Capture | `crates/pyromirror-capture/src/capture_linux.cpp`, `lib.rs` | Offers DMA-BUF formats with the encoder's modifiers ahead of shared memory, settles on one modifier, keeps the newest buffer out of the pool while it is encoded, and waits for the compositor with `poll()`. `Frame::dmabuf` describes the buffer; `Frame::texture` is an id that is never reused. |
| Server | `crates/pyromirror-server/src/main.rs` | Creates the device before capture, encodes the first frame from a DMA-BUF too, imports each buffer of the pool the first time it comes by. |
| Viewer | `crates/pyromirror-client/src/gpu_present_linux.rs`, `dmabuf_planes.c` | Design A below: three sets of single-channel GBM buffers, imported into Vulkan as decode targets and into OpenGL as EGL images, drawn by SDL as one "IYUV" texture. EGL and GBM are loaded at run time, so the viewer gained no dependency. |

What was learned on the way:

- **Storage writes to an imported single-channel DMA-BUF work** on RADV, which was the open
  question of design A. Design B was not needed.
- **SDL gives a texture it is handed storage of its own** (`glTexImage2D`), which would detach
  the EGL image. The image is therefore bound after SDL has wrapped the texture.
- **A fence of PyroWave's own** needs `VK_SEMAPHORE_IMPORT_TEMPORARY_BIT` set even though
  nothing is imported; without it `pyrowave_sync_object_create` returns "invalid argument".
- **Falling back on the sharing side renegotiates the stream** to shared memory
  (`pw_stream_update_params` without the DMA-BUF formats). KWin sends a frame right after, so
  the picture is there at once; the encoder is not left with a stale one.
- **Zero-copy display needs OpenGL on EGL.** Under X11, SDL uses GLX and the viewer converts
  on the CPU as before (logged at debug level, not as a warning).
- Buffers are imported the first time they are seen rather than in `add_buffer`: the import
  happens on the encoding thread, and ids that never repeat make stale imports harmless.

## Still open

- **GNOME**, and any compositor other than KWin. Things that may differ: how pointer-only
  buffers are marked (a chunk of size 0 or the `CORRUPTED` flag is skipped), and whether a frame
  follows a renegotiation.
- **NVIDIA's proprietary driver**: `poll()` may return at once (no implicit sync) and frames
  could tear; the sync-file route of step 5 is not implemented.
- **Two GPUs.** The server uses the default device and does not check that the compositor
  renders on it. The viewer does compare render nodes (`VK_EXT_physical_device_drm` against
  EGL's device) and falls back on a mismatch, but that path has not run on real hardware.
- **Modifiers with more than one memory plane** on the sharing side are passed through as
  PipeWire reports them, assuming all planes are in the first buffer's fd. Not seen in practice.
- **`docker/demo`** was not re-run. It has no DMA-BUFs, so it takes the shared-memory path,
  which `--no-zero-copy` and a forced import failure exercised here.
- **NV12** from the compositor: not offered.
- The menu, pointer and fullscreen were not checked by hand with the zero-copy display, only
  that the window's contents (`--dump-window`, also fullscreen) are right.

PyroWave's own interop test (`submodules/pyrowave/pyrowave_c_interop_test.cpp`) is the reference
for every import; it has DMA-BUF cases next to the Direct3D ones.

## Part 1: sharing on Linux

Today `capture_linux.cpp` asks PipeWire for memory buffers only (`SPA_DATA_MemPtr | MemFd`, no
`SPA_FORMAT_VIDEO_modifier`) and copies each one. The compositor does the GPU readback for us,
so that cost is hidden: it does not show in our timings, but it is real.

Needed:

1. **Negotiate DMA-BUFs.** Offer two `EnumFormat` params: first one with
   `SPA_FORMAT_VIDEO_modifier` (the modifiers Vulkan can import for the format, flags
   `MANDATORY | DONT_FIXATE`), then the current one as the fallback. Fixate the modifier in
   `param_changed`. Add `SPA_DATA_DmaBuf` to the buffer types.
2. **Know which modifiers Vulkan can import.** `vkGetPhysicalDeviceFormatProperties2` with
   `VkDrmFormatModifierPropertiesListEXT` for `VK_FORMAT_B8G8R8A8_UNORM` (and `R8G8B8A8`). The
   glue has no Vulkan loader of its own; get the handles from
   `pyrowave_device_get_vk_device_handles` and the function through `dlopen("libvulkan.so.1")`.
   So the PyroWave device has to exist before the stream is negotiated, which is the opposite
   order from today (`main.rs` creates the capturer first).
3. **Import each buffer once.** PipeWire cycles through a small pool. Import in `add_buffer`,
   destroy in `remove_buffer`: `pyrowave_image_create` with
   `VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT`, tiling `DRM_FORMAT_MODIFIER`, and
   `VkImageDrmFormatModifierExplicitCreateInfoEXT` (modifier, plane offset and stride from the
   `spa_data` chunk). PyroWave takes ownership of the fd, so `dup()` it.
4. **Hold the buffer while it is encoded.** Today `on_process` copies and queues the buffer
   straight back. With zero-copy the newest buffer must stay dequeued until the next
   `acquire`/`release`, as the Windows side keeps its shared texture untouched.
5. **Wait for the compositor's rendering.** DMA-BUFs carry implicit fences. The CPU-wait
   equivalent of rule 1 is `poll()` on the fd for `POLLIN` before handing the frame out. If that
   proves unreliable, export a sync file (`DMA_BUF_IOCTL_EXPORT_SYNC_FILE`, kernel 6.0+) and pass
   it as the acquire semaphore (`pyrowave_sync_object_create`, `SYNC_FD`).
6. **Widen the Rust interfaces.** One shared texture with one id does not fit a pool: the frame
   needs the buffer's id plus fd, modifier, offset, stride and format; `TextureHandle` gets a
   `DmaBuf { .. }` variant; `Encoder` keeps a small map from id to imported image instead of a
   single `texture`.

Open questions, to settle on the machine:

- **Which GPU.** On a laptop with two GPUs the PyroWave device must be the one the compositor
  renders on. Start with the default device and rely on the fallback; if that picks wrong, match
  through `VK_EXT_physical_device_drm` or `pyrowave_create_device_by_compat` with a device UUID.
- **NVIDIA's proprietary driver** has no implicit sync, so step 5's `poll()` may return at once
  and frames could tear. Mesa drivers (Intel, AMD) are the expected easy case.
- **NV12.** Some compositors can hand out NV12. PyroWave's scaled encode accepts it
  (`VK_FORMAT_G8_B8R8_2PLANE_420_UNORM` with the colour aspect); not needed for a first version.

## Part 2: viewing on Linux

SDL does not draw with Direct3D here, so the Windows viewer code does not carry over; the shape
of it does. Two candidate designs, neither tried:

- **A. OpenGL renderer, shared planes.** Allocate three single-channel buffers per set with GBM,
  import them into Vulkan as DMA-BUFs (decode targets, `STORAGE` usage) and into GL through
  `EGL_EXT_image_dma_buf_import`, then wrap the three GL textures in one SDL "IYUV" texture
  (`SDL_PROP_TEXTURE_CREATE_OPENGL_TEXTURE_NUMBER`, `_U_NUMBER`, `_V_NUMBER`). This mirrors
  Windows, keeps SDL's shader doing the colour conversion, and keeps the menu drawing as it is.
  Unknown: whether drivers allow `STORAGE` writes to an imported single-channel DMA-BUF.
- **B. SDL's Vulkan renderer on a shared device.** SDL can be given an existing Vulkan device
  (`SDL_PROP_RENDERER_CREATE_VULKAN_*`) and textures made from a `VkImage`. Its planar textures
  are one multi-planar image, not three, so the decoder would write a `G8_B8_R8_3PLANE` image
  (PyroWave's test covers that, and reports which formats a driver accepts). Unknown: whether
  PyroWave's own device has the swapchain extensions SDL needs, or whether PyroWave must run on
  a device we create (`pyrowave_create_device`).

Start with a spike of A on the target machine. Fall back to B if imported planes cannot be
written.

The viewer waits for each decode through the imported Direct3D fence on Windows. On Linux the
equivalent is a sync object PyroWave creates itself (pass an invalid handle to
`pyrowave_sync_object_create`) and `pyrowave_sync_object_cpu_wait`.

## How to test

Build and run on the machine itself; `RUST_LOG=debug` prints the timings every two seconds.

```bash
cargo build --release -p pyromirror-server -p pyromirror-client
```

Sharing, compared against the old path (the "Frame cost" line says which path ran):

```bash
RUST_LOG=debug target/release/pyromirror-server --bind 127.0.0.1 --port 9123 --no-pairing
```

```bash
RUST_LOG=debug target/release/pyromirror-server --bind 127.0.0.1 --port 9123 --no-pairing --no-zero-copy
```

Viewing, with a picture of the window to check by eye (`--dump-window` works with zero-copy;
`--dump-frame` forces the old path because it needs the pixels):

```bash
RUST_LOG=debug target/release/pyromirror-client 127.0.0.1:9123 --exit-after 8 --dump-window window.bmp
```

A viewer showing the desktop it runs on shows itself inside itself; that is expected, and
`SDL_VIDEODRIVER=dummy` avoids it when only the decode matters.

## Done when

- The server logs "zero-copy" in its "Frame cost" line on GNOME and on KDE, the picture matches
  `--no-zero-copy`, and the time per frame is lower.
- Pulling the plug on any step (wrong GPU, a compositor without DMA-BUFs, `--no-zero-copy`)
  leaves a working stream and one warning in the log.
- The viewer logs "Zero-copy display", `--dump-window` shows a correct picture in 4:4:4 and
  4:2:0, and the menu, pointer and fullscreen still work.
- `docker/demo` still works: it has no DMA-BUFs, so it exercises the fallback.
