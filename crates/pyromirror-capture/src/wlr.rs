//! Input injection for wlroots desktops (Sway, Hyprland, labwc, ...).
//!
//! Their portal shares the screen but has no RemoteDesktop interface, so the mouse and keyboard
//! go through the compositor's virtual-pointer and virtual-keyboard Wayland protocols instead.
//! Unlike the portal, these need no permission from the person at the desk.

use std::io::Write;
use std::os::fd::AsFd;
use std::time::Instant;

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_keyboard::KeymapFormat;
use wayland_client::protocol::wl_pointer::{Axis, AxisSource, ButtonState};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

/// Absolute positions are sent as a fraction of this extent.
const EXTENT: u32 = 0x1_0000;
/// What one wheel notch is worth in Wayland's scroll units.
const NOTCH: f64 = 15.0;

struct State;

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ignore ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ignore ZwlrVirtualPointerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardV1);

pub(crate) struct WlrInput {
    connection: Connection,
    pointer: ZwlrVirtualPointerV1,
    keyboard: ZwpVirtualKeyboardV1,
    started: Instant,
}

/// The keyboard layout the virtual keyboard types with, as an xkb keymap the compositor compiles.
/// Keys arrive as physical keys, so this decides what they produce, like a real keyboard's layout
/// setting would.
fn keymap() -> String {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    // Only the first layout of a list such as "us,de".
    let layout = var("XKB_DEFAULT_LAYOUT").and_then(|l| l.split(',').next().map(str::to_string));
    let layout = layout.unwrap_or_else(|| "us".to_string());
    let variant = var("XKB_DEFAULT_VARIANT").and_then(|v| v.split(',').next().map(str::to_string));
    let symbols = match variant.filter(|v| !v.is_empty()) {
        Some(variant) => format!("{layout}({variant})"),
        None => layout,
    };
    format!(
        "xkb_keymap {{\n\
         xkb_keycodes {{ include \"evdev+aliases(qwerty)\" }};\n\
         xkb_types {{ include \"complete\" }};\n\
         xkb_compat {{ include \"complete\" }};\n\
         xkb_symbols {{ include \"pc+{symbols}+inet(evdev)\" }};\n\
         }};\n"
    )
}

impl WlrInput {
    /// Connects to the compositor of this session. Fails on desktops without the two protocols
    /// (GNOME and KDE, which have the portal instead) and outside Wayland.
    pub fn connect() -> Result<Self, String> {
        let connection = Connection::connect_to_env().map_err(|e| format!("no Wayland compositor: {e}"))?;
        let (globals, mut queue) = registry_queue_init::<State>(&connection).map_err(|e| e.to_string())?;
        let handle = queue.handle();

        let pointers: ZwlrVirtualPointerManagerV1 =
            globals.bind(&handle, 1..=2, ()).map_err(|e| format!("no virtual pointer protocol: {e}"))?;
        let keyboards: ZwpVirtualKeyboardManagerV1 =
            globals.bind(&handle, 1..=1, ()).map_err(|e| format!("no virtual keyboard protocol: {e}"))?;
        let seat: WlSeat = globals.bind(&handle, 1..=1, ()).map_err(|e| format!("no seat: {e}"))?;

        let pointer = pointers.create_virtual_pointer(Some(&seat), &handle, ());
        let keyboard = keyboards.create_virtual_keyboard(&seat, &handle, ());

        // The keymap travels as a file descriptor; an unlinked temporary file is enough.
        let mut text = keymap().into_bytes();
        text.push(0);
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let path = dir.join(format!("pyromirror-keymap-{}", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| format!("could not create {}: {e}", path.display()))?;
        let _ = std::fs::remove_file(&path);
        file.write_all(&text).map_err(|e| e.to_string())?;
        keyboard.keymap(KeymapFormat::XkbV1.into(), file.as_fd(), text.len() as u32);

        // A refused keymap or creation shows up as a protocol error on this round trip.
        queue.roundtrip(&mut State).map_err(|e| format!("the compositor refused the virtual devices: {e}"))?;

        // Nothing the compositor says later matters, but it has to be read.
        std::thread::Builder::new()
            .name("wayland-input".into())
            .spawn(move || while queue.blocking_dispatch(&mut State).is_ok() {})
            .map_err(|e| e.to_string())?;

        Ok(Self { connection, pointer, keyboard, started: Instant::now() })
    }

    fn time(&self) -> u32 {
        self.started.elapsed().as_millis() as u32
    }

    fn flush(&self) {
        if let Err(e) = self.connection.flush() {
            static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                log::warn!("Lost the compositor connection used for input: {}", e);
            }
        }
    }

    pub fn pointer_absolute(&self, x: f64, y: f64) {
        let scale = (EXTENT - 1) as f64;
        self.pointer.motion_absolute(self.time(), (x * scale) as u32, (y * scale) as u32, EXTENT, EXTENT);
        self.pointer.frame();
        self.flush();
    }

    pub fn pointer_relative(&self, dx: f64, dy: f64) {
        self.pointer.motion(self.time(), dx, dy);
        self.pointer.frame();
        self.flush();
    }

    /// `code` is an evdev button code.
    pub fn button(&self, code: u32, down: bool) {
        let state = if down { ButtonState::Pressed } else { ButtonState::Released };
        self.pointer.button(self.time(), code, state);
        self.pointer.frame();
        self.flush();
    }

    /// 120 units are one notch; positive is down / right, as Wayland has it.
    pub fn wheel(&self, dx: i32, dy: i32) {
        self.pointer.axis_source(AxisSource::Wheel);
        for (axis, units) in [(Axis::VerticalScroll, dy), (Axis::HorizontalScroll, dx)] {
            if units != 0 {
                let value = units as f64 / 120.0 * NOTCH;
                self.pointer.axis_discrete(self.time(), axis, value, units / 120);
            }
        }
        self.pointer.frame();
        self.flush();
    }

    pub fn key(&self, evdev: u16, down: bool) {
        self.keyboard.key(self.time(), evdev as u32, if down { 1 } else { 0 });
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keymap_names_the_layout() {
        // Whatever the environment says, the result must be one complete keymap.
        let text = keymap();
        assert!(text.starts_with("xkb_keymap {") && text.contains("xkb_symbols { include \"pc+"));
    }

    /// Needs a wlroots session with a terminal under the pointer at 30%, 30% of the screen (the
    /// demo container): clicks it and types a command that leaves a file behind.
    #[test]
    #[ignore]
    fn clicks_and_types_in_a_live_session() {
        let input = WlrInput::connect().expect("connect");
        let pause = || std::thread::sleep(std::time::Duration::from_millis(60));
        input.pointer_absolute(0.3, 0.3);
        pause();
        input.button(0x110, true);
        input.button(0x110, false);
        pause();
        // evdev codes for: t o u c h space / t m p / t y p e d Enter
        for key in [20, 24, 22, 46, 35, 57, 53, 20, 50, 25, 53, 20, 21, 25, 18, 32, 28] {
            input.key(key, true);
            input.key(key, false);
            pause();
        }
        input.wheel(0, 120);
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}
