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

/// Draws the PyroMirror icon (the same screen-and-flame as the window, shortcut and package
/// icons) with the sharing state worked in:
///
/// - off: the flame is grey
/// - sharing: the flame burns
/// - someone connected: burning, with a green badge
/// - needs attention: burning, with a yellow badge
///
/// Returns `size * size` RGBA pixels. The shapes are those of `assets/icon.png` (a test checks it),
/// drawn in a 256x256 coordinate space.
pub fn icon_rgba(size: u32, indicator: Indicator) -> Vec<u8> {
    const BG: [u8; 3] = [0x12, 0x15, 0x1c];
    const SCREEN: [u8; 3] = [0x1b, 0x20, 0x2b];
    const BORDER: [u8; 3] = [0x2e, 0x35, 0x45];
    let (flame_outer, flame_inner) = match indicator {
        Indicator::Off => ([0x6b, 0x73, 0x82], [0x9a, 0xa1, 0xae]),
        _ => ([0xff, 0x7a, 0x2f], [0xff, 0xd2, 0x7a]),
    };
    let badge = match indicator {
        Indicator::Connected => Some([0x3e, 0xcf, 0x8e]),
        Indicator::Attention => Some([0xf2, 0xc1, 0x4e]),
        Indicator::Off | Indicator::On => None,
    };

    let rounded_rect = |x: f32, y: f32, x0: f32, y0: f32, w: f32, h: f32, r: f32| {
        let cx = x.clamp(x0 + r, x0 + w - r);
        let cy = y.clamp(y0 + r, y0 + h - r);
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    };
    // A disc with a pointed top.
    let flame = |x: f32, y: f32, cy: f32, r: f32, tip: f32| {
        let dx = x - 128.0;
        dx * dx + (y - cy).powi(2) <= r * r || (y >= tip && y <= cy && dx.abs() <= r * ((y - tip) / (cy - tip)).powf(0.8))
    };
    let disc = |x: f32, y: f32, cx: f32, cy: f32, r: f32| (x - cx).powi(2) + (y - cy).powi(2) <= r * r;

    // Supersampled: every pixel averages SS x SS samples.
    const SS: u32 = 4;
    let scale = 256.0 / (size * SS) as f32;
    let mut out = vec![0u8; (size * size * 4) as usize];
    for py in 0..size {
        for px in 0..size {
            let (mut sum, mut covered) = ([0u32; 3], 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = ((px * SS + sx) as f32 + 0.5) * scale;
                    let y = ((py * SS + sy) as f32 + 0.5) * scale;
                    if !rounded_rect(x, y, 0.0, 0.0, 256.0, 256.0, 56.0) {
                        continue;
                    }
                    let mut color = BG;
                    if rounded_rect(x, y, 104.0, 184.0, 48.0, 12.0, 4.0) || rounded_rect(x, y, 84.0, 196.0, 88.0, 12.0, 6.0) {
                        color = BORDER;
                    }
                    if rounded_rect(x, y, 37.0, 53.0, 182.0, 122.0, 17.0) {
                        color = BORDER;
                    }
                    if rounded_rect(x, y, 43.0, 59.0, 170.0, 110.0, 11.0) {
                        color = SCREEN;
                    }
                    if flame(x, y, 130.0, 30.0, 72.0) {
                        color = flame_outer;
                    }
                    if flame(x, y, 141.0, 12.0, 116.0) {
                        color = flame_inner;
                    }
                    if let Some(badge) = badge {
                        // Big enough to read at 16 pixels, with a dark ring to set it off.
                        if disc(x, y, 196.0, 196.0, 58.0) {
                            color = BG;
                        }
                        if disc(x, y, 196.0, 196.0, 44.0) {
                            color = badge;
                        }
                    }
                    for i in 0..3 {
                        sum[i] += color[i] as u32;
                    }
                    covered += 1;
                }
            }
            if covered > 0 {
                let i = ((py * size + px) * 4) as usize;
                for c in 0..3 {
                    out[i + c] = (sum[c] / covered) as u8;
                }
                out[i + 3] = (covered * 255 / (SS * SS)) as u8;
            }
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
    fn icon_is_the_app_icon_with_the_state_worked_in() {
        let size = 64;
        let at = |pixels: &[u8], x: u32, y: u32| -> [u8; 4] {
            pixels[((y * size + x) * 4) as usize..][..4].try_into().unwrap()
        };
        let on = icon_rgba(size, Indicator::On);
        assert_eq!(on.len(), (size * size * 4) as usize);
        assert_eq!(at(&on, 0, 0)[3], 0, "rounded corner is transparent");
        assert_eq!(at(&on, 32, 28), [0xff, 0x7a, 0x2f, 255], "the flame burns while sharing");
        assert_eq!(at(&on, 20, 20), [0x1b, 0x20, 0x2b, 255], "screen");

        let off = icon_rgba(size, Indicator::Off);
        assert_eq!(at(&off, 32, 28), [0x6b, 0x73, 0x82, 255], "the flame is grey while off");

        // Same icon as "on", plus a badge in the corner.
        let connected = icon_rgba(size, Indicator::Connected);
        assert_eq!(at(&connected, 32, 28), at(&on, 32, 28));
        assert_eq!(at(&connected, 49, 49), [0x3e, 0xcf, 0x8e, 255], "green badge when someone is connected");
        assert_eq!(at(&icon_rgba(size, Indicator::Attention), 49, 49), [0xf2, 0xc1, 0x4e, 255]);
        assert_ne!(at(&on, 49, 49), at(&connected, 49, 49));
    }

    /// The window, shortcut and installer use assets/icon.png; the tray draws the same picture.
    #[test]
    fn tray_icon_matches_the_bundled_app_icon() {
        let png = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).unwrap();
        assert_eq!((png.width, png.height), (256, 256));
        let drawn = icon_rgba(256, Indicator::On);
        let worst = png.rgba.iter().zip(&drawn).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        assert!(worst <= 2, "tray icon differs from the app icon by up to {worst}");
    }
}
