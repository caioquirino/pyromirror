//! The session toolbar: slides in when the pointer touches the top edge of the window.
//!
//! Drawn with SDL's renderer and its built-in 8x8 font (at twice the size), so it needs no font
//! files and looks the same everywhere.

use std::time::{Duration, Instant};

use sdl3::pixels::Color;
use sdl3::render::{BlendMode, FRect, WindowCanvas};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Fullscreen,
    KeyboardGrab,
    MouseLock,
    Mute,
    Disconnect,
}

const BUTTONS: [(Action, &str); 5] = [
    (Action::Fullscreen, "Fullscreen"),
    (Action::KeyboardGrab, "Grab keys"),
    (Action::MouseLock, "Lock mouse"),
    (Action::Mute, "Mute"),
    (Action::Disconnect, "Disconnect"),
];

const TEXT_SCALE: f32 = 2.0;
const CHAR: f32 = 8.0 * TEXT_SCALE;
const HEIGHT: f32 = 40.0;
const PAD: f32 = 12.0;
const GAP: f32 = 6.0;
/// Touching this many pixels at the top of the window reveals the toolbar.
const REVEAL_ZONE: f32 = 4.0;

const BAR: Color = Color::RGBA(0x12, 0x15, 0x1c, 235);
const BUTTON: Color = Color::RGB(0x26, 0x2c, 0x3a);
const BUTTON_HOVER: Color = Color::RGB(0x3a, 0x43, 0x57);
const ACCENT: Color = Color::RGB(0xff, 0x7a, 0x2f);
const DANGER: Color = Color::RGB(0xc8, 0x3c, 0x3d);
const TEXT: Color = Color::RGB(0xe6, 0xe9, 0xef);
const MUTED: Color = Color::RGB(0x8b, 0x93, 0xa3);

pub struct Toolbar {
    /// Shown at least until this time (used for the hint right after connecting).
    pinned_until: Instant,
    pointer: Option<(f32, f32)>,
    visible: bool,
}

fn text_width(text: &str) -> f32 {
    text.chars().count() as f32 * CHAR
}

fn button_width(label: &str) -> f32 {
    text_width(label) + 2.0 * PAD
}

impl Toolbar {
    pub fn new() -> Self {
        Self { pinned_until: Instant::now() + Duration::from_secs(4), pointer: None, visible: true }
    }

    /// The bar and its buttons, centred at the top of a window `window_width` wide.
    fn layout(window_width: f32, stats: &str) -> (FRect, [FRect; BUTTONS.len()]) {
        let buttons_width: f32 = BUTTONS.iter().map(|(_, label)| button_width(label) + GAP).sum();
        let stats_width = if stats.is_empty() { 0.0 } else { text_width(stats) + PAD };
        let mut width = buttons_width + GAP + stats_width;
        // Drop the statistics rather than overflow a narrow window.
        if width > window_width {
            width = buttons_width + GAP;
        }
        let left = ((window_width - width) / 2.0).max(0.0);

        let mut x = left + GAP;
        let rects = BUTTONS.map(|(_, label)| {
            let rect = FRect::new(x, 5.0, button_width(label), HEIGHT - 10.0);
            x += rect.w + GAP;
            rect
        });
        (FRect::new(left, 0.0, width, HEIGHT), rects)
    }

    fn contains(rect: &FRect, (x, y): (f32, f32)) -> bool {
        x >= rect.x && x < rect.x + rect.w && y >= rect.y && y < rect.y + rect.h
    }

    /// Recomputes visibility. Returns true if the toolbar needs to be redrawn.
    pub fn update(&mut self, pointer: Option<(f32, f32)>, window_width: f32, stats: &str) -> bool {
        let (bar, _) = Self::layout(window_width, stats);
        let over = pointer.is_some_and(|p| {
            // Reveal from the very top edge anywhere; keep it while the pointer is on it.
            p.1 < REVEAL_ZONE || (self.visible && Self::contains(&bar, p))
        });
        let visible = over || Instant::now() < self.pinned_until;
        let changed = visible != self.visible || (visible && pointer != self.pointer);
        self.visible = visible;
        self.pointer = pointer;
        changed
    }

    #[cfg(test)]
    fn visible(&self) -> bool {
        self.visible
    }

    /// Whether a pointer position belongs to the toolbar rather than the remote desktop.
    pub fn captures(&self, pointer: (f32, f32), window_width: f32, stats: &str) -> bool {
        self.visible && Self::contains(&Self::layout(window_width, stats).0, pointer)
    }

    pub fn click(&self, pointer: (f32, f32), window_width: f32, stats: &str) -> Option<Action> {
        if !self.visible {
            return None;
        }
        let (_, rects) = Self::layout(window_width, stats);
        rects.iter().position(|r| Self::contains(r, pointer)).map(|i| BUTTONS[i].0)
    }

    /// Draws the toolbar. `active` says which toggles are on.
    pub fn draw(&self, canvas: &mut WindowCanvas, window_width: f32, stats: &str, active: impl Fn(Action) -> bool) {
        if !self.visible {
            return;
        }
        let (bar, rects) = Self::layout(window_width, stats);

        canvas.set_blend_mode(BlendMode::Blend);
        canvas.set_draw_color(BAR);
        let _ = canvas.fill_rect(bar);

        let text = |canvas: &mut WindowCanvas, label: &str, x: f32, color: Color| {
            // The built-in font is 8 pixels tall; scale the renderer rather than the glyphs.
            let _ = canvas.set_scale(TEXT_SCALE, TEXT_SCALE);
            canvas.set_draw_color(color);
            let y = (HEIGHT - CHAR) / 2.0;
            let _ = canvas.draw_debug_text(label, (x / TEXT_SCALE, y / TEXT_SCALE));
            let _ = canvas.set_scale(1.0, 1.0);
        };

        for ((action, label), rect) in BUTTONS.iter().zip(&rects) {
            let hovered = self.pointer.is_some_and(|p| Self::contains(rect, p));
            let on = active(*action);
            let fill = match (action, on, hovered) {
                (Action::Disconnect, _, true) => DANGER,
                (_, true, _) => ACCENT,
                (_, false, true) => BUTTON_HOVER,
                _ => BUTTON,
            };
            canvas.set_draw_color(fill);
            let _ = canvas.fill_rect(*rect);
            let color = if on && *action != Action::Disconnect { Color::RGB(0, 0, 0) } else { TEXT };
            text(canvas, label, rect.x + PAD, color);
        }

        if let Some(last) = rects.last() {
            let x = last.x + last.w + PAD;
            if !stats.is_empty() && x + text_width(stats) <= bar.x + bar.w {
                text(canvas, stats, x, MUTED);
            }
        }
        canvas.set_blend_mode(BlendMode::None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_are_hit_where_they_are_drawn() {
        let stats = "60 fps 250 Mbps";
        let (bar, rects) = Toolbar::layout(1920.0, stats);
        assert!(bar.x > 0.0 && bar.x + bar.w < 1920.0);
        let toolbar = Toolbar::new();
        for (i, rect) in rects.iter().enumerate() {
            let centre = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
            assert_eq!(toolbar.click(centre, 1920.0, stats), Some(BUTTONS[i].0));
            assert!(toolbar.captures(centre, 1920.0, stats));
        }
        assert_eq!(toolbar.click((5.0, 500.0), 1920.0, stats), None);
        assert!(!toolbar.captures((960.0, 500.0), 1920.0, stats));
    }

    #[test]
    fn hides_after_the_hint_and_reveals_from_the_top_edge() {
        let mut toolbar = Toolbar::new();
        toolbar.pinned_until = Instant::now() - Duration::from_secs(1);
        toolbar.update(Some((960.0, 500.0)), 1920.0, "");
        assert!(!toolbar.visible());
        assert_eq!(toolbar.click((960.0, 20.0), 1920.0, ""), None);
        toolbar.update(Some((100.0, 1.0)), 1920.0, "");
        assert!(toolbar.visible());
        // Stays while the pointer is on the bar, goes away when it leaves.
        toolbar.update(Some((960.0, 30.0)), 1920.0, "");
        assert!(toolbar.visible());
        toolbar.update(Some((960.0, 300.0)), 1920.0, "");
        assert!(!toolbar.visible());
    }

    #[test]
    fn narrow_windows_drop_the_statistics() {
        let (wide, _) = Toolbar::layout(1920.0, "60 fps 250 Mbps");
        let (narrow, _) = Toolbar::layout(900.0, "60 fps 250 Mbps");
        assert!(narrow.w < wide.w && narrow.w <= 900.0);
    }
}
