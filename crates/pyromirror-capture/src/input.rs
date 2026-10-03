//! Injecting the client's mouse and keyboard into the captured desktop.
//!
//! - Windows: `SendInput`.
//! - Linux: the RemoteDesktop portal session that also provides the screen cast.

use std::collections::HashSet;
use std::sync::Mutex;

use crate::keymap;

/// Mouse buttons use SDL's numbering: 1 left, 2 middle, 3 right, 4 back, 5 forward.
pub struct InputInjector {
    backend: Backend,
    /// What is currently held down, so it can be released when the client goes away.
    held_keys: Mutex<HashSet<u16>>,
    held_buttons: Mutex<HashSet<u8>>,
}

impl InputInjector {
    pub(crate) fn new(backend: Backend) -> Self {
        Self { backend, held_keys: Mutex::default(), held_buttons: Mutex::default() }
    }

    /// Moves the pointer to a position on the captured monitor; `x` and `y` are 0..1.
    pub fn pointer_absolute(&self, x: f64, y: f64) {
        self.backend.pointer_absolute(x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
    }

    pub fn pointer_relative(&self, dx: f64, dy: f64) {
        self.backend.pointer_relative(dx, dy);
    }

    pub fn button(&self, button: u8, down: bool) {
        let mut held = self.held_buttons.lock().unwrap();
        if down { held.insert(button) } else { held.remove(&button) };
        self.backend.button(button, down);
    }

    /// Scrolls; 120 units are one wheel notch, positive `dy` is away from the user, positive
    /// `dx` is to the right.
    pub fn wheel(&self, dx: i32, dy: i32) {
        self.backend.wheel(dx, dy);
    }

    /// Presses or releases a key identified by its USB HID usage code. Unknown keys are ignored.
    pub fn key(&self, hid_usage: u16, down: bool) {
        let Some(evdev) = keymap::hid_to_evdev(hid_usage) else {
            log::debug!("No mapping for HID key usage {}", hid_usage);
            return;
        };
        let mut held = self.held_keys.lock().unwrap();
        if down { held.insert(evdev) } else { held.remove(&evdev) };
        self.backend.key(evdev, down);
    }

    /// Releases everything the client left pressed.
    pub fn release_all(&self) {
        for evdev in self.held_keys.lock().unwrap().drain() {
            self.backend.key(evdev, false);
        }
        for button in self.held_buttons.lock().unwrap().drain() {
            self.backend.button(button, false);
        }
    }
}

#[cfg(windows)]
pub(crate) use windows_backend::Backend;

#[cfg(windows)]
mod windows_backend {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    use crate::keymap;

    /// Bounds of the captured monitor in virtual-desktop pixels.
    pub(crate) struct Backend {
        pub x: i32,
        pub y: i32,
        pub width: u32,
        pub height: u32,
    }

    fn send(input: INPUT) {
        // Fails (returns 0) while a UAC prompt or another elevated window has focus; there is
        // nothing useful to do about that per event.
        unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
    }

    fn mouse(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) {
        send(INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT { dx, dy, mouseData: data as _, dwFlags: flags, time: 0, dwExtraInfo: 0 },
            },
        });
    }

    impl Backend {
        pub fn pointer_absolute(&self, x: f64, y: f64) {
            // Absolute coordinates are 0..65535 across the whole virtual desktop.
            let (vx, vy, vw, vh) = unsafe {
                (
                    GetSystemMetrics(SM_XVIRTUALSCREEN),
                    GetSystemMetrics(SM_YVIRTUALSCREEN),
                    GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
                    GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
                )
            };
            let px = self.x as f64 + x * (self.width.max(1) - 1) as f64;
            let py = self.y as f64 + y * (self.height.max(1) - 1) as f64;
            let nx = ((px - vx as f64) * 65535.0 / (vw - 1) as f64).round() as i32;
            let ny = ((py - vy as f64) * 65535.0 / (vh - 1) as f64).round() as i32;
            mouse(nx, ny, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK);
        }

        pub fn pointer_relative(&self, dx: f64, dy: f64) {
            mouse(dx.round() as i32, dy.round() as i32, 0, MOUSEEVENTF_MOVE);
        }

        pub fn button(&self, button: u8, down: bool) {
            let (flags, data) = match (button, down) {
                (1, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                (1, false) => (MOUSEEVENTF_LEFTUP, 0),
                (2, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                (2, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                (3, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                (3, false) => (MOUSEEVENTF_RIGHTUP, 0),
                (4, true) => (MOUSEEVENTF_XDOWN, 1),
                (4, false) => (MOUSEEVENTF_XUP, 1),
                (5, true) => (MOUSEEVENTF_XDOWN, 2),
                (5, false) => (MOUSEEVENTF_XUP, 2),
                _ => return,
            };
            mouse(0, 0, data, flags);
        }

        pub fn wheel(&self, dx: i32, dy: i32) {
            if dy != 0 {
                mouse(0, 0, dy, MOUSEEVENTF_WHEEL);
            }
            if dx != 0 {
                mouse(0, 0, dx, MOUSEEVENTF_HWHEEL);
            }
        }

        pub fn key(&self, evdev: u16, down: bool) {
            let Some((scancode, extended)) = keymap::evdev_to_set1(evdev) else { return };
            let mut flags = KEYEVENTF_SCANCODE;
            if extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            if !down {
                flags |= KEYEVENTF_KEYUP;
            }
            send(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT { wVk: 0, wScan: scancode, dwFlags: flags, time: 0, dwExtraInfo: 0 },
                },
            });
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) use linux_backend::Backend;

#[cfg(target_os = "linux")]
mod linux_backend {
    use std::sync::Arc;

    use ashpd::desktop::remote_desktop::{Axis, KeyState, NotifyPointerAxisOptions};

    use crate::portal::RemoteControl;

    pub(crate) struct Backend {
        pub remote: Arc<RemoteControl>,
    }

    fn state(down: bool) -> KeyState {
        if down { KeyState::Pressed } else { KeyState::Released }
    }

    impl Backend {
        fn run<F: std::future::Future<Output = ashpd::Result<()>>>(&self, what: &str, call: F) {
            if let Err(e) = self.remote.runtime.block_on(call) {
                // The first failure is worth seeing; after that it would flood the log.
                static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                if WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    log::debug!("Portal {} failed: {}", what, e);
                } else {
                    log::warn!("Portal {} failed: {} (further input errors are logged at debug level)", what, e);
                }
            }
        }

        pub fn pointer_absolute(&self, x: f64, y: f64) {
            let r = &self.remote;
            // Stream coordinates are in the stream's logical size.
            let (x, y) = (x * (r.width - 1.0).max(0.0), y * (r.height - 1.0).max(0.0));
            self.run(
                "pointer motion",
                r.proxy.notify_pointer_motion_absolute(&r.session, r.node, x, y, Default::default()),
            );
        }

        pub fn pointer_relative(&self, dx: f64, dy: f64) {
            let r = &self.remote;
            self.run("pointer motion", r.proxy.notify_pointer_motion(&r.session, dx, dy, Default::default()));
        }

        pub fn button(&self, button: u8, down: bool) {
            // evdev BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, BTN_SIDE, BTN_EXTRA
            let code = match button {
                1 => 0x110,
                2 => 0x112,
                3 => 0x111,
                4 => 0x113,
                5 => 0x114,
                _ => return,
            };
            let r = &self.remote;
            self.run("button", r.proxy.notify_pointer_button(&r.session, code, state(down), Default::default()));
        }

        pub fn wheel(&self, dx: i32, dy: i32) {
            let r = &self.remote;
            // Wayland scrolls down / right for positive values, the opposite of our dy.
            let (dx, dy) = (dx, -dy);
            // Whole notches go out as wheel clicks, which every application understands. Only
            // what is left over (touchpads, high-resolution wheels) is sent as smooth scrolling,
            // at roughly 15 units per notch.
            for (axis, steps) in [(Axis::Vertical, dy / 120), (Axis::Horizontal, dx / 120)] {
                if steps != 0 {
                    self.run(
                        "scroll",
                        r.proxy.notify_pointer_axis_discrete(&r.session, axis, steps, Default::default()),
                    );
                }
            }
            let (rx, ry) = (dx % 120, dy % 120);
            if rx != 0 || ry != 0 {
                let (px, py) = (rx as f64 / 120.0 * 15.0, ry as f64 / 120.0 * 15.0);
                self.run(
                    "scroll",
                    r.proxy.notify_pointer_axis(&r.session, px, py, NotifyPointerAxisOptions::default()),
                );
            }
        }

        pub fn key(&self, evdev: u16, down: bool) {
            let r = &self.remote;
            self.run(
                "key",
                r.proxy.notify_keyboard_keycode(&r.session, evdev as i32, state(down), Default::default()),
            );
        }
    }
}
