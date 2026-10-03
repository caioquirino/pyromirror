#ifndef PYROMIRROR_CAPTURE_H_
#define PYROMIRROR_CAPTURE_H_

#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct pyromirror_capture_context pyromirror_capture_context;

typedef struct pyromirror_capture_frame {
    uintptr_t os_handle;     // Win32 HANDLE or Linux DMA-BUF fd
    uint32_t width;
    uint32_t height;
    uint32_t format;
    uintptr_t fence_handle;  // Synchronization fence (D3D11 fence / timeline semaphore)
    uint64_t timeline_value; // Timeline fence value
} pyromirror_capture_frame;

// Lifecycle
pyromirror_capture_context* pyromirror_capture_create(void);
bool pyromirror_capture_acquire(pyromirror_capture_context* ctx, uint32_t timeout_ms, pyromirror_capture_frame* out_frame);
bool pyromirror_capture_read_pixels(pyromirror_capture_context* ctx, uint8_t* out_pixels, uint32_t pitch);
void pyromirror_capture_get_resolution(pyromirror_capture_context* ctx, uint32_t* width, uint32_t* height);
void pyromirror_capture_release(pyromirror_capture_context* ctx);
void pyromirror_capture_destroy(pyromirror_capture_context* ctx);

#ifdef __cplusplus
}
#endif

#endif // PYROMIRROR_CAPTURE_H_
