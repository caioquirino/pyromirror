//! Settings shown in the launcher, saved as JSON in the user's config directory.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Tab {
    #[default]
    Connect,
    Host,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub tab: Tab,

    // Connect
    pub address: String,
    pub recent: Vec<String>,
    pub fullscreen: bool,
    pub lock_mouse: bool,
    pub play_audio: bool,

    // Host
    pub port: u16,
    pub bitrate_mbps: u32,
    pub fps: u32,
    pub scale: u32,
    pub chroma_444: bool,
    pub share_audio: bool,
    pub allow_control: bool,
    pub require_pairing: bool,
    pub mtu: u32,
    pub pace_factor: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tab: Tab::Connect,
            address: String::new(),
            recent: Vec::new(),
            fullscreen: false,
            lock_mouse: false,
            play_audio: true,
            port: 9000,
            bitrate_mbps: 250,
            fps: 60,
            scale: 1,
            chroma_444: true,
            share_audio: true,
            allow_control: true,
            require_pairing: true,
            mtu: 1400,
            pace_factor: 2.0,
        }
    }
}

fn path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }?;
    Some(base.join("pyromirror").join("settings.json"))
}

impl Config {
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = path() else { return };
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&path, serde_json::to_string_pretty(self).unwrap_or_default()));
        if let Err(e) = written {
            log::warn!("Could not save settings to {}: {}", path.display(), e);
        }
    }

    /// Moves `address` to the front of the recent list.
    pub fn remember(&mut self, address: &str) {
        self.recent.retain(|a| a != address);
        self.recent.insert(0, address.to_owned());
        self.recent.truncate(5);
    }

    pub fn server_args(&self) -> Vec<String> {
        let mut args = vec![
            "--port".into(),
            self.port.to_string(),
            "--bitrate-mbps".into(),
            self.bitrate_mbps.to_string(),
            "--fps".into(),
            self.fps.to_string(),
            "--scale".into(),
            self.scale.to_string(),
            "--chroma".into(),
            if self.chroma_444 { "444" } else { "420" }.into(),
            "--mtu".into(),
            self.mtu.to_string(),
            "--pace-factor".into(),
            format!("{:.2}", self.pace_factor),
        ];
        if !self.share_audio {
            args.push("--no-audio".into());
        }
        if !self.allow_control {
            args.push("--no-input".into());
        }
        if !self.require_pairing {
            args.push("--no-pairing".into());
        }
        args
    }

    pub fn client_args(&self) -> Vec<String> {
        let mut args = vec![self.address.trim().to_owned()];
        if self.fullscreen {
            args.push("--fullscreen".into());
        }
        if self.lock_mouse {
            args.push("--lock-mouse".into());
        }
        if !self.play_audio {
            args.push("--no-audio".into());
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_survive_partial_files() {
        let config: Config = serde_json::from_str(r#"{"fps": 120}"#).unwrap();
        assert_eq!(config.fps, 120);
        assert_eq!(config.port, 9000);
    }

    #[test]
    fn args_reflect_toggles() {
        let mut config = Config { address: " 10.0.0.2 ".into(), ..Default::default() };
        assert_eq!(config.client_args(), ["10.0.0.2"]);
        config.fullscreen = true;
        config.lock_mouse = true;
        config.play_audio = false;
        config.share_audio = false;
        config.chroma_444 = false;
        assert_eq!(config.client_args(), ["10.0.0.2", "--fullscreen", "--lock-mouse", "--no-audio"]);
        let server = config.server_args();
        assert!(server.contains(&"--no-audio".to_string()) && !server.contains(&"--no-input".to_string()));
        assert!(!server.contains(&"--no-pairing".to_string()));
        assert!(server.windows(2).any(|w| w == ["--chroma", "420"]));
    }

    #[test]
    fn recent_is_deduplicated_and_capped() {
        let mut config = Config::default();
        for host in ["a", "b", "c", "a", "d", "e", "f"] {
            config.remember(host);
        }
        assert_eq!(config.recent, ["f", "e", "d", "a", "c"]);
    }
}
