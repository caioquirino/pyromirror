//! The session menu: a small handle that shows when the pointer touches the top edge of the
//! window, and a menu that drops from it when it is clicked.
//!
//! The handle is deliberately tiny and can be dragged along the edge, so that it stays out of the
//! way of whatever the remote desktop has up there (edge scrolling in a game, a browser's tabs).
//! Pointer motion over the handle still reaches the remote desktop; only clicks on it do not.
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
    RelativeMouse,
    Mute,
    Disconnect,
}

/// Menu rows: what they do, their label, and the keyboard shortcut (after "Ctrl+Alt+").
const ITEMS: [(Action, &str, &str); 6] = [
    (Action::Fullscreen, "Fullscreen", "F"),
    (Action::KeyboardGrab, "Grab keys", "G"),
    (Action::MouseLock, "Lock mouse", "L"),
    (Action::RelativeMouse, "Relative mouse", "M"),
    (Action::Mute, "Mute", ""),
    (Action::Disconnect, "Disconnect", "Q"),
];

const TEXT_SCALE: f32 = 2.0;
const CHAR: f32 = 8.0 * TEXT_SCALE;
const PAD: f32 = 12.0;
/// Touching this many pixels at the top of the window reveals the handle.
const REVEAL_ZONE: f32 = 4.0;
const HANDLE_W: f32 = 72.0;
const HANDLE_H: f32 = 20.0;
const ROW_H: f32 = 36.0;
const CHECK: f32 = 14.0;
/// The pointer may stray this far from the open menu before it closes.
const SLACK: f32 = 16.0;
/// A press on the handle that moves further than this is a drag, not a click.
const DRAG_THRESHOLD: f32 = 4.0;
const NOTICE_TIME: Duration = Duration::from_secs(4);

const BAR: Color = Color::RGBA(0x12, 0x15, 0x1c, 235);
const ROW_HOVER: Color = Color::RGB(0x2a, 0x31, 0x40);
const ACCENT: Color = Color::RGB(0xff, 0x7a, 0x2f);
const DANGER: Color = Color::RGB(0xc8, 0x3c, 0x3d);
const TEXT: Color = Color::RGB(0xe6, 0xe9, 0xef);
const MUTED: Color = Color::RGB(0x8b, 0x93, 0xa3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Hidden,
    Handle,
    Open,
}

struct Drag {
    press_x: f32,
    /// Where on the handle it was picked up, relative to its centre.
    offset: f32,
    moved: bool,
}

struct Layout {
    handle: FRect,
    panel: FRect,
    rows: [FRect; ITEMS.len()],
    shortcuts: bool,
    stats: Option<FRect>,
}

pub struct Toolbar {
    /// The handle is shown at least until this time (right after connecting).
    pinned_until: Instant,
    pointer: Option<(f32, f32)>,
    state: State,
    /// Centre of the handle as a fraction of the window width.
    position: f32,
    drag: Option<Drag>,
    notice: Option<(String, Instant)>,
}

fn text_width(text: &str) -> f32 {
    text.chars().count() as f32 * CHAR
}

fn contains(rect: &FRect, (x, y): (f32, f32)) -> bool {
    x >= rect.x && x < rect.x + rect.w && y >= rect.y && y < rect.y + rect.h
}

fn shortcut(key: &str) -> String {
    if key.is_empty() {
        String::new()
    } else {
        format!("Ctrl+Alt+{key}")
    }
}

impl Toolbar {
    /// `position` is where the handle sits along the top edge, from 0 (left) to 1 (right).
    pub fn new(position: f32) -> Self {
        let mut toolbar = Self {
            pinned_until: Instant::now() + NOTICE_TIME,
            pointer: None,
            state: State::Handle,
            position: if position.is_finite() { position.clamp(0.0, 1.0) } else { 0.5 },
            drag: None,
            notice: None,
        };
        toolbar.notify("Menu: move the pointer to the top edge");
        toolbar
    }

    pub fn position(&self) -> f32 {
        self.position
    }

    /// Shows a line of text at the top of the window for a few seconds.
    pub fn notify(&mut self, text: &str) {
        self.notice = Some((text.to_string(), Instant::now() + NOTICE_TIME));
    }

    fn layout(&self, window_width: f32, stats: &str) -> Layout {
        let handle_left = (self.position * window_width - HANDLE_W / 2.0).clamp(0.0, (window_width - HANDLE_W).max(0.0));
        let handle = FRect::new(handle_left, 0.0, HANDLE_W, HANDLE_H);

        let label_width = ITEMS.iter().map(|(_, label, _)| text_width(label)).fold(0.0, f32::max);
        let shortcut_width = ITEMS.iter().map(|(_, _, key)| text_width(&shortcut(key))).fold(0.0, f32::max);
        let narrow = PAD + CHECK + PAD + label_width + PAD;
        let wide = narrow + PAD + shortcut_width;
        // Drop the shortcuts rather than overflow a narrow window.
        let shortcuts = wide <= window_width;
        let width = if shortcuts { wide } else { narrow };

        let left = (handle.x + HANDLE_W / 2.0 - width / 2.0).clamp(0.0, (window_width - width).max(0.0));
        let mut y = HANDLE_H;
        let rows = ITEMS.map(|_| {
            let rect = FRect::new(left, y, width, ROW_H);
            y += ROW_H;
            rect
        });
        let stats = (!stats.is_empty() && text_width(stats) + 2.0 * PAD <= width).then(|| {
            let rect = FRect::new(left, y, width, ROW_H);
            y += ROW_H;
            rect
        });
        Layout { handle, panel: FRect::new(left, HANDLE_H, width, y - HANDLE_H), rows, shortcuts, stats }
    }

    /// Recomputes what is shown, and moves the handle while it is dragged. Returns true if the
    /// window needs to be redrawn.
    pub fn update(&mut self, pointer: Option<(f32, f32)>, window_width: f32, stats: &str) -> bool {
        let mut changed = false;

        if let (Some(drag), Some((x, _))) = (&mut self.drag, pointer) {
            drag.moved |= (x - drag.press_x).abs() > DRAG_THRESHOLD;
            if drag.moved && window_width > 0.0 {
                let position = ((x - drag.offset) / window_width).clamp(0.0, 1.0);
                changed |= position != self.position;
                self.position = position;
            }
        }
        if pointer.is_none() {
            // Relative mouse mode, or the pointer left the window.
            self.drag = None;
        }

        let layout = self.layout(window_width, stats);
        let on_handle = pointer.is_some_and(|p| contains(&layout.handle, p));
        let state = if self.drag.is_some() {
            self.state
        } else if self.state == State::Open && pointer.is_some_and(|p| on_handle || Self::near(&layout.panel, p)) {
            State::Open
        } else if on_handle && self.state != State::Hidden
            || pointer.is_some_and(|p| p.1 < REVEAL_ZONE)
            || Instant::now() < self.pinned_until
        {
            State::Handle
        } else {
            State::Hidden
        };

        changed |= state != self.state || (state != State::Hidden && pointer != self.pointer);
        if self.notice.as_ref().is_some_and(|(_, until)| Instant::now() >= *until) {
            self.notice = None;
            changed = true;
        }
        self.state = state;
        self.pointer = pointer;
        changed
    }

    fn near(rect: &FRect, (x, y): (f32, f32)) -> bool {
        x >= rect.x - SLACK && x < rect.x + rect.w + SLACK && y >= rect.y - SLACK && y < rect.y + rect.h + SLACK
    }

    /// Whether a click or wheel turn at this position belongs to the menu rather than the remote
    /// desktop.
    pub fn captures(&self, pointer: (f32, f32), window_width: f32, stats: &str) -> bool {
        if self.drag.is_some() {
            return true;
        }
        let layout = self.layout(window_width, stats);
        match self.state {
            State::Hidden => false,
            State::Handle => contains(&layout.handle, pointer),
            State::Open => contains(&layout.handle, pointer) || contains(&layout.panel, pointer),
        }
    }

    /// Whether pointer motion at this position should be kept from the remote desktop. Motion over
    /// the bare handle is not: the remote side may be using that edge too.
    pub fn captures_motion(&self, pointer: (f32, f32), window_width: f32, stats: &str) -> bool {
        self.drag.as_ref().is_some_and(|d| d.moved)
            || (self.state == State::Open && contains(&self.layout(window_width, stats).panel, pointer))
    }

    /// A button press that `captures` claimed. Returns the menu row it chose, if any.
    pub fn press(&mut self, pointer: (f32, f32), window_width: f32, stats: &str) -> Option<Action> {
        let layout = self.layout(window_width, stats);
        if contains(&layout.handle, pointer) {
            let centre = layout.handle.x + HANDLE_W / 2.0;
            self.drag = Some(Drag { press_x: pointer.0, offset: pointer.0 - centre, moved: false });
            return None;
        }
        if self.state != State::Open {
            return None;
        }
        layout.rows.iter().position(|r| contains(r, pointer)).map(|i| ITEMS[i].0)
    }

    /// The button was released. A press on the handle that did not drag it opens or closes the
    /// menu; returns true if the handle was moved instead.
    pub fn release(&mut self) -> bool {
        match self.drag.take() {
            Some(drag) if drag.moved => true,
            Some(_) => {
                self.state = if self.state == State::Open { State::Handle } else { State::Open };
                false
            }
            None => false,
        }
    }

    /// Draws the menu. `active` says which toggles are on.
    pub fn draw(&self, canvas: &mut WindowCanvas, window_width: f32, stats: &str, active: impl Fn(Action) -> bool) {
        if self.state == State::Hidden && self.notice.is_none() {
            return;
        }
        let layout = self.layout(window_width, stats);
        canvas.set_blend_mode(BlendMode::Blend);

        let text = |canvas: &mut WindowCanvas, label: &str, x: f32, row: &FRect, color: Color| {
            // The built-in font is 8 pixels tall; scale the renderer rather than the glyphs.
            let _ = canvas.set_scale(TEXT_SCALE, TEXT_SCALE);
            canvas.set_draw_color(color);
            let y = row.y + (row.h - CHAR) / 2.0;
            let _ = canvas.draw_debug_text(label, (x / TEXT_SCALE, y / TEXT_SCALE));
            let _ = canvas.set_scale(1.0, 1.0);
        };

        if let Some((notice, _)) = &self.notice {
            let width = (text_width(notice) + 2.0 * PAD).min(window_width);
            // Below the handle and clear of the open menu's usual place only when it fits beside
            // it; a few seconds of overlap is acceptable otherwise.
            let rect = FRect::new(((window_width - width) / 2.0).max(0.0), HANDLE_H + 8.0, width, ROW_H);
            if self.state != State::Open {
                canvas.set_draw_color(BAR);
                let _ = canvas.fill_rect(rect);
                text(canvas, notice, rect.x + PAD, &rect, TEXT);
            }
        }

        if self.state != State::Hidden {
            let handle = layout.handle;
            let hot = self.drag.is_some() || self.pointer.is_some_and(|p| contains(&handle, p)) || self.state == State::Open;
            canvas.set_draw_color(BAR);
            let _ = canvas.fill_rect(handle);
            canvas.set_draw_color(if hot { ACCENT } else { MUTED });
            // Three grip bars, and a line along the bottom.
            for i in 0..3 {
                let x = handle.x + HANDLE_W / 2.0 - 13.0 + i as f32 * 10.0;
                let _ = canvas.fill_rect(FRect::new(x, 6.0, 6.0, 6.0));
            }
            let _ = canvas.fill_rect(FRect::new(handle.x, HANDLE_H - 2.0, HANDLE_W, 2.0));
        }

        if self.state == State::Open {
            canvas.set_draw_color(BAR);
            let _ = canvas.fill_rect(layout.panel);

            for ((action, label, key), row) in ITEMS.iter().zip(&layout.rows) {
                let hovered = self.pointer.is_some_and(|p| contains(row, p));
                let danger = *action == Action::Disconnect;
                if hovered {
                    canvas.set_draw_color(if danger { DANGER } else { ROW_HOVER });
                    let _ = canvas.fill_rect(*row);
                }
                if !danger {
                    let check = FRect::new(row.x + PAD, row.y + (ROW_H - CHECK) / 2.0, CHECK, CHECK);
                    if active(*action) {
                        canvas.set_draw_color(ACCENT);
                        let _ = canvas.fill_rect(check);
                    } else {
                        canvas.set_draw_color(MUTED);
                        let _ = canvas.draw_rect(check);
                    }
                }
                let x = row.x + PAD + CHECK + PAD;
                text(canvas, label, x, row, TEXT);
                if layout.shortcuts {
                    let keys = shortcut(key);
                    let color = if danger && hovered { TEXT } else { MUTED };
                    text(canvas, &keys, row.x + row.w - PAD - text_width(&keys), row, color);
                }
            }
            if let Some(row) = &layout.stats {
                text(canvas, stats, row.x + PAD, row, MUTED);
            }
        }
        canvas.set_blend_mode(BlendMode::None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f32 = 1920.0;

    /// A toolbar past its start-up hint, with the pointer somewhere harmless.
    fn settled(position: f32) -> Toolbar {
        let mut toolbar = Toolbar::new(position);
        toolbar.pinned_until = Instant::now() - Duration::from_secs(1);
        toolbar.update(Some((960.0, 500.0)), W, "");
        toolbar
    }

    fn click(toolbar: &mut Toolbar, p: (f32, f32)) -> Option<Action> {
        toolbar.update(Some(p), W, "");
        assert!(toolbar.captures(p, W, ""));
        let action = toolbar.press(p, W, "");
        toolbar.release();
        action
    }

    #[test]
    fn handle_shows_only_at_the_top_edge() {
        let mut toolbar = settled(0.5);
        assert_eq!(toolbar.state, State::Hidden);
        assert!(!toolbar.captures((960.0, 10.0), W, ""));
        // Hovering where the handle would be, without touching the edge, does not reveal it.
        toolbar.update(Some((960.0, 10.0)), W, "");
        assert_eq!(toolbar.state, State::Hidden);
        toolbar.update(Some((100.0, 1.0)), W, "");
        assert_eq!(toolbar.state, State::Handle);
        // Only the handle itself takes clicks, and it never takes motion.
        assert!(!toolbar.captures((100.0, 1.0), W, ""));
        assert!(toolbar.captures((960.0, 1.0), W, ""));
        assert!(!toolbar.captures_motion((960.0, 1.0), W, ""));
        // Stays while the pointer is on it, goes away when it leaves.
        toolbar.update(Some((960.0, 15.0)), W, "");
        assert_eq!(toolbar.state, State::Handle);
        toolbar.update(Some((960.0, 300.0)), W, "");
        assert_eq!(toolbar.state, State::Hidden);
    }

    #[test]
    fn clicking_the_handle_opens_the_menu_and_rows_are_hit_where_drawn() {
        let mut toolbar = settled(0.5);
        assert_eq!(click(&mut toolbar, (960.0, 1.0)), None);
        assert_eq!(toolbar.state, State::Open);

        let layout = toolbar.layout(W, "");
        for (i, row) in layout.rows.iter().enumerate() {
            let centre = (row.x + row.w / 2.0, row.y + row.h / 2.0);
            assert!(toolbar.captures_motion(centre, W, ""));
            assert_eq!(click(&mut toolbar, centre), Some(ITEMS[i].0));
            // Choosing a row leaves the menu open, so several can be switched in one go.
            assert_eq!(toolbar.state, State::Open);
        }

        toolbar.update(Some((960.0, 800.0)), W, "");
        assert_eq!(toolbar.state, State::Hidden);
        assert!(!toolbar.captures((960.0, 60.0), W, ""));
    }

    #[test]
    fn dragging_moves_the_handle_without_opening_the_menu() {
        let mut toolbar = settled(0.5);
        toolbar.update(Some((970.0, 1.0)), W, "");
        assert_eq!(toolbar.press((970.0, 1.0), W, ""), None);
        // The pointer may wander off the edge while dragging.
        toolbar.update(Some((370.0, 200.0)), W, "");
        assert!(toolbar.captures_motion((370.0, 200.0), W, ""));
        assert!(toolbar.release());
        assert_eq!(toolbar.state, State::Handle);
        let handle = toolbar.layout(W, "").handle;
        assert!((handle.x + HANDLE_W / 2.0 - 360.0).abs() < 0.5);
        assert!((toolbar.position() - 360.0 / W).abs() < 0.001);
    }

    #[test]
    fn handle_and_menu_stay_inside_the_window() {
        for position in [0.0, 1.0] {
            let toolbar = Toolbar::new(position);
            let layout = toolbar.layout(W, "60 fps 250 Mbps");
            assert!(layout.handle.x >= 0.0 && layout.handle.x + layout.handle.w <= W);
            assert!(layout.panel.x >= 0.0 && layout.panel.x + layout.panel.w <= W);
            assert!(layout.shortcuts && layout.stats.is_some());
        }
        let narrow = Toolbar::new(0.5).layout(300.0, "");
        assert!(!narrow.shortcuts && narrow.panel.w <= 300.0);
    }

    #[test]
    fn relative_mode_hides_everything() {
        let mut toolbar = settled(0.5);
        click(&mut toolbar, (960.0, 1.0));
        toolbar.update(None, W, "");
        assert_eq!(toolbar.state, State::Hidden);
    }
}
