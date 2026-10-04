//! Pairing: proving that a client is allowed to connect.
//!
//! The host shows a short pairing code. The first time a client connects, the person types that
//! code; both sides then derive a long-lived token from it and remember each other, so later
//! connections need no code. Neither the code nor the token ever crosses the network: each
//! connection answers a fresh random challenge with an HMAC keyed by one of them.
//!
//! This keeps strangers out. It is not encryption: the session itself is still readable by
//! anyone on the path, and someone who records a first-time pairing can guess a 6-digit code
//! offline. Pair on a network you trust.

use std::path::{Path, PathBuf};

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::ProtoError;

pub const MSG_TYPE_AUTH_CHALLENGE: u32 = 2;
pub const MSG_TYPE_AUTH_RESPONSE: u32 = 3;
pub const MSG_TYPE_AUTH_RESULT: u32 = 4;

pub type Id = [u8; 16];
pub type Token = [u8; 32];

/// Sent by the server right after the client's hello.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthChallenge {
    /// False if the host accepts anyone.
    pub required: bool,
    /// Stable identity of the host, so a client recognises it under a new address.
    pub server_id: Id,
    pub nonce: [u8; 16],
}

impl AuthChallenge {
    pub const SIZE: usize = 33;

    pub fn serialize(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0] = self.required as u8;
        buf[1..17].copy_from_slice(&self.server_id);
        buf[17..33].copy_from_slice(&self.nonce);
        buf
    }

    pub fn deserialize(buf: &[u8]) -> Result<Self, ProtoError> {
        if buf.len() < Self::SIZE {
            return Err(ProtoError::BufferTooSmall { required: Self::SIZE, provided: buf.len() });
        }
        Ok(Self {
            required: buf[0] != 0,
            server_id: buf[1..17].try_into().unwrap(),
            nonce: buf[17..33].try_into().unwrap(),
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
}

impl AuthResponse {
    pub const SIZE: usize = 49;

    pub fn serialize(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0] = self.method as u8;
        buf[1..17].copy_from_slice(&self.client_id);
        buf[17..49].copy_from_slice(&self.mac);
        buf
    }

    pub fn deserialize(buf: &[u8]) -> Result<Self, ProtoError> {
        if buf.len() < Self::SIZE {
            return Err(ProtoError::BufferTooSmall { required: Self::SIZE, provided: buf.len() });
        }
        let method = match buf[0] {
            0 => AuthMethod::None,
            1 => AuthMethod::Code,
            2 => AuthMethod::Token,
            other => return Err(ProtoError::UnknownMessageType(other as u32)),
        };
        Ok(Self { method, client_id: buf[1..17].try_into().unwrap(), mac: buf[17..49].try_into().unwrap() })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthResult {
    Ok = 0,
    WrongCode = 1,
    /// The client is not (or no longer) paired and sent no code.
    NotPaired = 2,
}

impl AuthResult {
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => AuthResult::Ok,
            1 => AuthResult::WrongCode,
            _ => AuthResult::NotPaired,
        }
    }
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

/// Tokens of the peers this installation is paired with, one `<peer id> <token>` per line.
pub struct TokenStore {
    path: PathBuf,
    entries: Vec<(Id, Token)>,
}

impl TokenStore {
    pub fn load(path: PathBuf) -> Self {
        let entries = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let (id, token) = line.trim().split_once(' ')?;
                Some((from_hex(id)?, from_hex(token.trim())?))
            })
            .collect();
        Self { path, entries }
    }

    pub fn get(&self, peer: &Id) -> Option<&Token> {
        self.entries.iter().find(|(id, _)| id == peer).map(|(_, token)| token)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Remembers `peer`, replacing any earlier pairing with it.
    pub fn insert(&mut self, peer: Id, token: Token) -> std::io::Result<()> {
        self.entries.retain(|(id, _)| *id != peer);
        self.entries.push((peer, token));
        let text: String =
            self.entries.iter().map(|(id, token)| format!("{} {}\n", to_hex(id), to_hex(token))).collect();
        write_private(&self.path, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn challenge() -> AuthChallenge {
        AuthChallenge { required: true, server_id: [7; 16], nonce: [9; 16] }
    }

    #[test]
    fn messages_roundtrip() {
        let c = challenge();
        assert_eq!(AuthChallenge::deserialize(&c.serialize()).unwrap(), c);
        let r = AuthResponse { method: AuthMethod::Token, client_id: [3; 16], mac: [5; 32] };
        assert_eq!(AuthResponse::deserialize(&r.serialize()).unwrap(), r);
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

    #[test]
    fn store_persists_and_replaces() {
        let dir = std::env::temp_dir().join(format!("pyromirror-test-{}", to_hex(&random::<8>())));
        let path = dir.join("paired");
        let mut store = TokenStore::load(path.clone());
        assert!(store.is_empty());
        store.insert([1; 16], [2; 32]).unwrap();
        store.insert([3; 16], [4; 32]).unwrap();
        store.insert([1; 16], [5; 32]).unwrap();

        let reloaded = TokenStore::load(path);
        assert_eq!(reloaded.len(), 2);
        assert_eq!(reloaded.get(&[1; 16]), Some(&[5; 32]));
        assert_eq!(reloaded.get(&[9; 16]), None);

        let id_path = dir.join("id");
        assert_eq!(local_id(&id_path), local_id(&id_path));
        let _ = std::fs::remove_dir_all(dir);
    }
}
