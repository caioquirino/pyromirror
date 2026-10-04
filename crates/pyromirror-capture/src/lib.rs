//! PyroMirror desktop capture.
//!
//! - Windows: DXGI Desktop Duplication.
//! - Linux: xdg-desktop-portal ScreenCast + PipeWire.
//!
//! Both backends currently deliver CPU-readable 32-bit frames. Handing the GPU texture straight
//! to the encoder (D3D11 shared handle / DMA-BUF) is future work.

mod input;
mod keymap;
#[cfg(target_os = "linux")]
mod portal;
#[cfg(target_os = "linux")]
mod wlr;

pub use input::InputInjector;

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
    /// The pixels; empty when the image stayed on the GPU (`texture`).
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
    /// Bytes per row; may be larger than `width * 4`.
    pub stride: u32,
    pub format: PixelFormat,
    /// Set when GPU frames are enabled and the image is in the capturer's shared texture
    /// instead of `data`. The value identifies the texture: when it changes, the texture has
    /// been replaced and must be exported again (`Capturer::export_texture`).
    pub texture: Option<u64>,
    /// How long it took to get the image here (reading it back from the GPU, or copying it
    /// there), not counting the wait for the desktop to change. Zero where not measured.
    pub prepare: Duration,
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
    cursor_metadata: bool,
}

#[repr(C)]
struct RawCursor {
    serial: u64,
    in_video: bool,
    visible: bool,
    width: u32,
    height: u32,
    hot_x: u32,
    hot_y: u32,
    rgba: *const u8,
}

/// The desktop's mouse pointer, as the viewer needs it to draw the pointer itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    /// Changes whenever anything else here changes.
    pub serial: u64,
    /// The pointer is drawn into the captured frames, so there is no shape to hand out.
    pub in_video: bool,
    /// False while the desktop hides the pointer (video players, games).
    pub visible: bool,
    /// Zero until a shape has been seen.
    pub width: u32,
    pub height: u32,
    pub hot_x: u32,
    pub hot_y: u32,
    /// `width * height * 4` bytes, straight alpha.
    pub rgba: Vec<u8>,
}

#[repr(C)]
struct RawFrame {
    data: *const u8,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
    gpu_texture: u64,
    prepare_us: u32,
}

const RAW_FORMAT_RGBX: u32 = 1;
const RAW_FRAME: i32 = 1;
const RAW_NO_FRAME: i32 = 0;

extern "C" {
    fn pyromirror_capture_create(config: *const RawConfig, error: *mut c_char, error_size: u32) -> *mut RawContext;
    fn pyromirror_capture_acquire(ctx: *mut RawContext, timeout_ms: u32, out_frame: *mut RawFrame) -> i32;
    fn pyromirror_capture_release(ctx: *mut RawContext);
    #[cfg_attr(not(windows), allow(dead_code))]
    fn pyromirror_capture_get_bounds(ctx: *mut RawContext, x: *mut i32, y: *mut i32, width: *mut u32, height: *mut u32) -> bool;
    fn pyromirror_capture_get_cursor(ctx: *mut RawContext, out: *mut RawCursor);
    fn pyromirror_capture_set_gpu(ctx: *mut RawContext, enable: bool) -> bool;
    fn pyromirror_capture_export_texture(ctx: *mut RawContext) -> usize;
    fn pyromirror_capture_get_adapter_luid(ctx: *mut RawContext, luid: *mut u8) -> bool;
    fn pyromirror_capture_hdr_active(ctx: *mut RawContext) -> bool;
    fn pyromirror_capture_last_error(ctx: *mut RawContext) -> *const c_char;
    fn pyromirror_capture_destroy(ctx: *mut RawContext);
}

pub struct Capturer {
    ctx: *mut RawContext,
    permission_remembered: bool,
    #[cfg(target_os = "linux")]
    _portal: portal::PortalSession,
}

// The native context is only used through `&mut self` and has no thread affinity.
unsafe impl Send for Capturer {}

/// Makes this process see real pixels on scaled (high-DPI) displays, so that monitor bounds and
/// pointer coordinates agree with the captured image. Call once, before anything else. No-op
/// outside Windows.
pub fn init_process() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::HiDpi::*;
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

impl Capturer {
    /// Starts capturing. On Linux this may block on a permission dialog shown by the desktop.
    pub fn new(options: &CaptureOptions) -> Result<Self, CaptureError> {
        let mut config = RawConfig {
            output_index: options.output.map_or(-1, |o| o as i32),
            pipewire_fd: -1,
            pipewire_node: 0,
            cursor_metadata: false,
        };

        // Windows needs no consent to capture; on Linux the portal decides.
        #[cfg(not(target_os = "linux"))]
        let permission_remembered = true;

        #[cfg(target_os = "linux")]
        let (portal_session, permission_remembered) = {
            let stream = portal::open()?;
            config.pipewire_fd = stream.pipewire_fd;
            config.pipewire_node = stream.pipewire_node;
            config.cursor_metadata = stream.cursor_metadata;
            (stream.session, stream.remembered)
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
            log::info!(
                "The captured monitor is in HDR mode; the picture is converted to SDR, with highlights \
                 brighter than SDR white clipped"
            );
        }

        Ok(Self {
            ctx,
            permission_remembered,
            #[cfg(target_os = "linux")]
            _portal: portal_session,
        })
    }

    /// The pointer's current shape and visibility, if it differs from the one with serial
    /// `known`. Pass `None` to get it unconditionally.
    pub fn cursor(&mut self, known: Option<u64>) -> Option<Cursor> {
        let mut raw = RawCursor { serial: 0, in_video: false, visible: true, width: 0, height: 0, hot_x: 0, hot_y: 0, rgba: std::ptr::null() };
        unsafe { pyromirror_capture_get_cursor(self.ctx, &mut raw) };
        if known == Some(raw.serial) {
            return None;
        }
        let len = raw.width as usize * raw.height as usize * 4;
        let rgba = if raw.rgba.is_null() || len == 0 {
            Vec::new()
        } else {
            // SAFETY: the native side guarantees `rgba` covers width * height * 4 bytes until
            // the next call into it, which needs `&mut self`.
            unsafe { std::slice::from_raw_parts(raw.rgba, len) }.to_vec()
        };
        let (width, height) = if rgba.is_empty() { (0, 0) } else { (raw.width, raw.height) };
        Some(Cursor { serial: raw.serial, in_video: raw.in_video, visible: raw.visible, width, height, hot_x: raw.hot_x, hot_y: raw.hot_y, rgba })
    }

    /// Whether capture will start next time without asking the person at the desk. False on
    /// Linux desktops whose portal cannot remember the grant.
    pub fn permission_remembered(&self) -> bool {
        self.permission_remembered
    }

    /// Something to feed the client's mouse and keyboard into, if this desktop allows it.
    pub fn input_injector(&self) -> Option<InputInjector> {
        #[cfg(target_os = "linux")]
        {
            match &self._portal {
                portal::PortalSession::Remote(remote) => Some(InputInjector::new(input::Backend::Portal(remote.clone()))),
                portal::PortalSession::ViewOnly(_) => match wlr::WlrInput::connect() {
                    Ok(wlr) => {
                        log::info!("Using the compositor's virtual pointer and keyboard for input");
                        Some(InputInjector::new(input::Backend::Wlr(wlr)))
                    }
                    Err(e) => {
                        log::debug!("No compositor input either: {}", e);
                        None
                    }
                },
            }
        }
        #[cfg(windows)]
        {
            let (mut x, mut y, mut width, mut height) = (0i32, 0i32, 0u32, 0u32);
            unsafe { pyromirror_capture_get_bounds(self.ctx, &mut x, &mut y, &mut width, &mut height) }
                .then(|| InputInjector::new(input::Backend { x, y, width, height }))
        }
    }

    /// The graphics adapter that captures (Windows: its LUID), which is where GPU frames live.
    /// The encoder has to run on the same one to use them.
    pub fn adapter_luid(&self) -> Option<[u8; 8]> {
        let mut luid = [0u8; 8];
        unsafe { pyromirror_capture_get_adapter_luid(self.ctx, luid.as_mut_ptr()) }.then_some(luid)
    }

    /// Asks for frames to stay on the GPU (`Frame::texture`) rather than be read back as pixels.
    /// Returns false where capture cannot do that. Switching it off again makes the next
    /// `next_frame` return the latest image as pixels, so nothing is lost by trying.
    pub fn set_gpu_frames(&mut self, enable: bool) -> bool {
        unsafe { pyromirror_capture_set_gpu(self.ctx, enable) }
    }

    /// A new OS handle to the texture GPU frames are in (Windows: an NT handle to a BGRA8
    /// `ID3D11Texture2D`). The caller owns it.
    pub fn export_texture(&mut self) -> Option<usize> {
        match unsafe { pyromirror_capture_export_texture(self.ctx) } {
            0 => None,
            handle => Some(handle),
        }
    }

    /// Waits up to `timeout` for the desktop to change. `Ok(None)` means nothing was redrawn,
    /// which is the normal state of an idle desktop.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame<'_>>, CaptureError> {
        let mut raw = RawFrame { data: std::ptr::null(), width: 0, height: 0, stride: 0, format: 0, gpu_texture: 0, prepare_us: 0 };
        let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        match unsafe { pyromirror_capture_acquire(self.ctx, timeout_ms, &mut raw) } {
            RAW_FRAME => {
                let on_gpu = raw.gpu_texture != 0 || raw.data.is_null();
                let len = raw.stride as usize * (raw.height as usize).saturating_sub(1) + raw.width as usize * 4;
                Ok(Some(Frame {
                    // SAFETY: the native side guarantees `data` covers every row at `stride`
                    // spacing until the next acquire/release, which needs `&mut self`.
                    data: if on_gpu { &[] } else { unsafe { std::slice::from_raw_parts(raw.data, len) } },
                    texture: (raw.gpu_texture != 0).then_some(raw.gpu_texture),
                    prepare: Duration::from_micros(raw.prepare_us as u64),
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
