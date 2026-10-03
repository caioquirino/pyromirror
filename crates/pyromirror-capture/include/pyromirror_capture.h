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
} pyromirror_capture_config;

typedef struct pyromirror_capture_frame {
    const uint8_t* data; // Valid until the next acquire/release/destroy call.
    uint32_t width;
    uint32_t height;
    uint32_t stride;     // Bytes per row.
    uint32_t format;     // PYROMIRROR_CAPTURE_FORMAT_*
} pyromirror_capture_frame;

// Returns NULL on failure; if `error` is non-NULL a description is written to it.
pyromirror_capture_context* pyromirror_capture_create(const pyromirror_capture_config* config, char* error, uint32_t error_size);
// Releases any previously acquired frame, then waits up to timeout_ms for a new one.
int pyromirror_capture_acquire(pyromirror_capture_context* ctx, uint32_t timeout_ms, pyromirror_capture_frame* out_frame);
void pyromirror_capture_release(pyromirror_capture_context* ctx);
// Windows: position and size of the captured monitor in virtual-desktop pixels. Returns false
// where that is not known (Linux).
bool pyromirror_capture_get_bounds(pyromirror_capture_context* ctx, int32_t* x, int32_t* y, uint32_t* width, uint32_t* height);
// True if the captured monitor is in HDR mode. Frames are still 8-bit SDR in that case, converted
// by the OS, which typically looks washed out.
bool pyromirror_capture_hdr_active(pyromirror_capture_context* ctx);
const char* pyromirror_capture_last_error(pyromirror_capture_context* ctx);
void pyromirror_capture_destroy(pyromirror_capture_context* ctx);

#ifdef __cplusplus
}
#endif

#endif // PYROMIRROR_CAPTURE_H_
