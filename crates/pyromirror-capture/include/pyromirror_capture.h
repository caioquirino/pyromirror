#ifndef PYROMIRROR_CAPTURE_H_
#define PYROMIRROR_CAPTURE_H_

#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct pyromirror_capture_context pyromirror_capture_context;

enum {
    PYROMIRROR_CAPTURE_FORMAT_BGRX = 0, // B, G, R, X in memory order
    PYROMIRROR_CAPTURE_FORMAT_RGBX = 1, // R, G, B, X in memory order
};

enum {
    PYROMIRROR_CAPTURE_FRAME = 1,    // out_frame holds a new frame
    PYROMIRROR_CAPTURE_NO_FRAME = 0, // nothing changed on screen within the timeout
    PYROMIRROR_CAPTURE_ERROR = -1,   // capture is permanently broken, see pyromirror_capture_last_error
};

typedef struct pyromirror_capture_config {
    // Windows: index of the monitor to capture in DXGI enumeration order, or -1 for the primary.
    int32_t output_index;
    // Linux: PipeWire remote fd from the ScreenCast portal (ownership is transferred) and the
    // node to consume.
    int32_t pipewire_fd;
    uint32_t pipewire_node;
    // Linux: true if the portal delivers the pointer as stream metadata rather than drawing it
    // into the frames.
    bool cursor_metadata;
} pyromirror_capture_config;

// The desktop's mouse pointer, kept out of the picture so the viewer can draw it locally.
typedef struct pyromirror_capture_cursor {
    uint64_t serial;     // Changes whenever anything below changes; starts at 0 (nothing known).
    bool in_video;       // The pointer is drawn into the frames; there is no shape to hand out.
    bool visible;        // False while the desktop hides the pointer.
    uint32_t width;      // Zero until a shape has been seen.
    uint32_t height;
    uint32_t hot_x;
    uint32_t hot_y;
    const uint8_t* rgba; // width * height * 4 bytes, straight alpha. Valid until the next call.
} pyromirror_capture_cursor;

typedef struct pyromirror_capture_frame {
    const uint8_t* data; // Valid until the next acquire/release/destroy call.
    uint32_t width;
    uint32_t height;
    uint32_t stride;     // Bytes per row.
    uint32_t format;     // PYROMIRROR_CAPTURE_FORMAT_*
    // Non-zero when the frame stayed on the GPU (see pyromirror_capture_set_gpu): `data` is NULL
    // and the image is in the shared texture with this id. The id changes when the texture is
    // replaced (first frame, mode change), which is when it has to be exported again.
    uint64_t gpu_texture;
    // Microseconds spent getting the image to where it is handed out from (reading it back, or
    // copying it on the GPU), not counting the wait for the desktop to change. 0 if not measured.
    uint32_t prepare_us;
} pyromirror_capture_frame;

// Returns NULL on failure; if `error` is non-NULL a description is written to it.
pyromirror_capture_context* pyromirror_capture_create(const pyromirror_capture_config* config, char* error, uint32_t error_size);
// Releases any previously acquired frame, then waits up to timeout_ms for a new one.
int pyromirror_capture_acquire(pyromirror_capture_context* ctx, uint32_t timeout_ms, pyromirror_capture_frame* out_frame);
void pyromirror_capture_release(pyromirror_capture_context* ctx);
// Current pointer shape and visibility. Cheap; call after each acquire.
void pyromirror_capture_get_cursor(pyromirror_capture_context* ctx, pyromirror_capture_cursor* out);
// Windows: position and size of the captured monitor in virtual-desktop pixels. Returns false
// where that is not known (Linux).
bool pyromirror_capture_get_bounds(pyromirror_capture_context* ctx, int32_t* x, int32_t* y, uint32_t* width, uint32_t* height);
// Zero-copy capture (Windows). When enabled, frames are left in a shared GPU texture instead of
// being read back: the frame's `gpu_texture` is set and its `data` is NULL. The texture's
// contents are complete when acquire returns, and stay untouched until the next acquire.
// Disabling it makes the next acquire return the most recent image again, as pixels, so a
// consumer that could not use the texture loses nothing. Returns false where unsupported.
bool pyromirror_capture_set_gpu(pyromirror_capture_context* ctx, bool enable);
// A new handle to the current shared texture (Windows: NT handle of an ID3D11Texture2D, BGRA8),
// owned by the caller; 0 if there is none.
uintptr_t pyromirror_capture_export_texture(pyromirror_capture_context* ctx);
// Windows: the LUID (8 bytes) of the graphics adapter that captures, which is where the shared
// texture lives. Returns false where that does not apply.
bool pyromirror_capture_get_adapter_luid(pyromirror_capture_context* ctx, uint8_t* luid);
// True if the captured monitor is in HDR mode. Frames are still 8-bit SDR in that case, converted
// by the OS, which typically looks washed out.
bool pyromirror_capture_hdr_active(pyromirror_capture_context* ctx);
const char* pyromirror_capture_last_error(pyromirror_capture_context* ctx);
void pyromirror_capture_destroy(pyromirror_capture_context* ctx);

#ifdef __cplusplus
}
#endif

#endif // PYROMIRROR_CAPTURE_H_
