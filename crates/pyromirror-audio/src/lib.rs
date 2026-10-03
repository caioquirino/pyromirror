//! Captures what the machine is playing (the default output device's loopback / monitor) as
//! interleaved 16-bit stereo.
//!
//! - Windows: WASAPI loopback through cpal.
//! - Linux: a PipeWire stream on the default sink's monitor.

use thiserror::Error;

/// Samples are always delivered as interleaved stereo.
pub const CHANNELS: u16 = 2;

#[derive(Error, Debug)]
#[error("audio capture failed: {0}")]
pub struct AudioError(String);

/// Called on a realtime audio thread with interleaved stereo samples; must not block.
pub type SampleCallback = Box<dyn FnMut(&[i16]) + Send + 'static>;

pub struct AudioCapture {
    sample_rate: u32,
    _backend: backend::Backend,
}

impl AudioCapture {
    /// Starts capturing. `on_samples` keeps being called until the capture is dropped; nothing is
    /// delivered while the machine is silent on Windows.
    pub fn start(on_samples: impl FnMut(&[i16]) + Send + 'static) -> Result<Self, AudioError> {
        let (backend, sample_rate) = backend::start(Box::new(on_samples))?;
        Ok(Self { sample_rate, _backend: backend })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

#[cfg(target_os = "linux")]
mod backend {
    use std::ffi::{c_char, c_void, CStr};

    use super::{AudioError, SampleCallback, CHANNELS};

    #[repr(C)]
    struct RawContext {
        _private: [u8; 0],
    }

    extern "C" {
        fn pyromirror_audio_create(
            callback: extern "C" fn(*mut c_void, *const i16, u32),
            user: *mut c_void,
            error: *mut c_char,
            error_size: u32,
        ) -> *mut RawContext;
        fn pyromirror_audio_destroy(ctx: *mut RawContext);
    }

    pub struct Backend {
        ctx: *mut RawContext,
        callback: *mut SampleCallback,
    }

    // The context is only touched again on drop.
    unsafe impl Send for Backend {}

    extern "C" fn trampoline(user: *mut c_void, samples: *const i16, frames: u32) {
        // SAFETY: `user` is the boxed callback owned by `Backend`, which outlives the stream, and
        // PipeWire calls this from a single thread.
        let callback = unsafe { &mut *(user as *mut SampleCallback) };
        let samples = unsafe { std::slice::from_raw_parts(samples, frames as usize * CHANNELS as usize) };
        callback(samples);
    }

    pub fn start(callback: SampleCallback) -> Result<(Backend, u32), AudioError> {
        let callback = Box::into_raw(Box::new(callback));
        let mut error = [0 as c_char; 256];
        let ctx = unsafe {
            pyromirror_audio_create(trampoline, callback as *mut c_void, error.as_mut_ptr(), error.len() as u32)
        };
        if ctx.is_null() {
            drop(unsafe { Box::from_raw(callback) });
            let message = unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy().into_owned();
            return Err(AudioError(message));
        }
        Ok((Backend { ctx, callback }, 48000))
    }

    impl Drop for Backend {
        fn drop(&mut self) {
            unsafe {
                // Stops the PipeWire thread first, so the callback is no longer running.
                pyromirror_audio_destroy(self.ctx);
                drop(Box::from_raw(self.callback));
            }
        }
    }
}

#[cfg(windows)]
mod backend {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::SampleFormat;

    use super::{AudioError, SampleCallback};

    pub struct Backend {
        _stream: cpal::Stream,
    }

    fn err(context: &str, e: impl std::fmt::Display) -> AudioError {
        AudioError(format!("{context}: {e}"))
    }

    /// Reduces interleaved `channels`-channel audio to stereo and hands it to `callback`.
    fn deliver<T: Copy>(
        data: &[T],
        channels: usize,
        to_i16: impl Fn(T) -> i16,
        scratch: &mut Vec<i16>,
        callback: &mut SampleCallback,
    ) {
        scratch.clear();
        for frame in data.chunks_exact(channels) {
            let left = to_i16(frame[0]);
            let right = if channels > 1 { to_i16(frame[1]) } else { left };
            scratch.extend([left, right]);
        }
        if !scratch.is_empty() {
            callback(scratch);
        }
    }

    pub fn start(mut callback: SampleCallback) -> Result<(Backend, u32), AudioError> {
        // Opening an input stream on an output device makes WASAPI record what it plays.
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| AudioError("no default audio output device".into()))?;
        let supported = device.default_output_config().map_err(|e| err("could not query the output format", e))?;
        let channels = supported.channels() as usize;
        let sample_rate = supported.sample_rate();
        let config = supported.config();
        let on_error = |e| log::warn!("Audio capture error: {}", e);
        let mut scratch = Vec::new();

        let stream = match supported.sample_format() {
            SampleFormat::F32 => device.build_input_stream(
                config,
                move |data: &[f32], _| {
                    deliver(data, channels, |s| (s.clamp(-1.0, 1.0) * 32767.0) as i16, &mut scratch, &mut callback)
                },
                on_error,
                None,
            ),
            SampleFormat::I16 => device.build_input_stream(
                config,
                move |data: &[i16], _| deliver(data, channels, |s| s, &mut scratch, &mut callback),
                on_error,
                None,
            ),
            other => return Err(AudioError(format!("unsupported output sample format {other:?}"))),
        }
        .map_err(|e| err("could not open the loopback stream", e))?;
        stream.play().map_err(|e| err("could not start the loopback stream", e))?;

        Ok((Backend { _stream: stream }, sample_rate))
    }
}
