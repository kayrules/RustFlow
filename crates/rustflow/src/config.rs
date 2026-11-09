//! `~/.config/rustflow/config.toml`

use std::path::PathBuf;

use anyhow::{Context, Result};
use rustflow_proto::DEFAULT_PORT;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Wait for the peer to connect (the machine that may sleep, e.g. Ubuntu).
    Serve,
    /// Connect to the peer and keep retrying.
    Connect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wol {
    /// MAC address of the peer's network card, e.g. "02:00:5e:10:20:30".
    pub mac: String,
    /// Broadcast address of the LAN, e.g. "192.168.1.255".
    pub broadcast: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Right,
}

/// How Mac keys behave on Linux.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyboardStyle {
    /// Mac shortcuts do what they do on a Mac (⌘C copies, ⌘Tab switches apps).
    #[default]
    Mac,
    /// Keys map by position like a PC keyboard (⌘ = Super, ⌥ = Alt).
    Linux,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Input {
    /// Share this machine's mouse and keyboard with the peer.
    pub enabled: bool,
    /// Which side of this screen the peer's screen sits on.
    pub peer_side: Side,
    pub keyboard: KeyboardStyle,
    /// Pointer speed on the peer (1.0 = one Mac point per peer pixel).
    pub speed: f64,
}

impl Default for Input {
    fn default() -> Self {
        Self { enabled: true, peer_side: Side::Left, keyboard: KeyboardStyle::Mac, speed: 1.0 }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Files {
    /// SSH host of the peer (as used with `ssh`), so pasted files can be
    /// copied with scp. Only the machine that can SSH to the other needs it.
    pub ssh_host: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    /// Mac: play the Linux machine's audio. Linux: offer the "Mac (RustFlow)" output.
    pub enabled: bool,
}

impl Default for Audio {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub role: Role,
    /// host or host:port of the peer (used when role = "connect").
    pub peer: Option<String>,
    /// Address to listen on (used when role = "serve").
    pub listen: String,
    /// Friendly name for the peer, shown in the menu.
    pub peer_name: Option<String>,
    pub wol: Option<Wol>,
    /// Let the peer turn this screen on and unlock it ("Wake" in its menu).
    /// Anyone holding the shared token can then unlock this machine.
    pub allow_remote_unlock: bool,
    /// Unlock this screen when the peer's pointer moves onto it.
    /// Anyone who can use the peer can then use this machine without a password.
    pub unlock_on_enter: bool,
    pub input: Input,
    pub files: Files,
    pub audio: Audio,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            role: if cfg!(target_os = "macos") { Role::Connect } else { Role::Serve },
            peer: None,
            listen: format!("0.0.0.0:{DEFAULT_PORT}"),
            peer_name: None,
            wol: None,
            allow_remote_unlock: false,
            unlock_on_enter: false,
            input: Input::default(),
            files: Files::default(),
            audio: Audio::default(),
        }
    }
}

/// `text` (a config file) with `[input] keyboard` set to `style`.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn with_keyboard_style(text: &str, style: KeyboardStyle) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    let value = match style {
        KeyboardStyle::Mac => "mac",
        KeyboardStyle::Linux => "linux",
    };
    doc["input"]["keyboard"] = toml_edit::value(value);
    if let Some(input) = doc["input"].as_table_mut() {
        // Replaced by `keyboard`.
        input.remove("swap_cmd_ctrl");
    }
    Ok(doc.to_string())
}

impl Config {
    pub fn path() -> PathBuf {
        crate::config_dir().join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("invalid {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// Remember the keyboard style chosen in the menu, keeping the file's comments.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn save_keyboard_style(style: KeyboardStyle) -> Result<()> {
        let path = Self::path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let updated = with_keyboard_style(&text, style).with_context(|| format!("invalid {}", path.display()))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, updated).with_context(|| format!("cannot write {}", path.display()))
    }

    /// Peer address with the default port filled in.
    pub fn peer_addr(&self) -> Option<String> {
        self.peer.as_ref().map(|p| {
            if p.contains(':') { p.clone() } else { format!("{p}:{DEFAULT_PORT}") }
        })
    }

    pub fn peer_label(&self) -> String {
        self.peer_name
            .clone()
            .or_else(|| self.peer.clone())
            .unwrap_or_else(|| "peer".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_keyboard_style_keeps_comments() {
        let text = "# my mac\nrole = \"connect\"\n\n[input]\n# side\npeer_side = \"left\"\nswap_cmd_ctrl = true\n";
        let out = with_keyboard_style(text, KeyboardStyle::Linux).unwrap();
        assert!(out.contains("# my mac") && out.contains("# side"));
        assert!(out.contains("keyboard = \"linux\"") && !out.contains("swap_cmd_ctrl"));
        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.input.keyboard, KeyboardStyle::Linux);
        assert_eq!(cfg.input.peer_side, Side::Left);
    }

    #[test]
    fn keyboard_style_is_added_when_missing() {
        let out = with_keyboard_style("role = \"connect\"\n", KeyboardStyle::Mac).unwrap();
        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.input.keyboard, KeyboardStyle::Mac);
    }
}
