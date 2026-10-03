//! PyroMirror Host Capture Abstraction
//!
//! Provides zero-copy desktop frame capture backends:
//! - Windows: DXGI OutputDuplication (Direct3D 11/12)
//! - Linux: Wayland Desktop Portal / PipeWire (DMA-BUF) & Direct DRM / KMS

use thiserror::Error;

#[derive(Error, Debug)]
pub enum CaptureError {
    #[error("Capture initialization failed: {0}")]
    InitFailed(String),
    #[error("Access lost (display mode or UAC transition), recreation needed")]
    AccessLost,
    #[error("Frame capture timeout")]
    Timeout,
    #[error("Platform unsupported")]
    UnsupportedPlatform,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CapturedFrameInfo {
    pub os_handle: usize,    // Win32 HANDLE or Linux DMA-BUF fd
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub fence_handle: usize, // Timeline semaphore sync handle
    pub timeline_value: u64,
}

enum CaptureContextOpaque {}

extern "C" {
    fn pyromirror_capture_create() -> *mut CaptureContextOpaque;
    fn pyromirror_capture_acquire(
        ctx: *mut CaptureContextOpaque,
        timeout_ms: u32,
        out_frame: *mut CapturedFrameInfo,
    ) -> bool;
    fn pyromirror_capture_read_pixels(
        ctx: *mut CaptureContextOpaque,
        out_pixels: *mut u8,
        pitch: u32,
    ) -> bool;
    fn pyromirror_capture_get_resolution(
        ctx: *mut CaptureContextOpaque,
        width: *mut u32,
        height: *mut u32,
    );
    fn pyromirror_capture_release(ctx: *mut CaptureContextOpaque);
    fn pyromirror_capture_destroy(ctx: *mut CaptureContextOpaque);
}

pub struct NativeCapturer {
    ctx: *mut CaptureContextOpaque,
}

unsafe impl Send for NativeCapturer {}

impl NativeCapturer {
    pub fn new() -> Result<Self, CaptureError> {
        let ctx = unsafe { pyromirror_capture_create() };
        if ctx.is_null() {
            Err(CaptureError::InitFailed("Failed to initialize native capture context".into()))
        } else {
            Ok(Self { ctx })
        }
    }
}

impl Drop for NativeCapturer {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            unsafe { pyromirror_capture_destroy(self.ctx) };
            self.ctx = std::ptr::null_mut();
        }
    }
}

pub trait ScreenCapturer: Send {
    fn acquire_frame(&mut self, timeout_ms: u32) -> Result<CapturedFrameInfo, CaptureError>;
    fn copy_pixels(&mut self, out_buffer: &mut [u8], pitch: u32) -> bool {
        let _ = (out_buffer, pitch);
        false
    }
    fn resolution(&self) -> (u32, u32) {
        (0, 0)
    }
    fn release_frame(&mut self);
}

impl ScreenCapturer for NativeCapturer {
    fn acquire_frame(&mut self, timeout_ms: u32) -> Result<CapturedFrameInfo, CaptureError> {
        let mut frame = CapturedFrameInfo {
            os_handle: 0,
            width: 0,
            height: 0,
            format: 0,
            fence_handle: 0,
            timeline_value: 0,
        };

        let ok = unsafe { pyromirror_capture_acquire(self.ctx, timeout_ms, &mut frame) };
        if ok {
            Ok(frame)
        } else {
            Err(CaptureError::Timeout)
        }
    }

    fn copy_pixels(&mut self, out_buffer: &mut [u8], pitch: u32) -> bool {
        unsafe {
            pyromirror_capture_read_pixels(self.ctx, out_buffer.as_mut_ptr(), pitch)
        }
    }

    fn resolution(&self) -> (u32, u32) {
        let mut w: u32 = 0;
        let mut h: u32 = 0;
        unsafe {
            pyromirror_capture_get_resolution(self.ctx, &mut w, &mut h);
        }
        (w, h)
    }

    fn release_frame(&mut self) {
        unsafe { pyromirror_capture_release(self.ctx) };
    }
}

/// Factory function to instantiate the active platform capturer
pub fn create_default_capturer() -> Result<Box<dyn ScreenCapturer>, CaptureError> {
    let capturer = NativeCapturer::new()?;
    Ok(Box::new(capturer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_native_capturer_lifecycle() {
        let capturer = NativeCapturer::new();
        assert!(capturer.is_ok());
    }
}
