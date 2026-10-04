//! xdg-desktop-portal session: ScreenCast for the picture, RemoteDesktop for input.
//!
//! This is the only route that works across Wayland compositors (GNOME, KDE, ...) and it also
//! covers X11 sessions on those desktops. The portal shows the user a permission dialog; the
//! choice is remembered through a restore token so the dialog only appears on first use.
//!
//! Desktops whose portal has no RemoteDesktop interface (wlroots) get a view-only ScreenCast
//! session.

use std::os::fd::{IntoRawFd, RawFd};
use std::path::PathBuf;
use std::sync::Arc;

use ashpd::desktop::remote_desktop::{DeviceType, RemoteDesktop, SelectDevicesOptions};
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType, Stream};
use ashpd::desktop::{PersistMode, Session};
use ashpd::enumflags2::BitFlags;

use crate::CaptureError;

/// What input injection needs from a RemoteDesktop session. Dropping it closes the session.
pub(crate) struct RemoteControl {
    // Field order matters: the session must go before the runtime that drives its connection.
    pub session: Session<RemoteDesktop>,
    pub proxy: RemoteDesktop,
    pub node: u32,
    /// Logical size of the stream, the coordinate space of absolute pointer motion.
    pub width: f64,
    pub height: f64,
    _screencast: Screencast,
    pub runtime: tokio::runtime::Runtime,
}

/// A ScreenCast-only session, kept alive for as long as the capture runs.
pub(crate) struct ViewOnly {
    _session: Session<Screencast>,
    _proxy: Screencast,
    _runtime: tokio::runtime::Runtime,
}

pub(crate) enum PortalSession {
    Remote(Arc<RemoteControl>),
    ViewOnly(#[allow(dead_code)] ViewOnly),
}

pub(crate) struct PortalStream {
    pub session: PortalSession,
    /// PipeWire remote restricted to the granted stream. Ownership passes to the native backend.
    pub pipewire_fd: RawFd,
    pub pipewire_node: u32,
    /// Whether the desktop handed out a restore token, i.e. will not ask again next time.
    pub remembered: bool,
    /// Whether the pointer comes as stream metadata instead of being drawn into the frames.
    pub cursor_metadata: bool,
}

fn token_path(name: &str) -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(state.join("pyromirror").join(name))
}

fn load_token(name: &str) -> Option<String> {
    let token = std::fs::read_to_string(token_path(name)?).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

fn save_token(name: &str, token: Option<&str>) {
    let (Some(path), Some(token)) = (token_path(name), token) else { return };
    let saved = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&path, token));
    if let Err(e) = saved {
        log::warn!("Could not save the portal restore token to {}: {}", path.display(), e);
    }
}

fn init_error(context: &str, e: impl std::fmt::Display) -> CaptureError {
    CaptureError::InitFailed(format!("{context}: {e}"))
}

fn first_stream(streams: &[Stream]) -> Result<&Stream, CaptureError> {
    let stream = streams
        .first()
        .ok_or_else(|| CaptureError::InitFailed("the portal returned no streams".into()))?;
    if let Some((w, h)) = stream.size() {
        log::info!("Portal granted a {}x{} stream (PipeWire node {})", w, h, stream.pipe_wire_node_id());
    }
    Ok(stream)
}

const UNAVAILABLE: &str = "the ScreenCast portal is unavailable (is xdg-desktop-portal, with a backend \
                           for your desktop, installed and running?)";
const REMOTE_TOKEN: &str = "remote-desktop-restore-token";
const SCREENCAST_TOKEN: &str = "screencast-restore-token";

/// Metadata keeps the pointer out of the picture and reports its shape on the side, so the
/// viewer can draw it locally without lag. Where a desktop cannot do that, Embedded has the
/// compositor draw it into the frames.
async fn cursor_mode(screencast: &Screencast) -> CursorMode {
    match screencast.available_cursor_modes().await {
        Ok(modes) if modes.contains(CursorMode::Metadata) => CursorMode::Metadata,
        _ => CursorMode::Embedded,
    }
}

fn monitor_sources(cursor: CursorMode) -> SelectSourcesOptions {
    SelectSourcesOptions::default()
        .set_cursor_mode(cursor)
        .set_sources(BitFlags::from(SourceType::Monitor))
        .set_multiple(false)
}

/// Screen cast plus keyboard and pointer control. `Ok(None)` means this desktop's portal has no
/// RemoteDesktop interface; errors after that point (e.g. the user declining) are final.
async fn open_remote(screencast: &Screencast, cursor: CursorMode) -> Result<Option<(Session<RemoteDesktop>, RemoteDesktop, RawFd, u32, f64, f64, bool)>, CaptureError> {
    // The portal is D-Bus activated, so a missing interface only shows up on the first call.
    let Ok(remote) = RemoteDesktop::new().await else { return Ok(None) };
    let session = match remote.create_session(Default::default()).await {
        Ok(session) => session,
        Err(e) => {
            log::warn!("RemoteDesktop portal unavailable ({}); the session will be view-only", e);
            return Ok(None);
        }
    };

    let mut devices = SelectDevicesOptions::default().set_devices(DeviceType::Keyboard | DeviceType::Pointer);
    // Persisting the grant needs RemoteDesktop version 2.
    if remote.version() >= 2 {
        let token = load_token(REMOTE_TOKEN);
        devices = devices.set_persist_mode(PersistMode::ExplicitlyRevoked).set_restore_token(token.as_deref());
    }
    remote
        .select_devices(&session, devices)
        .await
        .and_then(|request| request.response())
        .map_err(|e| init_error("RemoteDesktop.SelectDevices failed", e))?;
    screencast
        .select_sources(&session, monitor_sources(cursor))
        .await
        .and_then(|request| request.response())
        .map_err(|e| init_error("ScreenCast.SelectSources failed", e))?;

    log::info!("Waiting for remote control permission (check for a dialog from your desktop)...");
    let granted = remote
        .start(&session, None, Default::default())
        .await
        .and_then(|request| request.response())
        .map_err(|e| init_error("remote control was not granted", e))?;
    save_token(REMOTE_TOKEN, granted.restore_token());
    let remembered = granted.restore_token().is_some();

    let stream = first_stream(granted.streams())?;
    let node = stream.pipe_wire_node_id();
    let (width, height) = stream.size().map_or((0.0, 0.0), |(w, h)| (w as f64, h as f64));

    let fd = screencast
        .open_pipe_wire_remote(&session, Default::default())
        .await
        .map_err(|e| init_error("ScreenCast.OpenPipeWireRemote failed", e))?;
    Ok(Some((session, remote, fd.into_raw_fd(), node, width, height, remembered)))
}

async fn open_view_only(screencast: &Screencast, cursor: CursorMode) -> Result<(Session<Screencast>, RawFd, u32, bool), CaptureError> {
    let session = screencast.create_session(Default::default()).await.map_err(|e| init_error(UNAVAILABLE, e))?;

    let mut sources = monitor_sources(cursor);
    // Persisting the choice needs ScreenCast version 4.
    if screencast.version() >= 4 {
        let token = load_token(SCREENCAST_TOKEN);
        sources = sources.set_persist_mode(PersistMode::ExplicitlyRevoked).set_restore_token(token.as_deref());
    }
    screencast
        .select_sources(&session, sources)
        .await
        .and_then(|request| request.response())
        .map_err(|e| init_error("ScreenCast.SelectSources failed", e))?;

    log::info!("Waiting for screen sharing permission (check for a dialog from your desktop)...");
    let streams = screencast
        .start(&session, None, Default::default())
        .await
        .and_then(|request| request.response())
        .map_err(|e| init_error("screen sharing was not granted", e))?;
    save_token(SCREENCAST_TOKEN, streams.restore_token());
    let remembered = streams.restore_token().is_some();
    let node = first_stream(streams.streams())?.pipe_wire_node_id();

    let fd = screencast
        .open_pipe_wire_remote(&session, Default::default())
        .await
        .map_err(|e| init_error("ScreenCast.OpenPipeWireRemote failed", e))?;
    Ok((session, fd.into_raw_fd(), node, remembered))
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

    enum Opened {
        Remote(Session<RemoteDesktop>, RemoteDesktop, RawFd, u32, f64, f64, bool),
        ViewOnly(Session<Screencast>, RawFd, u32, bool),
    }

    let (screencast, opened, cursor_metadata) = runtime.block_on(async {
        let screencast = Screencast::new().await.map_err(|e| init_error(UNAVAILABLE, e))?;
        let cursor = cursor_mode(&screencast).await;
        let opened = match open_remote(&screencast, cursor).await? {
            Some((session, remote, fd, node, w, h, remembered)) => Opened::Remote(session, remote, fd, node, w, h, remembered),
            None => {
                let (session, fd, node, remembered) = open_view_only(&screencast, cursor).await?;
                Opened::ViewOnly(session, fd, node, remembered)
            }
        };
        Ok::<_, CaptureError>((screencast, opened, cursor == CursorMode::Metadata))
    })?;

    Ok(match opened {
        Opened::Remote(session, proxy, pipewire_fd, node, width, height, remembered) => PortalStream {
            session: PortalSession::Remote(Arc::new(RemoteControl {
                session,
                proxy,
                node,
                width,
                height,
                _screencast: screencast,
                runtime,
            })),
            pipewire_fd,
            pipewire_node: node,
            remembered,
            cursor_metadata,
        },
        Opened::ViewOnly(session, pipewire_fd, pipewire_node, remembered) => PortalStream {
            session: PortalSession::ViewOnly(ViewOnly { _session: session, _proxy: screencast, _runtime: runtime }),
            pipewire_fd,
            pipewire_node,
            remembered,
            cursor_metadata,
        },
    })
}
