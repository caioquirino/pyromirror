//! Packed 8-bit RGB <-> planar YCbCr (BT.709, full range, centre-sited chroma).
//!
//! Fixed-point Q16, split across a few threads by row bands.

use crate::Chroma;

/// Byte order of a packed 32-bit pixel. The fourth byte is ignored on input and written as 255.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// B, G, R, X in memory order (DXGI `B8G8R8A8`, PipeWire `BGRx`/`BGRA`).
    Bgrx,
    /// R, G, B, X in memory order (PipeWire `RGBx`/`RGBA`, SDL `RGBA32`).
    Rgbx,
}

impl PixelFormat {
    /// Byte offsets of (R, B) within a pixel; G is always at 1.
    #[inline]
    fn rb(self) -> (usize, usize) {
        match self {
            PixelFormat::Bgrx => (2, 0),
            PixelFormat::Rgbx => (0, 2),
        }
    }
}

#[inline(always)]
fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[inline(always)]
fn luma(r: i32, g: i32, b: i32) -> u8 {
    ((13933 * r + 46871 * g + 4732 * b + 32768) >> 16) as u8
}

#[inline(always)]
fn chroma_b(r: i32, g: i32, b: i32) -> u8 {
    clamp8(((-7509 * r - 25259 * g + 32768 * b + 32768) >> 16) + 128)
}

#[inline(always)]
fn chroma_r(r: i32, g: i32, b: i32) -> u8 {
    clamp8(((32768 * r - 29763 * g - 3005 * b + 32768) >> 16) + 128)
}

#[inline(always)]
fn to_rgb(y: u8, cb: u8, cr: u8) -> (u8, u8, u8) {
    let y = ((y as i32) << 16) + 32768;
    let cb = cb as i32 - 128;
    let cr = cr as i32 - 128;
    (
        clamp8((y + 103206 * cr) >> 16),
        clamp8((y - 12276 * cb - 30679 * cr) >> 16),
        clamp8((y + 121609 * cb) >> 16),
    )
}

/// Number of row bands to process in parallel, and rows per band (always even).
fn bands(width: usize, height: usize) -> usize {
    if width * height < 512 * 512 {
        return height.max(2);
    }
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
    let rows = height.div_ceil(threads);
    (rows + (rows & 1)).max(2)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn packed_to_planar(
    src: &[u8],
    stride: usize,
    format: PixelFormat,
    width: usize,
    height: usize,
    chroma: Chroma,
    y: &mut [u8],
    cb: &mut [u8],
    cr: &mut [u8],
) {
    let band_rows = bands(width, height);
    let (cw, c_band) = match chroma {
        Chroma::C444 => (width, width * band_rows),
        Chroma::C420 => (width / 2, (width / 2) * (band_rows / 2)),
    };

    std::thread::scope(|scope| {
        let iter = y
            .chunks_mut(width * band_rows)
            .zip(cb.chunks_mut(c_band))
            .zip(cr.chunks_mut(c_band))
            .enumerate();
        for (band, ((y, cb), cr)) in iter {
            let src = &src[band * band_rows * stride..];
            let mut work = move || match chroma {
                Chroma::C444 => band_to_444(src, stride, format, width, y, cb, cr),
                Chroma::C420 => band_to_420(src, stride, format, width, cw, y, cb, cr),
            };
            if band_rows >= height {
                work();
            } else {
                scope.spawn(work);
            }
        }
    });
}

fn band_to_444(src: &[u8], stride: usize, format: PixelFormat, width: usize, y: &mut [u8], cb: &mut [u8], cr: &mut [u8]) {
    let (ri, bi) = format.rb();
    let rows = y.chunks_exact_mut(width).zip(cb.chunks_exact_mut(width)).zip(cr.chunks_exact_mut(width));
    for (row, ((y, cb), cr)) in rows.enumerate() {
        let src = &src[row * stride..row * stride + width * 4];
        for (x, px) in src.chunks_exact(4).enumerate() {
            let (r, g, b) = (px[ri] as i32, px[1] as i32, px[bi] as i32);
            y[x] = luma(r, g, b);
            cb[x] = chroma_b(r, g, b);
            cr[x] = chroma_r(r, g, b);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn band_to_420(src: &[u8], stride: usize, format: PixelFormat, width: usize, cw: usize, y: &mut [u8], cb: &mut [u8], cr: &mut [u8]) {
    let (ri, bi) = format.rb();
    let rows = y.chunks_exact_mut(width * 2).zip(cb.chunks_exact_mut(cw)).zip(cr.chunks_exact_mut(cw));
    for (pair, ((y, cb), cr)) in rows.enumerate() {
        let top = &src[pair * 2 * stride..pair * 2 * stride + width * 4];
        let bottom = &src[(pair * 2 + 1) * stride..(pair * 2 + 1) * stride + width * 4];
        let (y_top, y_bottom) = y.split_at_mut(width);
        for x in 0..cw {
            let mut sum = (0i32, 0i32, 0i32);
            for (row, y_row) in [(top, &mut *y_top), (bottom, &mut *y_bottom)] {
                for i in [x * 2, x * 2 + 1] {
                    let px = &row[i * 4..i * 4 + 4];
                    let (r, g, b) = (px[ri] as i32, px[1] as i32, px[bi] as i32);
                    y_row[i] = luma(r, g, b);
                    sum = (sum.0 + r, sum.1 + g, sum.2 + b);
                }
            }
            let (r, g, b) = ((sum.0 + 2) >> 2, (sum.1 + 2) >> 2, (sum.2 + 2) >> 2);
            cb[x] = chroma_b(r, g, b);
            cr[x] = chroma_r(r, g, b);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn planar_to_packed(
    y: &[u8],
    cb: &[u8],
    cr: &[u8],
    width: usize,
    height: usize,
    chroma: Chroma,
    dst: &mut [u8],
    stride: usize,
    format: PixelFormat,
) {
    let band_rows = bands(width, height);
    let (ri, bi) = format.rb();
    let (cw, shift) = match chroma {
        Chroma::C444 => (width, 0),
        Chroma::C420 => (width / 2, 1),
    };

    std::thread::scope(|scope| {
        for (band, dst) in dst.chunks_mut(stride * band_rows).enumerate() {
            let first_row = band * band_rows;
            let mut work = move || {
                for (i, out) in dst.chunks_mut(stride).enumerate() {
                    let row = first_row + i;
                    if row >= height || out.len() < width * 4 {
                        break;
                    }
                    let y = &y[row * width..(row + 1) * width];
                    let c = (row >> shift) * cw;
                    let (cb, cr) = (&cb[c..c + cw], &cr[c..c + cw]);
                    for (x, px) in out[..width * 4].chunks_exact_mut(4).enumerate() {
                        let (r, g, b) = to_rgb(y[x], cb[x >> shift], cr[x >> shift]);
                        px[ri] = r;
                        px[1] = g;
                        px[bi] = b;
                        px[3] = 255;
                    }
                }
            };
            if band_rows >= height {
                work();
            } else {
                scope.spawn(work);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(width: usize, height: usize, chroma: Chroma, format: PixelFormat) -> i32 {
        let stride = width * 4 + 8;
        let mut src = vec![0u8; stride * height];
        for row in 0..height {
            for x in 0..width {
                // Smooth gradients so that 4:2:0 averaging stays within a small error too.
                let px = &mut src[row * stride + x * 4..][..4];
                px[0] = (x * 255 / width) as u8;
                px[1] = (row * 255 / height) as u8;
                px[2] = ((x + row) * 255 / (width + height)) as u8;
                px[3] = 7;
            }
        }
        let (cw, ch) = if chroma == Chroma::C420 { (width / 2, height / 2) } else { (width, height) };
        let (mut y, mut cb, mut cr) = (vec![0; width * height], vec![0; cw * ch], vec![0; cw * ch]);
        packed_to_planar(&src, stride, format, width, height, chroma, &mut y, &mut cb, &mut cr);

        let mut out = vec![0u8; stride * height];
        planar_to_packed(&y, &cb, &cr, width, height, chroma, &mut out, stride, format);

        let mut worst = 0;
        for row in 0..height {
            for x in 0..width {
                let (a, b) = (&src[row * stride + x * 4..][..4], &out[row * stride + x * 4..][..4]);
                for c in 0..3 {
                    worst = worst.max((a[c] as i32 - b[c] as i32).abs());
                }
                assert_eq!(b[3], 255);
            }
        }
        worst
    }

    #[test]
    fn roundtrip_444_is_near_lossless() {
        assert!(roundtrip(64, 48, Chroma::C444, PixelFormat::Bgrx) <= 2);
        assert!(roundtrip(1280, 721, Chroma::C444, PixelFormat::Rgbx) <= 2);
    }

    #[test]
    fn roundtrip_420_is_close_on_gradients() {
        assert!(roundtrip(64, 48, Chroma::C420, PixelFormat::Bgrx) <= 6);
        assert!(roundtrip(1920, 1080, Chroma::C420, PixelFormat::Rgbx) <= 6);
    }

    #[test]
    fn primaries_keep_their_channel_order() {
        for format in [PixelFormat::Bgrx, PixelFormat::Rgbx] {
            let (ri, bi) = format.rb();
            let mut src = [0u8; 4];
            src[ri] = 255;
            let (mut y, mut cb, mut cr) = ([0u8; 1], [0u8; 1], [0u8; 1]);
            packed_to_planar(&src, 4, format, 1, 1, Chroma::C444, &mut y, &mut cb, &mut cr);
            assert!(cr[0] > 250 && cb[0] < 128, "pure red should have max Cr");
            let mut out = [0u8; 4];
            planar_to_packed(&y, &cb, &cr, 1, 1, Chroma::C444, &mut out, 4, format);
            assert!(out[ri] > 250 && out[1] < 5 && out[bi] < 5);
        }
    }
}
