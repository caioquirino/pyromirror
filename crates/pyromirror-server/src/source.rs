//! Where frames come from: the desktop, or a generated test pattern.

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use pyromirror_capture::{Capturer, PixelFormat as CaptureFormat};
use pyromirror_codec::PixelFormat;

pub struct SourceFrame<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
}

enum Kind {
    Capture(Capturer),
    Pattern(TestPattern),
}

pub struct Source {
    kind: Kind,
    lost: bool,
}

impl Source {
    pub fn capture(capturer: Capturer) -> Self {
        Self { kind: Kind::Capture(capturer), lost: false }
    }

    pub fn pattern(pattern: TestPattern) -> Self {
        Self { kind: Kind::Pattern(pattern), lost: false }
    }

    /// True once the capture backend has failed for good.
    pub fn is_lost(&self) -> bool {
        self.lost
    }

    /// Waits up to `timeout` for a new frame; `None` means the picture did not change.
    pub fn next_frame(&mut self, timeout: Duration) -> anyhow::Result<Option<SourceFrame<'_>>> {
        if self.lost {
            bail!("desktop capture is no longer running");
        }
        match &mut self.kind {
            Kind::Capture(capturer) => match capturer.next_frame(timeout) {
                Ok(frame) => Ok(frame.map(|frame| SourceFrame {
                    data: frame.data,
                    width: frame.width,
                    height: frame.height,
                    stride: frame.stride,
                    format: match frame.format {
                        CaptureFormat::Bgrx => PixelFormat::Bgrx,
                        CaptureFormat::Rgbx => PixelFormat::Rgbx,
                    },
                })),
                Err(e) => {
                    self.lost = true;
                    Err(e.into())
                }
            },
            Kind::Pattern(pattern) => Ok(Some(pattern.next_frame(timeout))),
        }
    }

    /// Blocks until the first frame arrives (which is what reveals the desktop resolution) and
    /// passes it to `f`.
    pub fn with_first_frame<R>(
        &mut self,
        timeout: Duration,
        f: impl FnOnce(&SourceFrame<'_>) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(frame) = self.next_frame(Duration::from_millis(100))? {
                return f(&frame);
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("no frame arrived from the capture backend within {:?}", timeout));
            }
        }
    }
}

/// Colour gradient with a moving white bar: makes dropped frames, tearing and swapped colour
/// channels easy to spot (red grows to the right, green grows downwards).
pub struct TestPattern {
    width: u32,
    height: u32,
    frame: u64,
    pixels: Vec<u8>,
}

impl TestPattern {
    pub fn new(width: u32, height: u32) -> Self {
        let mut pattern = Self { width, height, frame: 0, pixels: vec![0; width as usize * height as usize * 4] };
        pattern.render();
        pattern
    }

    fn render(&mut self) {
        let (w, h) = (self.width as usize, self.height as usize);
        let bar = (self.frame as usize * 8) % w;
        for (y, row) in self.pixels.chunks_exact_mut(w * 4).enumerate() {
            let green = (y * 255 / h) as u8;
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let in_bar = (x + w - bar) % w < 40;
                let rgb = if in_bar { [255, 255, 255] } else { [(x * 255 / w) as u8, green, 180] };
                px[..3].copy_from_slice(&rgb);
                px[3] = 255;
            }
        }
    }

    fn next_frame(&mut self, _timeout: Duration) -> SourceFrame<'_> {
        self.frame += 1;
        self.render();
        SourceFrame {
            data: &self.pixels,
            width: self.width,
            height: self.height,
            stride: self.width * 4,
            format: PixelFormat::Rgbx,
        }
    }
}
