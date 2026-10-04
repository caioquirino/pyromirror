//! Background mode (`pyromirror --background`): no window, a tray icon, and optionally sharing
//! started right away. This is what runs at login.

use std::sync::mpsc;
use std::time::Duration;

use crate::config::Config;
use crate::daemon::{sibling, Daemon};
use crate::state::{host_state, HostState};
use crate::tray::{Indicator, Tray, TrayEvent};

/// Opens the launcher window, unless one is already open.
fn open_window() {
    if Daemon::window().is_running() {
        return;
    }
    if let Err(e) = std::process::Command::new(sibling("pyromirror")).spawn() {
        log::warn!("Could not open the PyroMirror window: {}", e);
    }
}

/// What the tray should show for a server state.
pub fn summarize(state: &HostState) -> (Indicator, String) {
    match state {
        HostState::Stopped => (Indicator::Off, "Sharing is off".into()),
        HostState::Failed => (Indicator::Attention, "Sharing could not start".into()),
        HostState::Starting => (Indicator::On, "Starting to share".into()),
        HostState::WaitingForPermission => (Indicator::Attention, "Waiting for your permission".into()),
        HostState::Ready => (Indicator::On, "Sharing is on".into()),
        HostState::PairingRequest { name, .. } => (Indicator::Attention, format!("{name} wants to connect")),
        HostState::Serving(peer) => (Indicator::Connected, format!("{peer} is connected")),
    }
}

pub fn run() {
    let agent = Daemon::agent();
    if !agent.claim() {
        log::info!("PyroMirror is already running in the background");
        return;
    }
    let server = Daemon::server();

    let config = Config::load();
    if config.auto_share {
        if let Err(e) = server.start(&config.server_args()) {
            log::error!("Could not start sharing: {}", e);
        }
    }

    let (events_tx, events) = mpsc::channel();
    let mut tray = match Tray::new(events_tx) {
        Ok(tray) => Some(tray),
        Err(e) => {
            // Typical on GNOME without the AppIndicator extension. Sharing still works; the
            // window is reachable from the application menu.
            log::warn!("No tray icon ({}); continuing without one", e);
            None
        }
    };

    let mut announcer = crate::notify::Announcer::default();
    let mut started_here = server.is_running();
    let mut was_attention = false;
    loop {
        let running = server.is_running();
        let state = if running {
            host_state(&server.log())
        } else if started_here && server.log().iter().any(|l| l.text.starts_with("Error")) {
            HostState::Failed
        } else {
            HostState::Stopped
        };

        // A pairing request (or a failure) needs the window; bring it up once.
        let attention = matches!(state, HostState::PairingRequest { .. } | HostState::Failed);
        if attention && !was_attention {
            open_window();
        }
        was_attention = attention;

        announcer.observe(&state);
        let (indicator, tooltip) = summarize(&state);
        if let Some(tray) = &mut tray {
            tray.update(indicator, &tooltip, running);
            tray.pump(Duration::from_millis(400));
        } else {
            std::thread::sleep(Duration::from_millis(400));
        }

        while let Ok(event) = events.try_recv() {
            match event {
                TrayEvent::Open => open_window(),
                TrayEvent::ToggleSharing if running => {
                    server.stop();
                    started_here = false;
                }
                TrayEvent::ToggleSharing => {
                    // Settings may have changed in the window since we started.
                    match server.start(&Config::load().server_args()) {
                        Ok(()) => started_here = true,
                        Err(e) => log::error!("Could not start sharing: {}", e),
                    }
                }
                TrayEvent::Quit => {
                    // Without the icon nothing would show that sharing is on, so it ends too.
                    server.stop();
                    agent.release();
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_reflects_what_needs_attention() {
        assert_eq!(summarize(&HostState::Stopped).0, Indicator::Off);
        assert_eq!(summarize(&HostState::Ready).0, Indicator::On);
        assert_eq!(summarize(&HostState::Serving("laptop".into())), (Indicator::Connected, "laptop is connected".into()));
        let pairing = HostState::PairingRequest { name: "laptop".into(), code: "123 456".into() };
        assert_eq!(summarize(&pairing), (Indicator::Attention, "laptop wants to connect".into()));
    }
}
