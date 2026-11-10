//! Mouse and keyboard sharing.
//!
//! macOS captures (event tap) and decides when the pointer crosses to the
//! peer; Linux injects through /dev/uinput, which works everywhere GNOME
//! shows a pointer, including the lock screen.

use std::sync::Arc;

use rustflow_proto::Message;

use crate::config::Config;
use crate::status::Shared;

#[cfg(target_os = "macos")]
mod keymap;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod shortcuts;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;

/// Handle used by the network relay. Cheap to clone.
#[derive(Clone)]
pub struct Input(Arc<platform::State>);

impl Input {
    /// Set up capture/injection for this OS. Problems are reported in the
    /// status line rather than returned, so clipboard sync keeps working.
    pub fn start(cfg: &Config, shared: &Shared) -> Self {
        Self(platform::start(cfg, shared))
    }

    /// Messages to send right after connecting (e.g. our screen size).
    pub fn on_connect(&self) -> Vec<Message> {
        self.0.on_connect()
    }

    /// Handle an input-related message from the peer.
    pub fn on_message(&self, msg: &Message) {
        self.0.on_message(msg)
    }

    pub fn on_disconnect(&self) {
        self.0.on_disconnect()
    }

    /// Next captured event to forward to the peer. Never resolves on
    /// machines that only receive input.
    pub async fn next_outgoing(&self) -> Option<Message> {
        self.0.next_outgoing().await
    }

    /// Switch between Mac-style and Linux-style keys (capturing side only).
    #[cfg(target_os = "macos")]
    pub fn set_keyboard_style(&self, style: crate::config::KeyboardStyle) {
        self.0.set_keyboard_style(style)
    }

    /// Turn capture on or off (only the capturing side has a toggle).
    #[cfg(target_os = "macos")]
    pub fn set_enabled(&self, on: bool) {
        self.0.set_enabled(on)
    }

    /// Where the peer's screen window is while it has focus (None otherwise).
    /// Clicks on the picture and all keys then go to the peer.
    #[cfg(target_os = "macos")]
    pub fn set_view_area(&self, area: Option<ViewArea>) {
        self.0.set_view_area(area)
    }
}

/// The peer's screen window, in Mac screen points (origin top left).
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub struct ViewArea {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// Size of the picture shown in it, in peer pixels.
    pub video_width: f64,
    pub video_height: f64,
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
impl ViewArea {
    /// The picture's rectangle (x, y, width, height): fitted, with bars on two sides.
    fn picture(self) -> (f64, f64, f64, f64) {
        let scale = (self.width / self.video_width).min(self.height / self.video_height);
        let (w, h) = (self.video_width * scale, self.video_height * scale);
        (self.x + (self.width - w) / 2.0, self.y + (self.height - h) / 2.0, w, h)
    }

    /// Position on the peer screen (each 0..1) of a Mac point, if it is on the picture.
    pub fn to_peer(self, px: f64, py: f64) -> Option<(f64, f64)> {
        let (x, y, w, h) = self.picture();
        let (u, v) = ((px - x) / w, (py - y) / h);
        ((0.0..1.0).contains(&u) && (0.0..1.0).contains(&v)).then_some((u, v))
    }

    /// Like `to_peer`, but points off the picture stick to its edge (for drags).
    pub fn to_peer_clamped(self, px: f64, py: f64) -> (f64, f64) {
        let (x, y, w, h) = self.picture();
        (((px - x) / w).clamp(0.0, 1.0), ((py - y) / h).clamp(0.0, 1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_area_letterboxes() {
        // A 16:10 picture in a wide window: bars left and right.
        let a = ViewArea { x: 100.0, y: 50.0, width: 1000.0, height: 500.0, video_width: 1920.0, video_height: 1200.0 };
        assert_eq!(a.to_peer(600.0, 50.0), Some((0.5, 0.0)));
        assert_eq!(a.to_peer(150.0, 300.0), None); // in the left bar
        assert_eq!(a.to_peer_clamped(150.0, 300.0), (0.0, 0.5));
        assert_eq!(a.to_peer(600.0, 550.0), None); // below the window
    }
}

/// True for messages this module consumes.
pub fn is_input_message(msg: &Message) -> bool {
    matches!(
        msg,
        Message::Screen { .. }
            | Message::Enter { .. }
            | Message::Leave
            | Message::PointerTo { .. }
            | Message::Button { .. }
            | Message::Scroll { .. }
            | Message::Key { .. }
    )
}
