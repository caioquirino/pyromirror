//! Shows the remote computer's mouse pointer as this window's own pointer.
//!
//! The host keeps the pointer out of the video and sends its shape instead. Drawing it locally
//! means there is exactly one pointer, it looks like the remote one (arrow, text cursor, resize
//! handles), and it follows the mouse with no network delay.

use pyromirror_proto::{CursorHeader, CURSOR_IN_VIDEO, CURSOR_VISIBLE};
use sdl3::mouse::{Cursor, MouseUtil, SystemCursor};
use sdl3::pixels::PixelFormat;
use sdl3::surface::Surface;

/// Resamples a straight-alpha RGBA image. Colours are weighted by alpha so transparent pixels
/// do not bleed dark or light fringes into the edges.
pub fn resize(rgba: &[u8], width: usize, height: usize, new_width: usize, new_height: usize) -> Vec<u8> {
    let mut out = vec![0u8; new_width * new_height * 4];
    for y in 0..new_height {
        // The source rows and columns this output pixel covers (at least one of each).
        let y0 = y * height / new_height;
        let y1 = ((y + 1) * height).div_ceil(new_height).clamp(y0 + 1, height);
        for x in 0..new_width {
            let x0 = x * width / new_width;
            let x1 = ((x + 1) * width).div_ceil(new_width).clamp(x0 + 1, width);
            let (mut r, mut g, mut b, mut a, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = &rgba[(sy * width + sx) * 4..][..4];
                    let alpha = p[3] as u32;
                    r += p[0] as u32 * alpha;
                    g += p[1] as u32 * alpha;
                    b += p[2] as u32 * alpha;
                    a += alpha;
                    n += 1;
                }
            }
            let o = &mut out[(y * new_width + x) * 4..][..4];
            if a > 0 {
                o[0] = (r / a) as u8;
                o[1] = (g / a) as u8;
                o[2] = (b / a) as u8;
                o[3] = (a / n) as u8;
            }
        }
    }
    out
}

/// What the window's pointer should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// The system's ordinary arrow: no shape known yet, or the pointer is over the toolbar.
    Arrow,
    /// Nothing: the remote desktop hides its pointer, or draws it into the video itself.
    Hidden,
    /// The remote pointer's own shape.
    Remote,
}

/// Decides the look from what the host last said.
pub fn look(shape: Option<&CursorHeader>, over_toolbar: bool) -> Look {
    match shape {
        _ if over_toolbar => Look::Arrow,
        None => Look::Arrow,
        Some(h) if h.flags & CURSOR_IN_VIDEO != 0 => Look::Hidden,
        Some(h) if h.flags & CURSOR_VISIBLE == 0 => Look::Hidden,
        Some(h) if h.width == 0 || h.height == 0 => Look::Arrow,
        Some(_) => Look::Remote,
    }
}

pub struct Pointer {
    shape: Option<(CursorHeader, Vec<u8>)>,
    arrow: Option<Cursor>,
    remote: Option<Cursor>,
    /// Scale the remote cursor was last built for.
    built_for: f32,
    shown: Option<Look>,
}

impl Pointer {
    pub fn new() -> Self {
        Self { shape: None, arrow: Cursor::from_system(SystemCursor::Arrow).ok(), remote: None, built_for: 0.0, shown: None }
    }

    /// Takes a shape received from the host.
    pub fn set_shape(&mut self, header: CursorHeader, rgba: Vec<u8>) {
        self.shape = Some((header, rgba));
        self.remote = None;
        self.shown = None;
    }

    /// Makes the window's pointer match. `picture_scale` is how many window pixels one pixel of
    /// the stream takes up. Cheap to call every frame.
    pub fn apply(&mut self, mouse: &MouseUtil, picture_scale: f32, over_toolbar: bool) {
        let header = self.shape.as_ref().map(|(h, _)| *h);
        let look = look(header.as_ref(), over_toolbar);

        if look == Look::Remote {
            let (header, rgba) = self.shape.as_ref().expect("Remote implies a shape");
            // The image is in desktop pixels; the stream may have been shrunk by the host and is
            // then scaled again by this window.
            let factor = (picture_scale / header.scale.max(1) as f32).clamp(0.25, 4.0);
            let stale = self.remote.is_none() || (factor / self.built_for - 1.0).abs() > 0.05;
            if stale {
                self.remote = build(header, rgba, factor);
                self.built_for = factor;
                self.shown = None;
            }
        }

        if self.shown == Some(look) {
            return;
        }
        match look {
            Look::Hidden => mouse.show_cursor(false),
            Look::Arrow => {
                if let Some(arrow) = &self.arrow {
                    arrow.set();
                }
                mouse.show_cursor(true);
            }
            Look::Remote => {
                // If the image could not be turned into a cursor, fall back to the arrow.
                match self.remote.as_ref().or(self.arrow.as_ref()) {
                    Some(cursor) => cursor.set(),
                    None => {}
                }
                mouse.show_cursor(true);
            }
        }
        self.shown = Some(look);
    }
}

fn build(header: &CursorHeader, rgba: &[u8], factor: f32) -> Option<Cursor> {
    let (w, h) = (header.width as usize, header.height as usize);
    if rgba.len() < w * h * 4 {
        return None;
    }
    let (new_w, new_h) = (((w as f32 * factor).round() as usize).max(1), ((h as f32 * factor).round() as usize).max(1));
    let mut pixels = if (new_w, new_h) == (w, h) { rgba[..w * h * 4].to_vec() } else { resize(rgba, w, h, new_w, new_h) };
    let surface = Surface::from_data(&mut pixels, new_w as u32, new_h as u32, (new_w * 4) as u32, PixelFormat::RGBA32).ok()?;
    let hot = |v: u16, limit: usize| ((v as f32 * factor).round() as i32).clamp(0, limit as i32 - 1);
    Cursor::from_surface(surface, hot(header.hot_x, new_w), hot(header.hot_y, new_h)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(width: u16, flags: u16) -> CursorHeader {
        CursorHeader { width, height: width, hot_x: 0, hot_y: 0, flags, scale: 1 }
    }

    #[test]
    fn look_follows_what_the_host_reports() {
        assert_eq!(look(None, false), Look::Arrow, "until the host says anything, keep the arrow");
        assert_eq!(look(Some(&header(32, CURSOR_VISIBLE)), false), Look::Remote);
        assert_eq!(look(Some(&header(32, 0)), false), Look::Hidden, "the remote desktop hid its pointer");
        assert_eq!(look(Some(&header(0, CURSOR_VISIBLE | CURSOR_IN_VIDEO)), false), Look::Hidden, "one pointer, not two");
        assert_eq!(look(Some(&header(0, CURSOR_VISIBLE)), false), Look::Arrow, "visible but no image yet");
        assert_eq!(look(Some(&header(32, 0)), true), Look::Arrow, "the toolbar always gets a pointer");
    }

    #[test]
    fn resize_keeps_colour_and_does_not_fringe() {
        // 2x2: one opaque red pixel, three fully transparent ones that happen to hold white.
        let src = [255, 0, 0, 255, 255, 255, 255, 0, 255, 255, 255, 0, 255, 255, 255, 0];
        let half = resize(&src, 2, 2, 1, 1);
        assert_eq!(&half[..3], &[255, 0, 0], "transparent pixels must not tint the colour");
        assert_eq!(half[3], 63, "a quarter covered");

        // Doubling an opaque image keeps it opaque and the same colour.
        let solid = [10, 20, 30, 255].repeat(4);
        let big = resize(&solid, 2, 2, 4, 4);
        assert_eq!(big.len(), 4 * 4 * 4);
        assert!(big.chunks_exact(4).all(|p| p == [10, 20, 30, 255]));
        // Same size in: visible pixels and all alpha values are unchanged (what a fully
        // transparent pixel "holds" does not matter).
        let same = resize(&src, 2, 2, 2, 2);
        assert_eq!(&same[..4], &src[..4]);
        assert!(same.chunks_exact(4).zip(src.chunks_exact(4)).all(|(a, b)| a[3] == b[3]));
    }
}
