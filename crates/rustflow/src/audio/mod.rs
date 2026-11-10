//! Play the Linux machine's audio on the Mac.
//!
//! Linux adds a virtual output, "Mac (RustFlow)". Whatever plays there is
//! captured as raw 48 kHz stereo 16-bit PCM and streamed to the Mac over its
//! own TCP connection (port + 1, so audio never delays keyboard and mouse).
//! The Mac plays it on its current output device.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;

use crate::net;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
pub use linux::{route_to_mac, run};
#[cfg(target_os = "macos")]
pub use macos::run;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;

const RETRY_AFTER: Duration = Duration::from_secs(3);

/// Shared on/off switch for playing on the Mac (Linux routes audio with
/// `route_to_mac` instead).
#[derive(Clone)]
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub struct Audio(Arc<AtomicBool>);

impl Audio {
    pub fn new(enabled: bool) -> Self {
        Self(Arc::new(AtomicBool::new(enabled)))
    }

    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn enabled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn set_enabled(&self, on: bool) {
        self.0.store(on, Ordering::Relaxed);
    }
}

/// The audio port is the main port + 1.
pub fn audio_addr(addr: &str) -> Result<String> {
    net::side_addr(addr, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_port_is_next_port() {
        assert_eq!(audio_addr("0.0.0.0:24801").unwrap(), "0.0.0.0:24802");
        assert_eq!(audio_addr("linux-box:24801").unwrap(), "linux-box:24802");
        assert!(audio_addr("nohost").is_err());
    }
}
