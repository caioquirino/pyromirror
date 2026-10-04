//! Controlling a running session from outside the viewer window (the tray menus).
//!
//! The viewer is what fullscreen, mouse lock and the rest belong to. Two tray menus can steer it:
//! the one on the viewing computer, and the one on the computer being viewed, which is reachable
//! through the session itself when everything else is out of reach.
//!
//! Each side uses two small files in the configuration directory: the program holding the
//! session (the viewer, or the server on behalf of its client) writes what is switched on to one,
//! and reads actions appended to the other. They belong to the user, like the rest of that
//! directory. Between the two computers, actions and state travel on the control connection.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use crate::auth::config_dir;

/// What can be switched during a session, from the viewer's menu, a hotkey or the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Fullscreen,
    KeyboardGrab,
    MouseLock,
    RelativeMouse,
    Mute,
    Disconnect,
}

impl Action {
    pub const ALL: [Action; 6] =
        [Action::Fullscreen, Action::KeyboardGrab, Action::MouseLock, Action::RelativeMouse, Action::Mute, Action::Disconnect];

    pub fn label(self) -> &'static str {
        match self {
            Action::Fullscreen => "Fullscreen",
            Action::KeyboardGrab => "Grab keys",
            Action::MouseLock => "Lock mouse",
            Action::RelativeMouse => "Relative mouse",
            Action::Mute => "Mute",
            Action::Disconnect => "Disconnect",
        }
    }

    /// The word used in the files.
    pub fn name(self) -> &'static str {
        match self {
            Action::Fullscreen => "fullscreen",
            Action::KeyboardGrab => "grab-keys",
            Action::MouseLock => "lock-mouse",
            Action::RelativeMouse => "relative-mouse",
            Action::Mute => "mute",
            Action::Disconnect => "disconnect",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.name() == name)
    }

    /// The byte that stands for this action on the control connection.
    pub fn to_byte(self) -> u8 {
        Self::ALL.iter().position(|a| *a == self).unwrap_or(0) as u8
    }

    pub fn from_byte(byte: u8) -> Option<Self> {
        Self::ALL.get(byte as usize).copied()
    }
}

/// Which end of a session a tray menu belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The computer showing the remote desktop.
    Viewer,
    /// The computer being shown.
    Host,
}

impl Side {
    fn state_file(self) -> &'static str {
        match self {
            Side::Viewer => "viewer.state",
            Side::Host => "host.state",
        }
    }

    fn commands_file(self) -> &'static str {
        match self {
            Side::Viewer => "viewer.commands",
            Side::Host => "host.commands",
        }
    }
}

/// Which of the toggles are on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Toggles {
    pub fullscreen: bool,
    pub keyboard_grab: bool,
    pub mouse_lock: bool,
    pub relative_mouse: bool,
    pub muted: bool,
}

impl Toggles {
    pub fn get(&self, action: Action) -> bool {
        match action {
            Action::Fullscreen => self.fullscreen,
            Action::KeyboardGrab => self.keyboard_grab,
            Action::MouseLock => self.mouse_lock,
            Action::RelativeMouse => self.relative_mouse,
            Action::Mute => self.muted,
            Action::Disconnect => false,
        }
    }

    /// One bit per toggle, for the control connection.
    pub fn to_bits(self) -> u8 {
        Action::ALL.into_iter().filter(|a| self.get(*a)).fold(0, |bits, a| bits | 1 << a.to_byte())
    }

    pub fn from_bits(bits: u8) -> Self {
        let on = |action: Action| bits & (1 << action.to_byte()) != 0;
        Self {
            fullscreen: on(Action::Fullscreen),
            keyboard_grab: on(Action::KeyboardGrab),
            mouse_lock: on(Action::MouseLock),
            relative_mouse: on(Action::RelativeMouse),
            muted: on(Action::Mute),
        }
    }

    fn to_text(self) -> String {
        Action::ALL.into_iter().filter(|a| self.get(*a)).map(Action::name).collect::<Vec<_>>().join(" ")
    }

    fn from_text(text: &str) -> Self {
        let on = |action: Action| text.split_whitespace().any(|word| word == action.name());
        Self {
            fullscreen: on(Action::Fullscreen),
            keyboard_grab: on(Action::KeyboardGrab),
            mouse_lock: on(Action::MouseLock),
            relative_mouse: on(Action::RelativeMouse),
            muted: on(Action::Mute),
        }
    }
}


/// The end that holds the session: the viewer, or the server for the client it is serving.
pub struct Listener {
    side: Side,
    dir: Option<PathBuf>,
    commands: Option<PathBuf>,
    /// How much of the commands file has been acted on.
    offset: u64,
    published: Option<Toggles>,
}

impl Listener {
    /// Starts with an empty commands file, so nothing meant for an earlier session is replayed.
    pub fn new(side: Side) -> Self {
        Self::in_dir(side, config_dir())
    }

    fn in_dir(side: Side, dir: Option<PathBuf>) -> Self {
        let commands = dir
            .as_ref()
            .filter(|dir| fs::create_dir_all(dir).is_ok())
            .map(|dir| dir.join(side.commands_file()))
            .filter(|path| fs::write(path, "").is_ok());
        Self { side, dir, commands, offset: 0, published: None }
    }

    /// Actions requested since the last call.
    pub fn poll(&mut self) -> Vec<Action> {
        let Some(path) = &self.commands else { return Vec::new() };
        let Ok(mut file) = fs::File::open(path) else { return Vec::new() };
        let len = file.metadata().map_or(0, |m| m.len());
        if len <= self.offset {
            return Vec::new();
        }
        let mut text = String::new();
        if file.seek(SeekFrom::Start(self.offset)).is_err() || file.read_to_string(&mut text).is_err() {
            return Vec::new();
        }
        // Only whole lines; the rest of a half-written one comes next time.
        let complete = text.rfind('\n').map_or(0, |end| end + 1);
        self.offset += complete as u64;
        text[..complete].lines().filter_map(|line| Action::parse(line.trim())).collect()
    }

    /// Tells the outside what is switched on. Cheap to call every frame.
    pub fn publish(&mut self, toggles: Toggles) {
        if self.published == Some(toggles) {
            return;
        }
        self.published = Some(toggles);
        if let Some(dir) = &self.dir {
            let _ = fs::write(dir.join(self.side.state_file()), toggles.to_text() + "\n");
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Some(dir) = &self.dir {
            let _ = fs::remove_file(dir.join(self.side.state_file()));
        }
    }
}

/// What the session on that side has switched on; `None` if there is none to steer.
pub fn toggles(side: Side) -> Option<Toggles> {
    toggles_in(side, &config_dir()?)
}

fn toggles_in(side: Side, dir: &std::path::Path) -> Option<Toggles> {
    fs::read_to_string(dir.join(side.state_file())).ok().map(|text| Toggles::from_text(&text))
}

/// Asks the session on that side to do something.
pub fn send(side: Side, action: Action) -> std::io::Result<()> {
    send_in(side, &config_dir().ok_or_else(|| std::io::Error::other("no configuration directory"))?, action)
}

fn send_in(side: Side, dir: &std::path::Path, action: Action) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new().append(true).create(true).open(dir.join(side.commands_file()))?;
    writeln!(file, "{}", action.name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for action in Action::ALL {
            assert_eq!(Action::parse(action.name()), Some(action));
        }
        assert_eq!(Action::parse("format-disk"), None);
    }

    #[test]
    fn a_viewer_hears_the_tray_and_the_tray_sees_the_viewer() {
        let dir = std::env::temp_dir().join(format!("pyromirror-control-test-{}", std::process::id()));
        // Left over from an earlier session; must not be replayed.
        fs::create_dir_all(&dir).unwrap();
        send_in(Side::Viewer, &dir, Action::Disconnect).unwrap();

        let mut viewer = Listener::in_dir(Side::Viewer, Some(dir.clone()));
        assert_eq!(viewer.poll(), []);
        assert_eq!(toggles_in(Side::Viewer, &dir), None);

        viewer.publish(Toggles { muted: true, ..Default::default() });
        assert_eq!(toggles_in(Side::Viewer, &dir), Some(Toggles { muted: true, ..Default::default() }));

        send_in(Side::Viewer, &dir, Action::RelativeMouse).unwrap();
        send_in(Side::Viewer, &dir, Action::Fullscreen).unwrap();
        assert_eq!(viewer.poll(), [Action::RelativeMouse, Action::Fullscreen]);
        assert_eq!(viewer.poll(), []);
        send_in(Side::Viewer, &dir, Action::Disconnect).unwrap();
        assert_eq!(viewer.poll(), [Action::Disconnect]);

        // The other side has files of its own.
        assert_eq!(toggles_in(Side::Host, &dir), None);
        // The state goes away with the viewer.
        drop(viewer);
        assert_eq!(toggles_in(Side::Viewer, &dir), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wire_forms_round_trip() {
        for action in Action::ALL {
            assert_eq!(Action::from_byte(action.to_byte()), Some(action));
        }
        assert_eq!(Action::from_byte(200), None);
        let toggles = Toggles { keyboard_grab: true, muted: true, ..Default::default() };
        assert_eq!(Toggles::from_bits(toggles.to_bits()), toggles);
    }

    #[test]
    fn toggles_round_trip() {
        let toggles = Toggles { fullscreen: true, relative_mouse: true, ..Default::default() };
        assert_eq!(toggles.to_text(), "fullscreen relative-mouse");
        assert_eq!(Toggles::from_text(&toggles.to_text()), toggles);
        assert_eq!(Toggles::from_text(""), Toggles::default());
    }
}
