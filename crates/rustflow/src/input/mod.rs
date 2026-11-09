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
