//! PyroMirror desktop capture.
//!
//! - Windows: DXGI Desktop Duplication.
//! - Linux: xdg-desktop-portal ScreenCast + PipeWire.
//!
//! Both backends currently deliver CPU-readable 32-bit frames. Handing the GPU texture straight
//! to the encoder (D3D11 shared handle / DMA-BUF) is future work.

#[cfg(target_os = "linux")]
mod portal;

use std::ffi::CStr;
use std::os::raw::c_char;
use std::time::Duration;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum CaptureError {
    #[error("capture initialization failed: {0}")]
    InitFailed(String),
    #[error("capture stopped: {0}")]
    Lost(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// B, G, R, X in memory order.
    Bgrx,
    /// R, G, B, X in memory order.
    Rgbx,
}

#[derive(Debug, Clone, Default)]
pub struct CaptureOptions {
    /// Windows: monitor index in DXGI enumeration order; `None` selects the primary monitor.
    /// Ignored on Linux, where the portal dialog picks the monitor.
    pub output: Option<u32>,
}

/// A captured desktop image, borrowed from the capturer until the next `next_frame` call.
pub struct Frame<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
    /// Bytes per row; may be larger than `width * 4`.
    pub stride: u32,
    pub format: PixelFormat,
}

#[repr(C)]
struct RawContext {
    _private: [u8; 0],
}

#[repr(C)]
struct RawConfig {
    output_index: i32,
    pipewire_fd: i32,
    pipewire_node: u32,
}

#[repr(C)]
struct RawFrame {
    data: *const u8,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
}

const RAW_FORMAT_RGBX: u32 = 1;
const RAW_FRAME: i32 = 1;
const RAW_NO_FRAME: i32 = 0;

extern "C" {
    fn pyromirror_capture_create(config: *const RawConfig, error: *mut c_char, error_size: u32) -> *mut RawContext;
    fn pyromirror_capture_acquire(ctx: *mut RawContext, timeout_ms: u32, out_frame: *mut RawFrame) -> i32;
    fn pyromirror_capture_release(ctx: *mut RawContext);
    fn pyromirror_capture_hdr_active(ctx: *mut RawContext) -> bool;
    fn pyromirror_capture_last_error(ctx: *mut RawContext) -> *const c_char;
    fn pyromirror_capture_destroy(ctx: *mut RawContext);
}

pub struct Capturer {
    ctx: *mut RawContext,
    #[cfg(target_os = "linux")]
    _portal: portal::PortalSession,
}

// The native context is only used through `&mut self` and has no thread affinity.
unsafe impl Send for Capturer {}

impl Capturer {
    /// Starts capturing. On Linux this may block on a permission dialog shown by the desktop.
    pub fn new(options: &CaptureOptions) -> Result<Self, CaptureError> {
        let mut config = RawConfig {
            output_index: options.output.map_or(-1, |o| o as i32),
            pipewire_fd: -1,
            pipewire_node: 0,
        };

        #[cfg(target_os = "linux")]
        let portal_session = {
            let stream = portal::open()?;
            config.pipewire_fd = stream.pipewire_fd;
            config.pipewire_node = stream.pipewire_node;
            stream.session
        };

        let mut error = [0 as c_char; 512];
        let ctx = unsafe { pyromirror_capture_create(&config, error.as_mut_ptr(), error.len() as u32) };
        if ctx.is_null() {
            let message = unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy().into_owned();
            return Err(CaptureError::InitFailed(message));
        }

        // Only Linux fills in the PipeWire fields.
        let _ = &mut config;

        if unsafe { pyromirror_capture_hdr_active(ctx) } {
            log::warn!(
                "The captured monitor is in HDR mode. HDR capture is not implemented yet, so Windows \
                 hands out an SDR conversion that looks washed out; turn HDR off for correct colours."
            );
        }

        Ok(Self {
            ctx,
            #[cfg(target_os = "linux")]
            _portal: portal_session,
        })
    }

    /// Waits up to `timeout` for the desktop to change. `Ok(None)` means nothing was redrawn,
    /// which is the normal state of an idle desktop.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame<'_>>, CaptureError> {
        let mut raw = RawFrame { data: std::ptr::null(), width: 0, height: 0, stride: 0, format: 0 };
        let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        match unsafe { pyromirror_capture_acquire(self.ctx, timeout_ms, &mut raw) } {
            RAW_FRAME => {
                let len = raw.stride as usize * (raw.height as usize).saturating_sub(1) + raw.width as usize * 4;
                Ok(Some(Frame {
                    // SAFETY: the native side guarantees `data` covers every row at `stride`
                    // spacing until the next acquire/release, which needs `&mut self`.
                    data: unsafe { std::slice::from_raw_parts(raw.data, len) },
                    width: raw.width,
                    height: raw.height,
                    stride: raw.stride,
                    format: if raw.format == RAW_FORMAT_RGBX { PixelFormat::Rgbx } else { PixelFormat::Bgrx },
                }))
            }
            RAW_NO_FRAME => Ok(None),
            _ => {
                let message = unsafe { CStr::from_ptr(pyromirror_capture_last_error(self.ctx)) };
                Err(CaptureError::Lost(message.to_string_lossy().into_owned()))
            }
        }
    }
}

impl Drop for Capturer {
    fn drop(&mut self) {
        unsafe {
            pyromirror_capture_release(self.ctx);
            pyromirror_capture_destroy(self.ctx);
        }
    }
}
