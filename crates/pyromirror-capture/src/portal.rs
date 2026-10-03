//! xdg-desktop-portal ScreenCast session.
//!
//! This is the only capture route that works across Wayland compositors (GNOME, KDE, wlroots via
//! xdg-desktop-portal-wlr, ...) and it also covers X11 sessions on those desktops. The portal
//! shows the user a monitor picker; the choice is remembered through a restore token so the
//! dialog only appears on first use.

use std::os::fd::{IntoRawFd, RawFd};
use std::path::PathBuf;

use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::{PersistMode, Session};

use crate::CaptureError;

/// Keeps the portal session (and with it the PipeWire stream) alive.
pub(crate) struct PortalSession {
    // Field order matters: the session must go before the runtime that drives its connection.
    _session: Session<Screencast>,
    _proxy: Screencast,
    _runtime: tokio::runtime::Runtime,
}

pub(crate) struct PortalStream {
    pub session: PortalSession,
    /// PipeWire remote restricted to the granted stream. Ownership passes to the native backend.
    pub pipewire_fd: RawFd,
    pub pipewire_node: u32,
}

fn token_path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(state.join("pyromirror/screencast-restore-token"))
}

fn init_error(context: &str, e: impl std::fmt::Display) -> CaptureError {
    CaptureError::InitFailed(format!("{context}: {e}"))
}

pub(crate) fn open() -> Result<PortalStream, CaptureError> {
    // zbus runs its connection on this runtime, so it needs a worker thread of its own that keeps
    // going after the setup below returns.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("pyromirror-portal")
        .enable_all()
        .build()
        .map_err(|e| init_error("failed to start the portal runtime", e))?;

    let token_path = token_path();
    let saved_token = token_path.as_ref().and_then(|p| std::fs::read_to_string(p).ok());

    let (proxy, session, fd, node, new_token) = runtime.block_on(async {
        // The portal is D-Bus activated, so a missing portal only shows up on the first call.
        const UNAVAILABLE: &str = "the ScreenCast portal is unavailable (is xdg-desktop-portal, with a \
                                   backend for your desktop, installed and running?)";
        let proxy = Screencast::new().await.map_err(|e| init_error(UNAVAILABLE, e))?;
        let session = proxy
            .create_session(Default::default())
            .await
            .map_err(|e| init_error(UNAVAILABLE, e))?;

        // Embedded makes the compositor draw the pointer into the frames. Persisting the choice
        // needs portal version 4.
        let mut options = SelectSourcesOptions::default()
            .set_cursor_mode(CursorMode::Embedded)
            .set_sources(ashpd::enumflags2::BitFlags::from(SourceType::Monitor))
            .set_multiple(false);
        if proxy.version() >= 4 {
            options = options
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(saved_token.as_deref().map(str::trim).filter(|t| !t.is_empty()));
        }
        proxy
            .select_sources(&session, options)
            .await
            .and_then(|request| request.response())
            .map_err(|e| init_error("ScreenCast.SelectSources failed", e))?;

        log::info!("Waiting for screen sharing permission (check for a dialog from your desktop)...");
        let streams = proxy
            .start(&session, None, Default::default())
            .await
            .and_then(|request| request.response())
            .map_err(|e| init_error("screen sharing was not granted", e))?;

        let stream = streams
            .streams()
            .first()
            .ok_or_else(|| CaptureError::InitFailed("the portal returned no streams".into()))?;
        let node = stream.pipe_wire_node_id();
        if let Some((w, h)) = stream.size() {
            log::info!("Portal granted a {}x{} stream (PipeWire node {})", w, h, node);
        }
        let new_token = streams.restore_token().map(str::to_owned);

        let fd = proxy
            .open_pipe_wire_remote(&session, Default::default())
            .await
            .map_err(|e| init_error("ScreenCast.OpenPipeWireRemote failed", e))?;

        Ok::<_, CaptureError>((proxy, session, fd, node, new_token))
    })?;

    if let (Some(path), Some(token)) = (token_path, new_token) {
        let saved = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&path, token));
        if let Err(e) = saved {
            log::warn!("Could not save the screen sharing restore token to {}: {}", path.display(), e);
        }
    }

    Ok(PortalStream {
        session: PortalSession { _session: session, _proxy: proxy, _runtime: runtime },
        pipewire_fd: fd.into_raw_fd(),
        pipewire_node: node,
    })
}
