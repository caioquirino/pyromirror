//! Desktop notifications ("toasts") for things that happen while nobody is looking at the
//! window: someone connecting, leaving, or asking to pair.

use crate::state::HostState;

/// What to tell the person when the sharing state goes from `before` to `now`, if anything.
pub fn message(before: &HostState, now: &HostState) -> Option<(String, String)> {
    match (before, now) {
        (HostState::Serving(a), HostState::Serving(b)) if a == b => None,
        (_, HostState::Serving(peer)) => Some((
            format!("{peer} connected"),
            "This computer is now being viewed and can be controlled remotely.".into(),
        )),
        (HostState::Serving(peer), _) => Some((format!("{peer} disconnected"), "Nobody is connected to this computer.".into())),
        (HostState::PairingRequest { .. }, HostState::PairingRequest { .. }) => None,
        (_, HostState::PairingRequest { name, .. }) => Some((
            format!("{name} wants to connect"),
            "Open PyroMirror to see the pairing code. Ignore this if you were not expecting it.".into(),
        )),
        _ => None,
    }
}

/// Remembers the last state it saw and shows a notification when it changes in a way that
/// matters.
#[derive(Default)]
pub struct Announcer {
    last: Option<HostState>,
}

impl Announcer {
    pub fn observe(&mut self, state: &HostState) {
        // Nothing is announced for the state we start in.
        if let Some((title, body)) = self.last.as_ref().and_then(|before| message(before, state)) {
            show(&title, &body);
        }
        self.last = Some(state.clone());
    }
}

/// Shows a notification. Failing to (no notification service, for instance) is not an error
/// worth bothering anyone with.
pub fn show(title: &str, body: &str) {
    if let Err(e) = imp::show(title, body) {
        log::debug!("Could not show a notification: {}", e);
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashMap;

    /// The freedesktop notification service, which every mainstream desktop provides.
    pub fn show(title: &str, body: &str) -> Result<(), String> {
        let hints: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
        let actions: Vec<&str> = Vec::new();
        zbus::blocking::Connection::session()
            .and_then(|connection| {
                connection.call_method(
                    Some("org.freedesktop.Notifications"),
                    "/org/freedesktop/Notifications",
                    Some("org.freedesktop.Notifications"),
                    "Notify",
                    // app name, replaces id, icon, summary, body, actions, hints, timeout (ms)
                    &("PyroMirror", 0u32, "pyromirror", title, body, actions, hints, 6000i32),
                )
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::process::CommandExt;
    use std::sync::Once;

    use tauri_winrt_notification::Toast;

    /// Windows attributes a toast to an "application user model id". Registering ours (name and
    /// icon) makes the toast say PyroMirror instead of borrowing another program's identity.
    const APP_ID: &str = "PyroMirror";

    fn register() {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let key = format!(r"HKCU\Software\Classes\AppUserModelId\{APP_ID}");
        let add = |name: &str, value: &str| {
            let _ = std::process::Command::new("reg.exe")
                .args(["add", &key, "/v", name, "/t", "REG_SZ", "/d", value, "/f"])
                .creation_flags(CREATE_NO_WINDOW)
                .output();
        };
        add("DisplayName", "PyroMirror");
        // The icon has to be a file; keep a copy next to the settings.
        if let Some(dir) = pyromirror_proto::auth::config_dir() {
            let icon = dir.join("icon.png");
            if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&icon, include_bytes!("../assets/icon.png")).is_ok() {
                add("IconUri", &icon.display().to_string());
            }
        }
    }

    pub fn show(title: &str, body: &str) -> Result<(), String> {
        static REGISTER: Once = Once::new();
        REGISTER.call_once(register);
        Toast::new(APP_ID).title(title).text1(body).show().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announces_connections_departures_and_pairing_requests_once() {
        let ready = HostState::Ready;
        let serving = HostState::Serving("caio-laptop (192.168.1.7)".into());
        let pairing = HostState::PairingRequest { name: "caio-laptop".into(), code: "123 456".into() };

        assert_eq!(message(&ready, &serving).unwrap().0, "caio-laptop (192.168.1.7) connected");
        assert_eq!(message(&pairing, &serving).unwrap().0, "caio-laptop (192.168.1.7) connected");
        assert_eq!(message(&serving, &ready).unwrap().0, "caio-laptop (192.168.1.7) disconnected");
        assert_eq!(message(&serving, &HostState::Stopped).unwrap().0, "caio-laptop (192.168.1.7) disconnected");
        assert_eq!(message(&ready, &pairing).unwrap().0, "caio-laptop wants to connect");

        // Nothing while a state merely persists, or for changes nobody needs to hear about.
        assert_eq!(message(&serving, &serving), None);
        assert_eq!(message(&pairing, &pairing), None);
        assert_eq!(message(&HostState::Starting, &ready), None);
        assert_eq!(message(&ready, &HostState::Stopped), None);
    }
}
