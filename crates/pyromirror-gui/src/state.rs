//! Reading what the server and the viewer are doing from the lines they log.

use crate::process::LogLine;

/// What the server is doing, as far as its log tells.
#[derive(Clone, PartialEq)]
pub enum HostState {
    Stopped,
    Starting,
    WaitingForPermission,
    Ready,
    /// An unknown computer wants to connect; the code has to be typed on it.
    PairingRequest { name: String, code: String },
    Serving(String),
    Failed,
}

pub fn host_state(log: &[LogLine]) -> HostState {
    let mut state = HostState::Starting;
    let mut peer = String::new();
    for line in log {
        let text = line.text.as_str();
        if text.starts_with("Waiting for") && text.contains("permission") {
            state = HostState::WaitingForPermission;
        } else if text.starts_with("Listening on") {
            state = HostState::Ready;
        } else if let Some(address) = text.strip_prefix("Client connected from ") {
            // Drop the ephemeral TCP port; the address is what people recognise. Until the
            // client has been accepted nothing is being shared with it yet.
            peer = address.rsplit_once(':').map_or(address, |(host, _)| host).to_owned();
        } else if let Some((name, code)) = text.strip_prefix("Pairing request from ").and_then(|r| r.rsplit_once(": code ")) {
            // Formatted for reading across the room: "123 456".
            let code = if code.len() == 6 { format!("{} {}", &code[..3], &code[3..]) } else { code.to_owned() };
            state = HostState::PairingRequest { name: name.to_owned(), code };
        } else if let Some(name) = text.strip_prefix("Accepted ") {
            state = HostState::Serving(if peer.is_empty() { name.to_owned() } else { format!("{} ({})", name, peer) });
        } else if text.ends_with(" disconnected") || text.starts_with("Session with ") {
            state = HostState::Ready;
        }
    }
    state
}

/// What the viewer is doing, as far as its log tells.
#[derive(Clone, PartialEq)]
pub enum ClientState {
    Connecting,
    /// The host is showing a pairing code that has to be typed here. `revoked` means this
    /// computer used to be paired and the host has removed it.
    CodeNeeded { wrong: bool, revoked: bool },
    Connected,
}

pub fn client_state(log: &[LogLine]) -> ClientState {
    let mut state = ClientState::Connecting;
    let (mut wrong, mut revoked) = (false, false);
    for line in log {
        let text = line.text.as_str();
        if text.starts_with("Wrong pairing code") {
            wrong = true;
        } else if text.contains("no longer paired with the host") {
            revoked = true;
        } else if text.starts_with("Pairing code needed") {
            state = ClientState::CodeNeeded { wrong, revoked };
        } else if text.starts_with("Stream: ") {
            state = ClientState::Connected;
        }
    }
    state
}

/// The name of the computer the viewer is connected to.
pub fn viewed_host(log: &[LogLine]) -> Option<String> {
    let (_id, name) = log.iter().find_map(|l| l.text.strip_prefix("Host: "))?.split_once(' ')?;
    Some(name.trim().to_owned()).filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::parse_line;

    #[test]
    fn viewed_host_is_the_name_after_the_id() {
        let log = [parse_line("[2026-01-01T00:00:00Z INFO  pyromirror_client] Host: 2b706206d8f0 Living room PC")];
        assert_eq!(viewed_host(&log).as_deref(), Some("Living room PC"));
        assert_eq!(viewed_host(&[]), None);
    }
}
