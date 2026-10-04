//! Where frames come from: the desktop, or a generated test pattern.

use std::time::{Duration, Instant};

use anyhow::bail;
use pyromirror_capture::{Capturer, PixelFormat as CaptureFormat};
use pyromirror_codec::PixelFormat;

pub struct SourceFrame<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
    /// Set when the image stayed on the GPU: `data` is empty, the size is the desktop's (not
    /// scaled), and the encoder reads the capturer's texture with this id.
    pub texture: Option<u64>,
    /// Linux: the DMA-BUF that texture is.
    pub dmabuf: Option<pyromirror_capture::DmaBuf>,
    /// Time spent preparing the frame: reading it back and shrinking it, or the GPU copy.
    pub prepare: Duration,
}

enum Kind {
    Capture(Capturer),
    Pattern(TestPattern),
}

pub struct Source {
    kind: Kind,
    lost: bool,
    /// Integer downscale factor applied to every frame (1 = off).
    scale: u32,
    scaled: Vec<u8>,
}

impl Source {
    pub fn capture(capturer: Capturer) -> Self {
        Self { kind: Kind::Capture(capturer), lost: false, scale: 1, scaled: Vec::new() }
    }

    pub fn pattern(pattern: TestPattern) -> Self {
        Self { kind: Kind::Pattern(pattern), lost: false, scale: 1, scaled: Vec::new() }
    }

    /// Shrinks every frame by an integer factor before it is handed out.
    pub fn with_scale(mut self, scale: u32) -> Self {
        self.scale = scale.max(1);
        self
    }

    /// The factor frames are shrunk by.
    pub fn scale(&self) -> u32 {
        self.scale
    }

    /// Whether frames come at a rate of the source's own choosing, which may be more than the
    /// stream wants. The test pattern makes exactly as many as it is asked for.
    pub fn sets_its_own_rate(&self) -> bool {
        matches!(self.kind, Kind::Capture(_))
    }

    /// Asks for frames to stay on the GPU. False where the source cannot do that.
    pub fn set_gpu_frames(&mut self, enable: bool) -> bool {
        match &mut self.kind {
            Kind::Capture(capturer) => capturer.set_gpu_frames(enable),
            Kind::Pattern(_) => false,
        }
    }

    /// A new handle to the texture GPU frames are in; the caller owns it.
    pub fn export_texture(&mut self) -> Option<usize> {
        match &mut self.kind {
            Kind::Capture(capturer) => capturer.export_texture(),
            Kind::Pattern(_) => None,
        }
    }

    /// The desktop pointer, if it changed since the one with serial `known`.
    pub fn cursor(&mut self, known: Option<u64>) -> Option<pyromirror_capture::Cursor> {
        match &mut self.kind {
            Kind::Capture(capturer) => capturer.cursor(known),
            // The pattern has no pointer; the viewer keeps its own.
            Kind::Pattern(_) => None,
        }
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
        let Self { kind, lost, scale, scaled } = self;
        let frame = match kind {
            Kind::Capture(capturer) => match capturer.next_frame(timeout) {
                Ok(frame) => frame.map(|frame| SourceFrame {
                    data: frame.data,
                    width: frame.width,
                    height: frame.height,
                    stride: frame.stride,
                    format: match frame.format {
                        CaptureFormat::Bgrx => PixelFormat::Bgrx,
                        CaptureFormat::Rgbx => PixelFormat::Rgbx,
                    },
                    texture: frame.texture,
                    dmabuf: frame.dmabuf,
                    prepare: frame.prepare,
                }),
                Err(e) => {
                    *lost = true;
                    return Err(e.into());
                }
            },
            Kind::Pattern(pattern) => Some(pattern.next_frame(timeout)),
        };

        Ok(match frame {
            // A frame on the GPU is shrunk there, by the encoder.
            Some(frame) if *scale > 1 && frame.texture.is_none() => {
                let start = Instant::now();
                let (width, height) = downscale(&frame, *scale as usize, scaled);
                let prepare = frame.prepare + start.elapsed();
                Some(SourceFrame { data: scaled, width, height, stride: width * 4, format: frame.format, texture: None, dmabuf: None, prepare })
            }
            other => other,
        })
    }
}

/// Colour gradient with a moving white bar: makes dropped frames, tearing and swapped colour
/// channels easy to spot (red grows to the right, green grows downwards).
pub struct TestPattern {
    width: u32,
    height: u32,
    frame: u64,
    /// When the next picture is due.
    next_at: Instant,
    pixels: Vec<u8>,
}

impl TestPattern {
    pub fn new(width: u32, height: u32) -> Self {
        let mut pattern = Self { width, height, frame: 0, next_at: Instant::now(), pixels: vec![0; width as usize * height as usize * 4] };
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

    /// A new picture every `interval`, like a desktop that refreshes at the stream's rate.
    fn next_frame(&mut self, interval: Duration) -> SourceFrame<'_> {
        self.frame += 1;
        self.render();
        pyromirror_net::sleep_until(self.next_at);
        self.next_at = (self.next_at + interval).max(Instant::now());
        SourceFrame {
            data: &self.pixels,
            width: self.width,
            height: self.height,
            stride: self.width * 4,
            format: PixelFormat::Rgbx,
            texture: None,
            dmabuf: None,
            prepare: Duration::ZERO,
        }
    }
}

/// Box-filters `frame` down by `factor` in both directions into `out` (tightly packed rows).
/// Rows and columns that do not fill a whole block are dropped. Returns the new size.
fn downscale(frame: &SourceFrame<'_>, factor: usize, out: &mut Vec<u8>) -> (u32, u32) {
    let (out_w, out_h) = (frame.width as usize / factor, frame.height as usize / factor);
    out.resize(out_w * out_h * 4, 255);
    if out_w == 0 || out_h == 0 {
        return (0, 0);
    }

    let (src, stride) = (frame.data, frame.stride as usize);
    let area = (factor * factor) as u32;
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
    let band_rows = out_h.div_ceil(threads);

    std::thread::scope(|scope| {
        for (band, out) in out.chunks_mut(band_rows * out_w * 4).enumerate() {
            scope.spawn(move || {
                for (i, out_row) in out.chunks_exact_mut(out_w * 4).enumerate() {
                    let first_src_row = (band * band_rows + i) * factor;
                    for (x, px) in out_row.chunks_exact_mut(4).enumerate() {
                        let mut sum = [area / 2; 3];
                        for row in first_src_row..first_src_row + factor {
                            let block = &src[row * stride + x * factor * 4..][..factor * 4];
                            for p in block.chunks_exact(4) {
                                sum[0] += p[0] as u32;
                                sum[1] += p[1] as u32;
                                sum[2] += p[2] as u32;
                            }
                        }
                        px[0] = (sum[0] / area) as u8;
                        px[1] = (sum[1] / area) as u8;
                        px[2] = (sum[2] / area) as u8;
                    }
                }
            });
        }
    });
    (out_w as u32, out_h as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_averages_blocks_and_drops_remainder() {
        // 5x4 image with stride padding; left 2x2 blocks are 10/30 (avg 20), next are all 100.
        let (w, h, stride) = (5usize, 4usize, 24usize);
        let mut data = vec![0u8; stride * h];
        for y in 0..h {
            for x in 0..w {
                let v = if x < 2 { if (x + y) % 2 == 0 { 10 } else { 30 } } else { 100 };
                data[y * stride + x * 4..][..4].copy_from_slice(&[v, v / 2, 7, 0]);
            }
        }
        let frame = SourceFrame { data: &data, width: 5, height: 4, stride: stride as u32, format: PixelFormat::Bgrx, texture: None, dmabuf: None, prepare: Duration::ZERO };
        let mut out = Vec::new();
        assert_eq!(downscale(&frame, 2, &mut out), (2, 2));
        assert_eq!(out.len(), 2 * 2 * 4);
        for row in out.chunks_exact(8) {
            assert_eq!(&row[..4], &[20, 10, 7, 255]);
            assert_eq!(&row[4..], &[100, 50, 7, 255]);
        }
    }
}
