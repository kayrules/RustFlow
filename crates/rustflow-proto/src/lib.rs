//! Wire protocol shared by every rustflow peer.
//!
//! Each frame is a 4-byte big-endian length followed by a JSON-encoded
//! [`Message`]. JSON keeps the protocol easy to debug; it can move to a
//! binary encoding later without touching the transport.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 4;
pub const DEFAULT_PORT: u16 = 24801;
/// Largest frame we accept. Clipboard text above this is dropped.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// First frame sent by the connecting side.
    Hello {
        version: u32,
        host: String,
        token: String,
    },
    /// Reply from the listening side once the token checks out.
    Welcome { host: String },
    /// The sender's clipboard now holds this text.
    ClipboardText { text: String },
    /// The sender copied files. Only names are sent; content moves on paste.
    ClipboardFiles { id: u64, files: Vec<FileInfo> },
    /// Copy the files of offer `id` into `dest` on the requester (who pastes).
    FetchFiles { id: u64, dest: String },
    /// The files of offer `id` are now in the requested `dest`.
    FilesReady { id: u64 },
    FilesFailed { id: u64, error: String },
    /// Turn the receiver's screen on and unlock it (if it allows that).
    Wake,
    /// Sent by a machine that can be controlled: its screen size in pixels.
    Screen { width: u32, height: u32 },
    /// The pointer moved onto the receiver's screen at (x, y), each 0..1.
    Enter { x: f64, y: f64 },
    /// The pointer went back; release every held key and button.
    Leave,
    /// Absolute pointer position on the receiver's screen, each 0..1.
    PointerTo { x: f64, y: f64 },
    Button { button: MouseButton, pressed: bool },
    /// Scroll in wheel notches; positive `dy` scrolls up, positive `dx` right.
    Scroll { dx: f64, dy: f64 },
    /// Linux evdev key code (KEY_*).
    Key { code: u16, pressed: bool },
    Ping,
    Pong,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    /// Base name, as it should appear where it is pasted.
    pub name: String,
    /// Size in bytes (0 for folders).
    pub size: u64,
    pub is_dir: bool,
    /// Absolute path on the machine that copied it (for pulling with scp).
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("Message always serializes")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let msgs = [
            Message::Hello {
                version: PROTOCOL_VERSION,
                host: "mac".into(),
                token: "secret".into(),
            },
            Message::Welcome { host: "linux-box".into() },
            Message::ClipboardText { text: "hai 👋\nline 2".into() },
            Message::ClipboardFiles {
                id: 7,
                files: vec![FileInfo { name: "a b.pdf".into(), size: 10, is_dir: false, path: "/x/a b.pdf".into() }],
            },
            Message::FetchFiles { id: 7, dest: "/tmp/x".into() },
            Message::FilesReady { id: 7 },
            Message::FilesFailed { id: 7, error: "nope".into() },
            Message::Wake,
            Message::Screen { width: 1920, height: 1200 },
            Message::Enter { x: 1.0, y: 0.5 },
            Message::PointerTo { x: 0.25, y: 0.75 },
            Message::Button { button: MouseButton::Back, pressed: true },
            Message::Scroll { dx: 0.0, dy: -1.5 },
            Message::Key { code: 30, pressed: false },
            Message::Leave,
            Message::Ping,
            Message::Pong,
        ];
        for m in msgs {
            assert_eq!(Message::decode(&m.encode()).unwrap(), m);
        }
    }
}
