//! The tray (notification area) icon of the background agent.
//!
//! - Windows: a native notification-area icon (tray-icon).
//! - Linux: a StatusNotifierItem over D-Bus (ksni). KDE shows these natively; GNOME needs the
//!   AppIndicator extension, without which there simply is no icon.

use pyromirror_proto::control::{Action, Side, Toggles};
use std::sync::mpsc::Sender;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    Open,
    ToggleSharing,
    /// Something from the menu of a session: the one this computer is viewing, or (`Side::Host`)
    /// the one of the computer that is viewing this one.
    Session(Side, Action),
    Quit,
}

/// A session with another computer, for its menu in the tray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The other computer.
    pub name: String,
    pub toggles: Toggles,
}

/// The sessions this computer is part of: at most one it views, and one in which it is viewed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sessions {
    pub viewing: Option<Session>,
    pub serving: Option<Session>,
}

impl Sessions {
    #[cfg(target_os = "linux")]
    fn each(&self) -> impl Iterator<Item = (Side, &Session)> {
        let viewing = self.viewing.as_ref().map(|s| (Side::Viewer, s));
        let serving = self.serving.as_ref().map(|s| (Side::Host, s));
        viewing.into_iter().chain(serving)
    }
}

impl Session {
    fn title(&self) -> String {
        format!("{} session", self.name)
    }
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

/// Draws the PyroMirror icon (a monitor with a flame on its screen, on a transparent
/// background so the shapes use the whole canvas) with the sharing state worked in:
///
/// - off: everything grey
/// - sharing: orange monitor, flame on a dark screen
/// - someone connected: the screen lights up, and a green dot appears
/// - needs attention: as sharing, with a yellow dot
///
/// Returns `size * size` RGBA pixels. The "sharing" look is the application icon;
/// `scripts/make_icons.py` writes the same shapes to the icon files and a test checks they agree.
pub fn icon_rgba(size: u32, indicator: Indicator) -> Vec<u8> {
    const ORANGE: [u8; 3] = [0xff, 0x7a, 0x2f];
    const YELLOW: [u8; 3] = [0xff, 0xd2, 0x7a];
    const DARK: [u8; 3] = [0x12, 0x15, 0x1c];
    const GREY: [u8; 3] = [0x7c, 0x84, 0x93];
    const GREY_LIGHT: [u8; 3] = [0xb4, 0xba, 0xc6];
    const WHITE: [u8; 3] = [0xff, 0xff, 0xff];

    // Frame, screen, outer flame, inner flame, dot.
    let (frame, screen, flame_outer, flame_inner, dot) = match indicator {
        Indicator::Off => (GREY, DARK, GREY, GREY_LIGHT, None),
        Indicator::On => (ORANGE, DARK, ORANGE, YELLOW, None),
        Indicator::Connected => (ORANGE, YELLOW, ORANGE, WHITE, Some([0x3e, 0xcf, 0x8e])),
        Indicator::Attention => (ORANGE, DARK, ORANGE, YELLOW, Some([0xf2, 0xc1, 0x4e])),
    };

    // Shapes in a 256x256 space.
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
    let disc = |x: f32, y: f32, r: f32| (x - 196.0).powi(2) + (y - 196.0).powi(2) <= r * r;

    let color_at = |x: f32, y: f32| -> Option<[u8; 3]> {
        let mut color = None;
        if rounded_rect(x, y, 100.0, 186.0, 56.0, 26.0, 4.0) || rounded_rect(x, y, 64.0, 210.0, 128.0, 26.0, 13.0) {
            color = Some(frame);
        }
        if rounded_rect(x, y, 6.0, 20.0, 244.0, 176.0, 30.0) {
            color = Some(frame);
        }
        if rounded_rect(x, y, 26.0, 40.0, 204.0, 136.0, 14.0) {
            color = Some(screen);
        }
        if flame(x, y, 122.0, 44.0, 50.0) {
            color = Some(flame_outer);
        }
        if flame(x, y, 138.0, 19.0, 100.0) {
            color = Some(flame_inner);
        }
        if let Some(dot) = dot {
            // A transparent ring around the dot separates it from the monitor.
            if disc(x, y, 62.0) {
                color = None;
            }
            if disc(x, y, 48.0) {
                color = Some(dot);
            }
        }
        color
    };

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
                    if let Some(color) = color_at(x, y) {
                        for i in 0..3 {
                            sum[i] += color[i] as u32;
                        }
                        covered += 1;
                    }
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
    /// Each session gets a submenu with what can be switched in it.
    pub fn update(&mut self, indicator: Indicator, tooltip: &str, sharing: bool, sessions: &Sessions) {
        self.0.update(indicator, tooltip, sharing, sessions);
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
        sessions: Sessions,
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
            use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
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
            let mut menu = vec![
                item("Open PyroMirror", TrayEvent::Open),
                item(if self.sharing { "Stop sharing" } else { "Start sharing" }, TrayEvent::ToggleSharing),
            ];
            for (side, session) in self.sessions.each() {
                let mut submenu: Vec<ksni::MenuItem<Self>> = Action::ALL
                    .into_iter()
                    .filter(|action| *action != Action::Disconnect)
                    .map(|action| {
                        CheckmarkItem {
                            label: action.label().into(),
                            checked: session.toggles.get(action),
                            activate: Box::new(move |model: &mut Self| {
                                let _ = model.events.send(TrayEvent::Session(side, action));
                            }),
                            ..Default::default()
                        }
                        .into()
                    })
                    .collect();
                submenu.push(ksni::MenuItem::Separator);
                submenu.push(item(Action::Disconnect.label(), TrayEvent::Session(side, Action::Disconnect)));
                menu.push(SubMenu { label: session.title(), submenu, ..Default::default() }.into());
            }
            menu.push(ksni::MenuItem::Separator);
            menu.push(item("Quit", TrayEvent::Quit));
            menu
        }
    }

    pub struct Tray {
        handle: ksni::blocking::Handle<Model>,
        shown: (Indicator, String, bool, Sessions),
    }

    impl Tray {
        pub fn new(events: Sender<TrayEvent>) -> Result<Self, String> {
            let shown = (Indicator::Off, String::new(), false, Sessions::default());
            let model = Model { events, indicator: shown.0, tooltip: shown.1.clone(), sharing: shown.2, sessions: Sessions::default() };
            let handle = model.spawn().map_err(|e| e.to_string())?;
            Ok(Self { handle, shown })
        }

        pub fn update(&mut self, indicator: Indicator, tooltip: &str, sharing: bool, sessions: &Sessions) {
            let state = (indicator, tooltip.to_owned(), sharing, sessions.clone());
            if self.shown == state {
                return;
            }
            self.shown = state.clone();
            self.handle.update(move |model| {
                model.indicator = state.0;
                model.tooltip = state.1;
                model.sharing = state.2;
                model.sessions = state.3;
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

    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
    use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE};

    use super::*;

    pub struct Tray {
        icon: TrayIcon,
        menu: Menu,
        toggle: MenuItem,
        viewing: SessionMenu,
        serving: SessionMenu,
        events: Sender<TrayEvent>,
        shown: (Indicator, String, bool),
    }

    /// Where the session submenus go: after "Open" and the sharing toggle.
    const SESSION_POSITION: usize = 2;

    /// A session's submenu; in the tray menu only while there is such a session.
    struct SessionMenu {
        submenu: Submenu,
        checks: Vec<(Action, CheckMenuItem)>,
        shown: bool,
    }

    /// Menu item ids carry the side they belong to.
    fn prefix(side: Side) -> &'static str {
        match side {
            Side::Viewer => "viewer:",
            Side::Host => "host:",
        }
    }

    impl SessionMenu {
        fn new(side: Side) -> Result<Self, String> {
            let id = |action: Action| format!("{}{}", prefix(side), action.name());
            let submenu = Submenu::with_id(prefix(side), "Session", true);
            let mut checks = Vec::new();
            for action in Action::ALL.into_iter().filter(|action| *action != Action::Disconnect) {
                let check = CheckMenuItem::with_id(id(action), action.label(), true, false, None);
                submenu.append(&check).map_err(|e| e.to_string())?;
                checks.push((action, check));
            }
            let disconnect = MenuItem::with_id(id(Action::Disconnect), Action::Disconnect.label(), true, None);
            submenu.append_items(&[&PredefinedMenuItem::separator(), &disconnect]).map_err(|e| e.to_string())?;
            Ok(Self { submenu, checks, shown: false })
        }

        fn update(&mut self, menu: &Menu, session: Option<&Session>) {
            if let Some(session) = session {
                self.submenu.set_text(session.title());
                // Every time: clicking a check mark flips it by itself, whatever the viewer did.
                for (action, check) in &self.checks {
                    check.set_checked(session.toggles.get(*action));
                }
            }
            if session.is_some() != self.shown {
                let changed = if session.is_some() {
                    menu.insert(&self.submenu, SESSION_POSITION)
                } else {
                    menu.remove(&self.submenu)
                };
                if changed.is_ok() {
                    self.shown = session.is_some();
                }
            }
        }
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

            let mut builder = TrayIconBuilder::new().with_menu(Box::new(menu.clone())).with_tooltip("PyroMirror");
            if let Some(icon) = icon(Indicator::Off) {
                builder = builder.with_icon(icon);
            }
            let icon = builder.build().map_err(|e| e.to_string())?;
            Ok(Self {
                icon,
                menu,
                toggle,
                viewing: SessionMenu::new(Side::Viewer)?,
                serving: SessionMenu::new(Side::Host)?,
                events,
                shown: (Indicator::Off, String::new(), false),
            })
        }

        pub fn update(&mut self, indicator: Indicator, tooltip: &str, sharing: bool, sessions: &Sessions) {
            self.viewing.update(&self.menu, sessions.viewing.as_ref());
            self.serving.update(&self.menu, sessions.serving.as_ref());
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
                        "quit" => TrayEvent::Quit,
                        other => {
                            let action = [Side::Viewer, Side::Host].into_iter().find_map(|side| {
                                Some((side, Action::parse(other.strip_prefix(prefix(side))?)?))
                            });
                            match action {
                                Some((side, action)) => TrayEvent::Session(side, action),
                                None => continue,
                            }
                        }
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
    fn icon_shows_each_state() {
        let size = 64;
        let at = |pixels: &[u8], x: u32, y: u32| -> [u8; 4] {
            pixels[((y * size + x) * 4) as usize..][..4].try_into().unwrap()
        };
        let (frame, screen, flame, corner, dot) = ((32, 7), (12, 20), (32, 18), (0, 0), (49, 49));

        let on = icon_rgba(size, Indicator::On);
        assert_eq!(on.len(), (size * size * 4) as usize);
        assert_eq!(at(&on, corner.0, corner.1)[3], 0, "the background is transparent");
        assert_eq!(at(&on, frame.0, frame.1), [0xff, 0x7a, 0x2f, 255], "orange monitor while sharing");
        assert_eq!(at(&on, screen.0, screen.1), [0x12, 0x15, 0x1c, 255], "dark screen");
        assert_eq!(at(&on, flame.0, flame.1), [0xff, 0x7a, 0x2f, 255], "flame");

        let off = icon_rgba(size, Indicator::Off);
        assert_eq!(at(&off, frame.0, frame.1), [0x7c, 0x84, 0x93, 255], "grey monitor while off");
        assert_eq!(at(&off, flame.0, flame.1), [0x7c, 0x84, 0x93, 255], "grey flame while off");

        let connected = icon_rgba(size, Indicator::Connected);
        assert_eq!(at(&connected, screen.0, screen.1), [0xff, 0xd2, 0x7a, 255], "the screen lights up when someone watches");
        assert_eq!(at(&connected, dot.0, dot.1), [0x3e, 0xcf, 0x8e, 255], "with a green dot");

        let attention = icon_rgba(size, Indicator::Attention);
        assert_eq!(at(&attention, screen.0, screen.1), [0x12, 0x15, 0x1c, 255]);
        assert_eq!(at(&attention, dot.0, dot.1), [0xf2, 0xc1, 0x4e, 255], "yellow dot when attention is needed");
    }

    /// The window, shortcut, installer and packages use assets/icon.png; the tray's "sharing"
    /// icon must be the same picture.
    #[test]
    fn tray_icon_matches_the_bundled_app_icon() {
        let png = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).unwrap();
        assert_eq!((png.width, png.height), (256, 256));
        let drawn = icon_rgba(256, Indicator::On);
        let worst = png.rgba.iter().zip(&drawn).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        assert!(worst <= 3, "tray icon differs from the app icon by up to {worst}");
    }
}
