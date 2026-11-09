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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Input {
    /// Share this machine's mouse and keyboard with the peer.
    pub enabled: bool,
    /// Which side of this screen the peer's screen sits on.
    pub peer_side: Side,
    /// Send Command as Ctrl (and Control as Super), so Cmd+C copies on Linux.
    pub swap_cmd_ctrl: bool,
    /// Pointer speed on the peer (1.0 = one Mac point per peer pixel).
    pub speed: f64,
}

impl Default for Input {
    fn default() -> Self {
        Self { enabled: true, peer_side: Side::Left, swap_cmd_ctrl: true, speed: 1.0 }
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
        }
    }
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
