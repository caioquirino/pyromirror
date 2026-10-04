//! The tray (notification area) icon of the background agent.
//!
//! - Windows: a native notification-area icon (tray-icon).
//! - Linux: a StatusNotifierItem over D-Bus (ksni). KDE shows these natively; GNOME needs the
//!   AppIndicator extension, without which there simply is no icon.

use std::sync::mpsc::Sender;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    Open,
    ToggleSharing,
    Quit,
}

/// What the icon's status dot shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indicator {
    /// Not sharing.
    Off,
    /// Sharing, nobody connected.
    On,
    /// Someone is connected.
    Connected,
    /// Something needs the person: a pairing request or a permission dialog.
    Attention,
}

/// Draws the icon: a dark rounded tile with an orange disc, and a status dot in the corner.
/// Returns `size * size` RGBA pixels.
pub fn icon_rgba(size: u32, indicator: Indicator) -> Vec<u8> {
    let s = size as f32;
    let dot = match indicator {
        Indicator::Off => [0x8b, 0x93, 0xa3],
        Indicator::On => [0x3e, 0xcf, 0x8e],
        Indicator::Connected => [0xff, 0xff, 0xff],
        Indicator::Attention => [0xf2, 0xc1, 0x4e],
    };
    // Signed distance to a shape -> coverage, for one pixel of anti-aliasing.
    let cover = |distance: f32| (0.5 - distance).clamp(0.0, 1.0);
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // Rounded tile.
            let r = s * 0.22;
            let (dx, dy) = ((px - s / 2.0).abs() - (s / 2.0 - r), (py - s / 2.0).abs() - (s / 2.0 - r));
            let tile = cover(dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - r);
            let disc = cover((px - s * 0.45).hypot(py - s * 0.45) - s * 0.27);
            let status = cover((px - s * 0.74).hypot(py - s * 0.74) - s * 0.19);
            // A dark ring keeps the dot readable on the disc.
            let ring = cover((px - s * 0.74).hypot(py - s * 0.74) - s * 0.26);

            let mut rgb = [0x12 as f32, 0x15 as f32, 0x1c as f32];
            let mut blend = |color: [u8; 3], a: f32| {
                for i in 0..3 {
                    rgb[i] += (color[i] as f32 - rgb[i]) * a;
                }
            };
            blend([0xff, 0x7a, 0x2f], disc);
            blend([0x12, 0x15, 0x1c], ring);
            blend(dot, status);

            let i = ((y * size + x) * 4) as usize;
            out[i..i + 3].copy_from_slice(&[rgb[0] as u8, rgb[1] as u8, rgb[2] as u8]);
            out[i + 3] = (tile * 255.0) as u8;
        }
    }
    out
}

pub struct Tray(imp::Tray);

impl Tray {
    /// Creates the icon. Fails where there is no tray to put it in.
    pub fn new(events: Sender<TrayEvent>) -> Result<Self, String> {
        imp::Tray::new(events).map(Self)
    }

    /// Reflects the current state. Cheap when nothing changed.
    pub fn update(&mut self, indicator: Indicator, tooltip: &str, sharing: bool) {
        self.0.update(indicator, tooltip, sharing);
    }

    /// Lets the icon process clicks for up to `wait`. Must be called regularly, from the thread
    /// that created the tray.
    pub fn pump(&mut self, wait: Duration) {
        self.0.pump(wait);
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use ksni::blocking::TrayMethods;

    use super::*;

    struct Model {
        events: Sender<TrayEvent>,
        indicator: Indicator,
        tooltip: String,
        sharing: bool,
    }

    impl ksni::Tray for Model {
        fn id(&self) -> String {
            "pyromirror".into()
        }

        fn title(&self) -> String {
            "PyroMirror".into()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            [22, 32, 48]
                .into_iter()
                .map(|size| {
                    // StatusNotifierItem wants ARGB in network byte order.
                    let mut data = icon_rgba(size, self.indicator);
                    for px in data.chunks_exact_mut(4) {
                        px.rotate_right(1);
                    }
                    ksni::Icon { width: size as i32, height: size as i32, data }
                })
                .collect()
        }

        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip { title: "PyroMirror".into(), description: self.tooltip.clone(), ..Default::default() }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = self.events.send(TrayEvent::Open);
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::StandardItem;
            let item = |label: &str, event: TrayEvent| -> ksni::MenuItem<Self> {
                StandardItem {
                    label: label.into(),
                    activate: Box::new(move |model: &mut Self| {
                        let _ = model.events.send(event);
                    }),
                    ..Default::default()
                }
                .into()
            };
            vec![
                item("Open PyroMirror", TrayEvent::Open),
                item(if self.sharing { "Stop sharing" } else { "Start sharing" }, TrayEvent::ToggleSharing),
                ksni::MenuItem::Separator,
                item("Quit", TrayEvent::Quit),
            ]
        }
    }

    pub struct Tray {
        handle: ksni::blocking::Handle<Model>,
        shown: (Indicator, String, bool),
    }

    impl Tray {
        pub fn new(events: Sender<TrayEvent>) -> Result<Self, String> {
            let shown = (Indicator::Off, String::new(), false);
            let model = Model { events, indicator: shown.0, tooltip: shown.1.clone(), sharing: shown.2 };
            let handle = model.spawn().map_err(|e| e.to_string())?;
            Ok(Self { handle, shown })
        }

        pub fn update(&mut self, indicator: Indicator, tooltip: &str, sharing: bool) {
            if self.shown == (indicator, tooltip.to_owned(), sharing) {
                return;
            }
            self.shown = (indicator, tooltip.to_owned(), sharing);
            let tooltip = tooltip.to_owned();
            self.handle.update(move |model| {
                model.indicator = indicator;
                model.tooltip = tooltip;
                model.sharing = sharing;
            });
        }

        pub fn pump(&mut self, wait: Duration) {
            // ksni runs on its own thread; there is nothing to pump.
            std::thread::sleep(wait);
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::time::Instant;

    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
    use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE};

    use super::*;

    pub struct Tray {
        icon: TrayIcon,
        toggle: MenuItem,
        events: Sender<TrayEvent>,
        shown: (Indicator, String, bool),
    }

    fn icon(indicator: Indicator) -> Option<Icon> {
        Icon::from_rgba(icon_rgba(32, indicator), 32, 32).ok()
    }

    impl Tray {
        pub fn new(events: Sender<TrayEvent>) -> Result<Self, String> {
            let menu = Menu::new();
            let open = MenuItem::with_id("open", "Open PyroMirror", true, None);
            let toggle = MenuItem::with_id("toggle", "Start sharing", true, None);
            let quit = MenuItem::with_id("quit", "Quit", true, None);
            menu.append_items(&[&open, &toggle, &PredefinedMenuItem::separator(), &quit]).map_err(|e| e.to_string())?;

            let mut builder = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("PyroMirror");
            if let Some(icon) = icon(Indicator::Off) {
                builder = builder.with_icon(icon);
            }
            let icon = builder.build().map_err(|e| e.to_string())?;
            Ok(Self { icon, toggle, events, shown: (Indicator::Off, String::new(), false) })
        }

        pub fn update(&mut self, indicator: Indicator, tooltip: &str, sharing: bool) {
            if self.shown == (indicator, tooltip.to_owned(), sharing) {
                return;
            }
            self.shown = (indicator, tooltip.to_owned(), sharing);
            let _ = self.icon.set_icon(icon(indicator));
            let _ = self.icon.set_tooltip(Some(format!("PyroMirror: {tooltip}")));
            self.toggle.set_text(if sharing { "Stop sharing" } else { "Start sharing" });
        }

        pub fn pump(&mut self, wait: Duration) {
            // The icon lives on a hidden window that only works while its messages are dispatched.
            let deadline = Instant::now() + wait;
            loop {
                unsafe {
                    let mut msg: MSG = std::mem::zeroed();
                    while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                while let Ok(event) = MenuEvent::receiver().try_recv() {
                    let event = match event.id.0.as_str() {
                        "open" => TrayEvent::Open,
                        "toggle" => TrayEvent::ToggleSharing,
                        _ => TrayEvent::Quit,
                    };
                    let _ = self.events.send(event);
                }
                while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                    if matches!(event, TrayIconEvent::DoubleClick { .. }) {
                        let _ = self.events.send(TrayEvent::Open);
                    }
                }
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_a_transparent_corner_an_orange_disc_and_a_status_dot() {
        let size = 32;
        let at = |pixels: &[u8], x: u32, y: u32| -> [u8; 4] {
            pixels[((y * size + x) * 4) as usize..][..4].try_into().unwrap()
        };
        let on = icon_rgba(size, Indicator::On);
        assert_eq!(on.len(), (size * size * 4) as usize);
        assert_eq!(at(&on, 0, 0)[3], 0, "rounded corner is transparent");
        assert_eq!(at(&on, 14, 14), [0xff, 0x7a, 0x2f, 255], "disc is accent orange");
        assert_eq!(at(&on, 24, 24), [0x3e, 0xcf, 0x8e, 255], "dot is green while sharing");
        let off = icon_rgba(size, Indicator::Off);
        assert_eq!(at(&off, 24, 24), [0x8b, 0x93, 0xa3, 255], "dot is grey while off");
    }
}
