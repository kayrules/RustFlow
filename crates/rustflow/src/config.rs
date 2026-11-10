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

/// Video encoder for the screen window (Linux).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoder {
    /// NVIDIA's hardware encoder when available, otherwise x264.
    #[default]
    Auto,
    /// NVIDIA GPU (nvh264enc, from gstreamer1.0-plugins-bad).
    Nvenc,
    /// CPU (x264enc, from gstreamer1.0-plugins-ugly).
    X264,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Screen {
    /// Mac: offer "Show <peer> screen" in the menu. Linux: let the Mac view this screen.
    pub enabled: bool,
    /// Linux: video bitrate in kbit/s. Higher is sharper but needs more bandwidth.
    pub bitrate_kbps: u32,
    pub encoder: Encoder,
}

impl Default for Screen {
    fn default() -> Self {
        Self { enabled: true, bitrate_kbps: 12_000, encoder: Encoder::Auto }
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
    pub screen: Screen,
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
            screen: Screen::default(),
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

/// What the Mac's Settings window edits. Empty strings mean "not set".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub struct MacSettings {
    pub peer: String,
    pub peer_name: String,
    pub input_enabled: bool,
    pub peer_side: Side,
    pub keyboard: KeyboardStyle,
    pub speed: f64,
    pub ssh_host: String,
    pub audio_enabled: bool,
    pub screen_enabled: bool,
    pub wol_mac: String,
    pub wol_broadcast: String,
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
impl MacSettings {
    pub fn from_config(c: &Config) -> Self {
        Self {
            peer: c.peer.clone().unwrap_or_default(),
            peer_name: c.peer_name.clone().unwrap_or_default(),
            input_enabled: c.input.enabled,
            peer_side: c.input.peer_side,
            keyboard: c.input.keyboard,
            speed: c.input.speed,
            ssh_host: c.files.ssh_host.clone().unwrap_or_default(),
            audio_enabled: c.audio.enabled,
            screen_enabled: c.screen.enabled,
            wol_mac: c.wol.as_ref().map(|w| w.mac.clone()).unwrap_or_default(),
            wol_broadcast: c.wol.as_ref().map(|w| w.broadcast.clone()).unwrap_or_default(),
        }
    }

    /// Trimmed copy, or the field that is wrong and why.
    pub fn validated(&self) -> Result<Self, (&'static str, String)> {
        let mut s = self.clone();
        for f in [&mut s.peer, &mut s.peer_name, &mut s.ssh_host, &mut s.wol_mac, &mut s.wol_broadcast] {
            *f = f.trim().to_owned();
        }
        if s.peer.is_empty() {
            return Err(("peer", "Enter the Linux PC's address.".into()));
        }
        if s.peer.contains(char::is_whitespace) {
            return Err(("peer", "The Linux address can't contain spaces.".into()));
        }
        if let Some((_, port)) = s.peer.rsplit_once(':') {
            if port.parse::<u16>().is_err() {
                return Err(("peer", format!("\"{port}\" is not a valid port.")));
            }
        }
        if !(0.1..=10.0).contains(&s.speed) {
            return Err(("speed", "Pointer speed must be between 0.1 and 10.".into()));
        }
        if s.ssh_host.contains(char::is_whitespace) {
            return Err(("ssh_host", "The SSH host can't contain spaces.".into()));
        }
        match (s.wol_mac.is_empty(), s.wol_broadcast.is_empty()) {
            (true, true) => {}
            (false, true) => return Err(("wol_broadcast", "Wake-on-LAN also needs the broadcast address.".into())),
            (true, false) => return Err(("wol_mac", "Wake-on-LAN also needs the MAC address.".into())),
            (false, false) => {
                let parts: Vec<&str> = s.wol_mac.split([':', '-']).collect();
                if parts.len() != 6 || parts.iter().any(|p| p.len() != 2 || u8::from_str_radix(p, 16).is_err()) {
                    return Err(("wol_mac", "The MAC address should look like a8:a1:59:2d:35:9d.".into()));
                }
                if s.wol_broadcast.parse::<std::net::Ipv4Addr>().is_err() {
                    return Err(("wol_broadcast", "The broadcast address should look like 192.168.1.255.".into()));
                }
            }
        }
        Ok(s)
    }

    /// `text` (a config file) with the fields that differ from `old` set to
    /// these values. Comments and other settings are kept.
    pub fn apply_to(&self, old: &Self, text: &str) -> Result<String> {
        let mut doc: toml_edit::DocumentMut = text.parse()?;
        let text_or_remove = |doc: &mut toml_edit::DocumentMut, table: Option<&str>, key: &str, v: &str| {
            let t = match table {
                Some(name) => doc[name].as_table_like_mut(),
                None => Some(doc.as_table_mut() as &mut dyn toml_edit::TableLike),
            };
            if v.is_empty() {
                if let Some(t) = t {
                    t.remove(key);
                }
            } else {
                match table {
                    Some(name) => doc[name][key] = toml_edit::value(v),
                    None => doc[key] = toml_edit::value(v),
                }
            }
        };
        if self.peer != old.peer {
            text_or_remove(&mut doc, None, "peer", &self.peer);
        }
        if self.peer_name != old.peer_name {
            text_or_remove(&mut doc, None, "peer_name", &self.peer_name);
        }
        if self.input_enabled != old.input_enabled {
            doc["input"]["enabled"] = toml_edit::value(self.input_enabled);
        }
        if self.peer_side != old.peer_side {
            doc["input"]["peer_side"] = toml_edit::value(match self.peer_side {
                Side::Left => "left",
                Side::Right => "right",
            });
        }
        if self.keyboard != old.keyboard {
            doc = with_keyboard_style(&doc.to_string(), self.keyboard)?.parse()?;
        }
        if self.speed != old.speed {
            doc["input"]["speed"] = toml_edit::value((self.speed * 100.0).round() / 100.0);
        }
        if self.ssh_host != old.ssh_host {
            text_or_remove(&mut doc, Some("files"), "ssh_host", &self.ssh_host);
        }
        if self.audio_enabled != old.audio_enabled {
            doc["audio"]["enabled"] = toml_edit::value(self.audio_enabled);
        }
        if self.screen_enabled != old.screen_enabled {
            doc["screen"]["enabled"] = toml_edit::value(self.screen_enabled);
        }
        if (&self.wol_mac, &self.wol_broadcast) != (&old.wol_mac, &old.wol_broadcast) {
            if self.wol_mac.is_empty() {
                doc.remove("wol");
            } else {
                doc["wol"]["mac"] = toml_edit::value(&self.wol_mac);
                doc["wol"]["broadcast"] = toml_edit::value(&self.wol_broadcast);
            }
        }
        Ok(doc.to_string())
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

    /// Save what the Settings window changed, keeping the file's comments.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn save_mac_settings(old: &MacSettings, new: &MacSettings) -> Result<()> {
        let path = Self::path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let updated = new.apply_to(old, &text).with_context(|| format!("invalid {}", path.display()))?;
        toml::from_str::<Config>(&updated).context("the new settings don't form a valid config")?;
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
    fn settings_change_only_what_changed() {
        let text = "# my mac\nrole = \"connect\"\npeer = \"10.0.0.2\" # wired\npeer_name = \"Box\"\n\n\
                    [input]\n# side\npeer_side = \"left\"\n\n[wol]\nmac = \"aa:bb:cc:dd:ee:ff\"\nbroadcast = \"10.0.0.255\"\n";
        let cfg: Config = toml::from_str(text).unwrap();
        let old = MacSettings::from_config(&cfg);
        let mut new = old.clone();
        new.peer_side = Side::Right;
        new.speed = 1.5;
        new.ssh_host = "box".into();
        new.peer_name = String::new();
        new.wol_mac = String::new();
        new.wol_broadcast = String::new();
        let out = new.validated().unwrap().apply_to(&old, text).unwrap();
        assert!(out.contains("# my mac") && out.contains("# side") && out.contains("peer = \"10.0.0.2\" # wired"));
        let saved: Config = toml::from_str(&out).unwrap();
        assert_eq!(MacSettings::from_config(&saved), new);
        assert!(!out.contains("peer_name") && !out.contains("[wol]"));
    }

    #[test]
    fn settings_are_checked() {
        let mut s = MacSettings::from_config(&Config::default());
        assert!(s.validated().is_err()); // no address
        s.peer = " 192.168.1.20 ".into();
        assert_eq!(s.validated().unwrap().peer, "192.168.1.20");
        s.wol_mac = "aa:bb".into();
        assert!(s.validated().is_err());
        s.wol_broadcast = "192.168.1.255".into();
        s.wol_mac = "aa:bb:cc:dd:ee:ff".into();
        assert!(s.validated().is_ok());
        s.peer = "host:99999".into();
        assert!(s.validated().is_err());
    }

    #[test]
    fn keyboard_style_is_added_when_missing() {
        let out = with_keyboard_style("role = \"connect\"\n", KeyboardStyle::Mac).unwrap();
        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.input.keyboard, KeyboardStyle::Mac);
    }
}
