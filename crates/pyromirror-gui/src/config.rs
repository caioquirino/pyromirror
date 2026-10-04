//! Settings shown in the launcher, saved as JSON in the user's config directory.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Tab {
    #[default]
    Connect,
    Host,
    Settings,
}

/// The pages of the Settings tab, one per situation the settings apply to.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SettingsPage {
    /// When other computers connect to this one.
    #[default]
    Sharing,
    /// When this computer controls another one.
    Connecting,
    /// The program itself, whichever way it is used.
    General,
}

/// A computer this one can control, as listed on the Connect tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Computer {
    /// What the computer calls itself, learned when it was added.
    pub name: String,
    pub address: String,
    /// Hex id of the host, which ties this entry to its pairing. Empty for entries carried over
    /// from the old "recent addresses" list.
    pub id: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub tab: Tab,
    pub settings_page: SettingsPage,

    // Connect
    pub computers: Vec<Computer>,
    /// The computer the viewer was last pointed at, so a launcher opened while the viewer is
    /// still running can say what it is connected to.
    pub last_target: Computer,
    /// Superseded by `computers`; only read to carry old entries over.
    pub recent: Vec<String>,
    pub fullscreen: bool,
    pub lock_mouse: bool,
    pub play_audio: bool,
    /// Decode straight into the window's textures where the system can.
    pub zero_copy_display: bool,

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
    /// Encode straight from the captured texture where the system can.
    pub zero_copy: bool,
    /// Windows: run ahead of other programs, so that a game does not hold the stream up.
    pub priority: bool,

    // Startup
    /// Start in the tray when the user logs in.
    pub autostart: bool,
    /// Start sharing as soon as the tray agent starts. Only set after a permission check.
    pub auto_share: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tab: Tab::Connect,
            settings_page: SettingsPage::Sharing,
            computers: Vec::new(),
            last_target: Computer::default(),
            recent: Vec::new(),
            fullscreen: false,
            lock_mouse: false,
            play_audio: true,
            zero_copy_display: true,
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
            zero_copy: true,
            priority: true,
            autostart: false,
            auto_share: false,
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
        let mut config: Self = path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        config.migrate();
        config
    }

    /// Turns the old list of recent addresses into computers.
    fn migrate(&mut self) {
        for address in std::mem::take(&mut self.recent) {
            if !self.computers.iter().any(|c| c.address == address) {
                self.computers.push(Computer { name: address.clone(), address, id: String::new() });
            }
        }
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

    /// Adds a computer, or updates the entry for the same host (or the same address) so a
    /// computer never appears twice.
    pub fn remember(&mut self, computer: Computer) {
        let same = |c: &Computer| (!computer.id.is_empty() && c.id == computer.id) || c.address == computer.address;
        match self.computers.iter_mut().find(|c| same(c)) {
            Some(existing) => *existing = computer,
            None => self.computers.push(computer),
        }
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
        if !self.zero_copy {
            args.push("--no-zero-copy".into());
        }
        if !self.priority {
            args.push("--no-priority".into());
        }
        args
    }

    pub fn client_args(&self, address: &str) -> Vec<String> {
        let mut args = vec![address.trim().to_owned()];
        if self.fullscreen {
            args.push("--fullscreen".into());
        }
        if self.lock_mouse {
            args.push("--lock-mouse".into());
        }
        if !self.play_audio {
            args.push("--no-audio".into());
        }
        if !self.zero_copy_display {
            args.push("--no-zero-copy".into());
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
        let mut config = Config::default();
        assert_eq!(config.client_args(" 10.0.0.2 "), ["10.0.0.2"]);
        config.fullscreen = true;
        config.lock_mouse = true;
        config.play_audio = false;
        config.share_audio = false;
        config.chroma_444 = false;
        assert_eq!(config.client_args("10.0.0.2"), ["10.0.0.2", "--fullscreen", "--lock-mouse", "--no-audio"]);
        let server = config.server_args();
        assert!(server.contains(&"--no-audio".to_string()) && !server.contains(&"--no-input".to_string()));
        assert!(!server.contains(&"--no-pairing".to_string()));
        assert!(server.windows(2).any(|w| w == ["--chroma", "420"]));
    }

    #[test]
    fn computers_are_not_duplicated_and_old_recents_carry_over() {
        let mut config: Config = serde_json::from_str(r#"{"recent": ["10.0.0.2", "10.0.0.3"]}"#).unwrap();
        config.migrate();
        assert_eq!(config.computers.len(), 2);
        assert!(config.recent.is_empty());
        assert_eq!(config.computers[0].name, "10.0.0.2");

        // Adding the computer at a known address fills in its name and id in place.
        let pc = Computer { name: "CAIO-PC".into(), address: "10.0.0.2".into(), id: "ab".into() };
        config.remember(pc.clone());
        assert_eq!(config.computers.len(), 2);
        assert_eq!(config.computers[0], pc);
        // The same host under a new address replaces the entry rather than adding one.
        config.remember(Computer { address: "10.0.0.9".into(), ..pc.clone() });
        assert_eq!(config.computers.len(), 2);
        assert_eq!(config.computers[0].address, "10.0.0.9");
        config.remember(Computer { name: "other".into(), address: "10.0.0.7".into(), id: "cd".into() });
        assert_eq!(config.computers.len(), 3);
    }
}
