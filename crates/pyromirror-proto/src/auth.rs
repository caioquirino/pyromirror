//! Pairing: proving that a client is allowed to connect.
//!
//! A client the host does not know yet has to be paired once. When it connects, the host makes
//! up a one-time 6-digit code and shows it; the person types it on the client. Both sides then
//! derive a long-lived token and remember each other, so later connections need no code. Neither
//! the code nor the token crosses the network: each is used to key an HMAC over a fresh random
//! challenge.
//!
//! The exchange, after the client's hello:
//!
//! 1. host -> client: [`AuthChallenge`]
//! 2. client -> host: [`AuthResponse`] with its token proof, or nothing to show
//! 3. host -> client: [`AuthResult::Ok`], or [`AuthResult::CodeNeeded`] once it displays a code
//! 4. client -> host: [`AuthResponse`] proving the code; the host answers `Ok` or `WrongCode`,
//!    and the client may retry a few times
//!
//! This keeps strangers out. It is not encryption: the session itself is still readable by
//! anyone on the path, and someone who records a pairing can guess a 6-digit code offline. Pair
//! on a network you trust.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::{read_message, write_message, ProtoError, MAX_MESSAGE_PAYLOAD};

pub const MSG_TYPE_AUTH_CHALLENGE: u32 = 2;
pub const MSG_TYPE_AUTH_RESPONSE: u32 = 3;
pub const MSG_TYPE_AUTH_RESULT: u32 = 4;

pub type Id = [u8; 16];
pub type Token = [u8; 32];

/// Longest device name sent in either direction.
pub const MAX_NAME: usize = 64;

/// Names come from the other machine and are shown to a person: keep them short and printable.
fn clean_name(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).chars().filter(|c| !c.is_control()).take(MAX_NAME).collect()
}

fn push_name(buf: &mut Vec<u8>, name: &str) {
    // Cut at a character boundary.
    let mut end = name.len().min(MAX_NAME);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    buf.extend_from_slice(&name.as_bytes()[..end]);
}

/// Sent by the server right after the client's hello.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthChallenge {
    /// False if the host accepts anyone.
    pub required: bool,
    /// Stable identity of the host, so a client recognises it under a new address.
    pub server_id: Id,
    pub nonce: [u8; 16],
    /// What the host calls itself, for the client's list of paired computers.
    pub name: String,
}

impl AuthChallenge {
    const FIXED: usize = 33;

    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = vec![0u8; Self::FIXED];
        buf[0] = self.required as u8;
        buf[1..17].copy_from_slice(&self.server_id);
        buf[17..33].copy_from_slice(&self.nonce);
        push_name(&mut buf, &self.name);
        buf
    }

    pub fn deserialize(buf: &[u8]) -> Result<Self, ProtoError> {
        if buf.len() < Self::FIXED {
            return Err(ProtoError::BufferTooSmall { required: Self::FIXED, provided: buf.len() });
        }
        Ok(Self {
            required: buf[0] != 0,
            server_id: buf[1..17].try_into().unwrap(),
            nonce: buf[17..33].try_into().unwrap(),
            name: clean_name(&buf[Self::FIXED..]),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    /// Nothing to prove (only accepted when the host does not require pairing).
    None = 0,
    /// First contact: proves knowledge of the pairing code.
    Code = 1,
    /// Proves possession of the token from an earlier pairing.
    Token = 2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthResponse {
    pub method: AuthMethod,
    pub client_id: Id,
    pub mac: [u8; 32],
    /// What the client calls itself, for the host to show in a pairing request.
    pub name: String,
}

impl AuthResponse {
    const FIXED: usize = 49;

    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = vec![0u8; Self::FIXED];
        buf[0] = self.method as u8;
        buf[1..17].copy_from_slice(&self.client_id);
        buf[17..49].copy_from_slice(&self.mac);
        push_name(&mut buf, &self.name);
        buf
    }

    pub fn deserialize(buf: &[u8]) -> Result<Self, ProtoError> {
        if buf.len() < Self::FIXED {
            return Err(ProtoError::BufferTooSmall { required: Self::FIXED, provided: buf.len() });
        }
        let method = match buf[0] {
            0 => AuthMethod::None,
            1 => AuthMethod::Code,
            2 => AuthMethod::Token,
            other => return Err(ProtoError::UnknownMessageType(other as u32)),
        };
        let name = clean_name(&buf[Self::FIXED..]);
        Ok(Self { method, client_id: buf[1..17].try_into().unwrap(), mac: buf[17..49].try_into().unwrap(), name })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthResult {
    Ok = 0,
    WrongCode = 1,
    /// The host does not know this client and does not accept new ones.
    NotPaired = 2,
    /// The host is showing a pairing code; the client should send a [`AuthMethod::Code`] proof.
    CodeNeeded = 3,
}

impl AuthResult {
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => AuthResult::Ok,
            1 => AuthResult::WrongCode,
            3 => AuthResult::CodeNeeded,
            _ => AuthResult::NotPaired,
        }
    }
}

/// How many codes a client may try before the host drops the pairing request.
pub const MAX_CODE_ATTEMPTS: u32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("connection problem during pairing: {0}")]
    Io(#[from] std::io::Error),
    #[error("unexpected message during pairing: {0}")]
    Protocol(#[from] ProtoError),
    #[error("unexpected message type {0} during pairing; is the other side running a different version?")]
    UnexpectedMessage(u32),
    #[error("wrong pairing code")]
    WrongCode,
    #[error("pairing was cancelled")]
    Cancelled,
    #[error("the host does not accept this computer")]
    Rejected,
}

fn read_typed<S: Read>(stream: &mut S, expected: u32, payload: &mut [u8; MAX_MESSAGE_PAYLOAD]) -> Result<usize, AuthError> {
    let (msg_type, len) = read_message(stream, payload)?;
    if msg_type != expected {
        return Err(AuthError::UnexpectedMessage(msg_type));
    }
    Ok(len)
}

/// A client the host let in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub name: String,
    pub client_id: Id,
}

/// Host side of the exchange. `announce` is called with the client's name and a fresh code when
/// a pairing request has to be shown to the person at the host.
///
/// The caller should put a read timeout on `stream` that leaves time to type a code.
pub fn serve<S: Read + Write>(
    stream: &mut S,
    required: bool,
    server_id: Id,
    server_name: &str,
    paired: &mut TokenStore,
    mut announce: impl FnMut(&str, &str),
) -> Result<Accepted, AuthError> {
    let challenge = AuthChallenge { required, server_id, nonce: random(), name: server_name.to_owned() };
    write_message(stream, MSG_TYPE_AUTH_CHALLENGE, &challenge.serialize())?;

    let mut payload = [0u8; MAX_MESSAGE_PAYLOAD];
    let len = read_typed(stream, MSG_TYPE_AUTH_RESPONSE, &mut payload)?;
    let response = AuthResponse::deserialize(&payload[..len])?;
    let name = if response.name.is_empty() { "an unnamed computer".to_owned() } else { response.name.clone() };

    let known = response.method == AuthMethod::Token
        && paired
            .get(&response.client_id)
            .is_some_and(|token| verify_token(token, &challenge, &response.client_id, &response.mac));
    if !required || known {
        write_message(stream, MSG_TYPE_AUTH_RESULT, &[AuthResult::Ok as u8])?;
        return Ok(Accepted { name, client_id: response.client_id });
    }

    // Unknown (or forgotten) client: show a one-time code and wait for it to come back.
    let code = generate_code();
    announce(&name, &code);
    write_message(stream, MSG_TYPE_AUTH_RESULT, &[AuthResult::CodeNeeded as u8])?;

    for attempt in 1..=MAX_CODE_ATTEMPTS {
        let len = read_typed(stream, MSG_TYPE_AUTH_RESPONSE, &mut payload)?;
        let answer = AuthResponse::deserialize(&payload[..len])?;
        let right = answer.method == AuthMethod::Code
            && answer.client_id == response.client_id
            && verify_code(&code, &challenge, &answer.client_id, &answer.mac);
        if right {
            let token = derive_token(&code, &challenge, &answer.client_id);
            // If this fails the client is simply asked for a code again next time.
            let _ = paired.insert(answer.client_id, token, &name);
            write_message(stream, MSG_TYPE_AUTH_RESULT, &[AuthResult::Ok as u8])?;
            return Ok(Accepted { name, client_id: answer.client_id });
        }
        // Slows down guessing; the code dies with this connection anyway.
        std::thread::sleep(std::time::Duration::from_secs(1));
        let last = attempt == MAX_CODE_ATTEMPTS;
        let result = if last { AuthResult::NotPaired } else { AuthResult::WrongCode };
        write_message(stream, MSG_TYPE_AUTH_RESULT, &[result as u8])?;
    }
    Err(AuthError::WrongCode)
}

/// Why the client is being asked for a pairing code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodePrompt {
    /// Codes already tried and refused during this request.
    pub wrong_attempts: u32,
    /// True if this computer was paired with the host before, but the host has since removed
    /// it: the old pairing is void and a new one is needed.
    pub pairing_revoked: bool,
}

/// Client side of the exchange. `ask_code` is called when the host shows a pairing code; it
/// returns what the person typed, or `None` to give up. Returns true if this connection created
/// a new pairing.
pub fn connect<S: Read + Write>(
    stream: &mut S,
    client_id: Id,
    name: &str,
    hosts: &mut TokenStore,
    mut ask_code: impl FnMut(CodePrompt) -> Option<String>,
) -> Result<bool, AuthError> {
    let mut payload = [0u8; MAX_MESSAGE_PAYLOAD];
    let len = read_typed(stream, MSG_TYPE_AUTH_CHALLENGE, &mut payload)?;
    let challenge = AuthChallenge::deserialize(&payload[..len])?;

    let (method, mac) = match hosts.get(&challenge.server_id) {
        Some(token) => (AuthMethod::Token, token_proof(token, &challenge, &client_id)),
        None => (AuthMethod::None, [0u8; 32]),
    };
    let response = AuthResponse { method, client_id, mac, name: name.to_owned() };
    write_message(stream, MSG_TYPE_AUTH_RESPONSE, &response.serialize())?;

    let result = |payload: &[u8]| AuthResult::from_byte(payload.first().copied().unwrap_or(AuthResult::NotPaired as u8));

    let len = read_typed(stream, MSG_TYPE_AUTH_RESULT, &mut payload)?;
    match result(&payload[..len]) {
        AuthResult::Ok => return Ok(false),
        AuthResult::CodeNeeded => {}
        AuthResult::WrongCode | AuthResult::NotPaired => return Err(AuthError::Rejected),
    }

    // We offered a token and were still asked for a code: the host has removed this computer.
    // The token is worthless now, so forget it whether or not the new pairing goes through.
    let pairing_revoked = method == AuthMethod::Token;
    if pairing_revoked {
        let _ = hosts.remove(&challenge.server_id);
    }

    let mut wrong_attempts = 0;
    loop {
        let prompt = CodePrompt { wrong_attempts, pairing_revoked };
        let code = normalize_code(&ask_code(prompt).ok_or(AuthError::Cancelled)?);
        let answer = AuthResponse {
            method: AuthMethod::Code,
            client_id,
            mac: code_proof(&code, &challenge, &client_id),
            name: name.to_owned(),
        };
        write_message(stream, MSG_TYPE_AUTH_RESPONSE, &answer.serialize())?;

        let len = read_typed(stream, MSG_TYPE_AUTH_RESULT, &mut payload)?;
        match result(&payload[..len]) {
            AuthResult::Ok => {
                // If this fails the host simply asks for a code again next time.
                let host_name = if challenge.name.is_empty() { "unnamed computer" } else { &challenge.name };
                let _ = hosts.insert(challenge.server_id, derive_token(&code, &challenge, &client_id), host_name);
                return Ok(true);
            }
            AuthResult::WrongCode => wrong_attempts += 1,
            // Out of attempts.
            AuthResult::NotPaired | AuthResult::CodeNeeded => return Err(AuthError::WrongCode),
        }
    }
}

/// A name for this computer to show in pairing requests.
pub fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "another computer".to_owned())
}

fn mac(key: &[u8], label: &[u8], challenge: &AuthChallenge, client_id: &Id) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(label);
    mac.update(&challenge.server_id);
    mac.update(&challenge.nonce);
    mac.update(client_id);
    mac
}

/// Proof that the sender knows the pairing code.
pub fn code_proof(code: &str, challenge: &AuthChallenge, client_id: &Id) -> [u8; 32] {
    mac(code.as_bytes(), b"pyromirror pair", challenge, client_id).finalize().into_bytes().into()
}

/// The long-lived token both sides derive from a successful pairing.
pub fn derive_token(code: &str, challenge: &AuthChallenge, client_id: &Id) -> Token {
    mac(code.as_bytes(), b"pyromirror token", challenge, client_id).finalize().into_bytes().into()
}

/// Proof that the sender holds the token from an earlier pairing.
pub fn token_proof(token: &Token, challenge: &AuthChallenge, client_id: &Id) -> [u8; 32] {
    mac(token, b"pyromirror auth", challenge, client_id).finalize().into_bytes().into()
}

/// Constant-time check of a [`code_proof`].
pub fn verify_code(code: &str, challenge: &AuthChallenge, client_id: &Id, proof: &[u8; 32]) -> bool {
    mac(code.as_bytes(), b"pyromirror pair", challenge, client_id).verify_slice(proof).is_ok()
}

/// Constant-time check of a [`token_proof`].
pub fn verify_token(token: &Token, challenge: &AuthChallenge, client_id: &Id, proof: &[u8; 32]) -> bool {
    mac(token, b"pyromirror auth", challenge, client_id).verify_slice(proof).is_ok()
}

pub fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the operating system has no random number source");
    bytes
}

/// A fresh 6-digit pairing code.
pub fn generate_code() -> String {
    // 2^64 is not a multiple of 10^6, but the bias is about 1 in 10^13.
    format!("{:06}", u64::from_le_bytes(random()) % 1_000_000)
}

/// Strips the spaces and dashes people add when typing a code.
pub fn normalize_code(code: &str) -> String {
    code.chars().filter(|c| !c.is_whitespace() && *c != '-').collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn from_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Where pairing data lives: `%APPDATA%\pyromirror` or `~/.config/pyromirror`.
pub fn config_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }?;
    Some(base.join("pyromirror"))
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// This installation's identity, created on first use.
pub fn local_id(path: &Path) -> Id {
    if let Some(id) = std::fs::read_to_string(path).ok().and_then(|text| from_hex(text.trim())) {
        return id;
    }
    let id: Id = random();
    // If it cannot be saved, pairings simply will not survive a restart.
    let _ = write_private(path, &to_hex(&id));
    id
}

/// One computer this installation is paired with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedDevice {
    pub id: Id,
    pub token: Token,
    /// What the other computer called itself when it was paired.
    pub name: String,
}

/// The computers this installation is paired with, one `<id> <token> <name>` per line.
///
/// The file is the source of truth: the launcher edits it while the server is running, so
/// callers reload it whenever they are about to rely on it.
pub struct TokenStore {
    path: PathBuf,
    entries: Vec<PairedDevice>,
}

impl TokenStore {
    pub fn load(path: PathBuf) -> Self {
        let entries = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let mut parts = line.trim().splitn(3, ' ');
                let (id, token) = (from_hex(parts.next()?)?, from_hex(parts.next()?)?);
                // Files written before names were recorded have none.
                let name = parts.next().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("unnamed computer");
                Some(PairedDevice { id, token, name: name.to_owned() })
            })
            .collect();
        Self { path, entries }
    }

    /// Picks up changes made by another program.
    pub fn reload(&mut self) {
        *self = Self::load(std::mem::take(&mut self.path));
    }

    pub fn get(&self, peer: &Id) -> Option<&Token> {
        self.entries.iter().find(|device| device.id == *peer).map(|device| &device.token)
    }

    pub fn devices(&self) -> &[PairedDevice] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn save(&self) -> std::io::Result<()> {
        let text: String = self
            .entries
            .iter()
            .map(|d| format!("{} {} {}\n", to_hex(&d.id), to_hex(&d.token), d.name.replace(['\n', '\r'], " ")))
            .collect();
        write_private(&self.path, &text)
    }

    /// Remembers `peer`, replacing any earlier pairing with it. Other changes made to the file
    /// in the meantime are kept.
    pub fn insert(&mut self, peer: Id, token: Token, name: &str) -> std::io::Result<()> {
        self.reload();
        self.entries.retain(|device| device.id != peer);
        self.entries.push(PairedDevice { id: peer, token, name: name.to_owned() });
        self.save()
    }

    /// Forgets `peer`. Returns whether it was paired.
    pub fn remove(&mut self, peer: &Id) -> std::io::Result<bool> {
        self.reload();
        let before = self.entries.len();
        self.entries.retain(|device| device.id != *peer);
        let removed = self.entries.len() != before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn challenge() -> AuthChallenge {
        AuthChallenge { required: true, server_id: [7; 16], nonce: [9; 16], name: "host-pc".into() }
    }

    #[test]
    fn messages_roundtrip() {
        let c = challenge();
        assert_eq!(AuthChallenge::deserialize(&c.serialize()).unwrap(), c);
        let r = AuthResponse { method: AuthMethod::Token, client_id: [3; 16], mac: [5; 32], name: "Caio's laptop".into() };
        assert_eq!(AuthResponse::deserialize(&r.serialize()).unwrap(), r);
        // Names are cut to a sane length and stripped of control characters.
        let long = AuthResponse { name: format!("bad\u{7}name{}", "x".repeat(200)), ..r.clone() };
        let back = AuthResponse::deserialize(&long.serialize()).unwrap();
        assert!(back.name.starts_with("badname") && back.name.len() <= MAX_NAME);
        assert!(AuthResponse::deserialize(&[9u8; 49]).is_err());
    }

    #[test]
    fn right_code_pairs_and_the_token_works_afterwards() {
        let (c, client) = (challenge(), [1u8; 16]);
        let proof = code_proof("123456", &c, &client);
        assert!(verify_code("123456", &c, &client, &proof));
        assert!(!verify_code("123457", &c, &client, &proof));

        // Both sides derive the same token; it answers later challenges.
        let token = derive_token("123456", &c, &client);
        let later = AuthChallenge { nonce: [42; 16], ..c.clone() };
        let proof = token_proof(&token, &later, &client);
        assert!(verify_token(&token, &later, &client, &proof));
        // A recorded answer is useless against a new challenge, another client, or another token.
        assert!(!verify_token(&token, &c, &client, &proof));
        assert!(!verify_token(&token, &later, &[2u8; 16], &proof));
        assert!(!verify_token(&[0u8; 32], &later, &client, &proof));
    }

    #[test]
    fn codes_are_six_digits_and_tolerate_formatting() {
        for _ in 0..50 {
            let code = generate_code();
            assert!(code.len() == 6 && code.chars().all(|c| c.is_ascii_digit()));
        }
        assert_eq!(normalize_code(" 123 456 "), "123456");
        assert_eq!(normalize_code("123-456"), "123456");
    }

    /// Runs the host side on a thread and the client side here, over a real socket pair.
    /// `answers` are what the "person" types for successive prompts.
    fn exchange(
        required: bool,
        paired: TokenStore,
        hosts: &mut TokenStore,
        answers: Vec<Option<&'static str>>,
        type_shown_code: bool,
    ) -> (Result<Accepted, AuthError>, Result<bool, AuthError>, TokenStore, Vec<CodePrompt>) {
        use std::net::{TcpListener, TcpStream};
        use std::sync::mpsc;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (code_tx, code_rx) = mpsc::channel::<String>();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut paired = paired;
            let result = serve(&mut stream, required, [7; 16], "host-pc", &mut paired, |_, code| {
                code_tx.send(code.to_owned()).unwrap();
            });
            (result, paired)
        });

        let mut stream = TcpStream::connect(addr).unwrap();
        let mut prompts = Vec::new();
        let mut answers = answers.into_iter();
        let client = connect(&mut stream, [1; 16], "test-client", hosts, |prompt| {
            prompts.push(prompt);
            match answers.next() {
                Some(Some(typed)) => Some(typed.to_owned()),
                Some(None) => None,
                // Type what the host is showing.
                None if type_shown_code => Some(code_rx.recv().unwrap()),
                None => None,
            }
        });
        drop(stream);
        let (server_result, paired) = server.join().unwrap();
        (server_result, client, paired, prompts)
    }

    fn temp_store(name: &str) -> (TokenStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("pyromirror-test-{}", to_hex(&random::<8>())));
        (TokenStore::load(dir.join(name)), dir)
    }

    #[test]
    fn unknown_client_pairs_with_the_shown_code_then_connects_without_one() {
        let (paired, dir_a) = temp_store("paired-clients");
        let (mut hosts, dir_b) = temp_store("paired-hosts");

        // First contact: one wrong code, then the right one.
        let (server, client, paired, prompts) = exchange(true, paired, &mut hosts, vec![Some("000000")], true);
        assert_eq!(server.unwrap(), Accepted { name: "test-client".into(), client_id: [1; 16] });
        assert!(client.unwrap(), "a new pairing should be reported");
        let fresh = |wrong_attempts| CodePrompt { wrong_attempts, pairing_revoked: false };
        assert_eq!(prompts, [fresh(0), fresh(1)]);
        assert_eq!(paired.len(), 1);
        // Each side recorded what the other calls itself.
        assert_eq!(paired.devices()[0].name, "test-client");
        assert_eq!(hosts.devices()[0].name, "host-pc");

        // Second contact: no prompt at all.
        let (server, client, paired, prompts) = exchange(true, paired, &mut hosts, vec![], false);
        assert!(server.is_ok());
        assert!(!client.unwrap());
        assert!(prompts.is_empty());

        // The host removes the client. The client still offers its token, is told (through the
        // prompt) that the pairing was revoked, and can pair again.
        let mut paired = paired;
        assert!(paired.remove(&[1; 16]).unwrap());
        assert!(!paired.remove(&[1; 16]).unwrap());
        let (server, client, paired, prompts) = exchange(true, paired, &mut hosts, vec![], true);
        assert!(server.is_ok() && client.unwrap());
        assert_eq!(prompts, [CodePrompt { wrong_attempts: 0, pairing_revoked: true }]);
        assert_eq!(paired.len(), 1);

        // Removed again, and this time the person gives up: the stale token must not linger, so
        // the next attempt is an ordinary first-time pairing.
        let mut paired = paired;
        paired.remove(&[1; 16]).unwrap();
        let (_, client, paired, _) = exchange(true, paired, &mut hosts, vec![None], false);
        assert!(matches!(client, Err(AuthError::Cancelled)));
        assert!(hosts.is_empty());
        let (_, client, _, prompts) = exchange(true, paired, &mut hosts, vec![], true);
        assert!(client.unwrap());
        assert_eq!(prompts, [fresh(0)]);
        let dir_c = dir_a.clone();

        for dir in [dir_a, dir_b, dir_c] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn wrong_codes_run_out_and_cancelling_stops() {
        let (paired, dir_a) = temp_store("paired-clients");
        let (mut hosts, dir_b) = temp_store("paired-hosts");

        let wrong = vec![Some("111111"), Some("222222"), Some("333333")];
        let (server, client, paired, prompts) = exchange(true, paired, &mut hosts, wrong, false);
        assert!(matches!(server, Err(AuthError::WrongCode)));
        assert!(matches!(client, Err(AuthError::WrongCode)));
        assert_eq!(prompts.iter().map(|p| p.wrong_attempts).collect::<Vec<_>>(), [0, 1, 2]);
        assert!(paired.is_empty() && hosts.is_empty());

        let (server, client, paired, _) = exchange(true, paired, &mut hosts, vec![None], false);
        assert!(server.is_err());
        assert!(matches!(client, Err(AuthError::Cancelled)));
        assert!(paired.is_empty());

        for dir in [dir_a, dir_b] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn open_hosts_let_everyone_in() {
        let (paired, dir_a) = temp_store("paired-clients");
        let (mut hosts, dir_b) = temp_store("paired-hosts");
        let (server, client, paired, prompts) = exchange(false, paired, &mut hosts, vec![], false);
        assert!(server.is_ok() && !client.unwrap());
        assert!(prompts.is_empty() && paired.is_empty());
        for dir in [dir_a, dir_b] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn store_persists_and_replaces() {
        let dir = std::env::temp_dir().join(format!("pyromirror-test-{}", to_hex(&random::<8>())));
        let path = dir.join("paired");
        let mut store = TokenStore::load(path.clone());
        assert!(store.is_empty());
        store.insert([1; 16], [2; 32], "old name").unwrap();
        store.insert([3; 16], [4; 32], "living room pc").unwrap();
        store.insert([1; 16], [5; 32], "caio's laptop").unwrap();

        let mut reloaded = TokenStore::load(path.clone());
        assert_eq!(reloaded.len(), 2);
        assert_eq!(reloaded.get(&[1; 16]), Some(&[5; 32]));
        assert_eq!(reloaded.get(&[9; 16]), None);
        assert_eq!(reloaded.devices().iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["living room pc", "caio's laptop"]);

        // Changes made through another handle (the launcher, while the server runs) are seen
        // after a reload and are not undone by a later insert.
        let mut other = TokenStore::load(path.clone());
        assert!(other.remove(&[3; 16]).unwrap());
        reloaded.insert([8; 16], [9; 32], "new").unwrap();
        assert_eq!(reloaded.len(), 2);
        assert_eq!(reloaded.get(&[3; 16]), None);

        // Files from before names were stored still load.
        std::fs::write(&path, format!("{} {}\n", to_hex(&[6u8; 16]), to_hex(&[7u8; 32]))).unwrap();
        assert_eq!(TokenStore::load(path).devices()[0].name, "unnamed computer");

        let id_path = dir.join("id");
        assert_eq!(local_id(&id_path), local_id(&id_path));
        let _ = std::fs::remove_dir_all(dir);
    }
}
