//! Platform clipboard backends behind one small interface.

use std::sync::Arc;

use anyhow::Result;
use rustflow_proto::FileInfo;
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::files::FileBroker;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_paste;
#[cfg(target_os = "macos")]
pub use macos_paste::withdraw as withdraw_peer_files;

/// What is on a clipboard.
#[derive(Debug, Clone)]
pub enum ClipEvent {
    Text(String),
    /// Files copied on one machine; content is fetched when pasted.
    Files { id: u64, files: Vec<FileInfo> },
}

/// Handle shared by every network connection. It stays valid while the
/// backend behind it restarts.
#[derive(Clone)]
pub struct Clip {
    /// What the user copied on this machine.
    pub changes: broadcast::Sender<ClipEvent>,
    /// What was copied on the peer, to put on this machine's clipboard.
    pub set: mpsc::UnboundedSender<ClipEvent>,
    pub files: FileBroker,
}

/// The backend's side of a [`Clip`]; handed to each backend start in turn.
#[derive(Clone)]
pub struct Ports {
    changes: broadcast::Sender<ClipEvent>,
    set_rx: Arc<Mutex<mpsc::UnboundedReceiver<ClipEvent>>>,
    files: FileBroker,
}

pub fn channels(files: FileBroker) -> (Clip, Ports) {
    let (changes, _) = broadcast::channel(16);
    let (set, set_rx) = mpsc::unbounded_channel();
    let clip = Clip { changes: changes.clone(), set, files: files.clone() };
    let ports = Ports { changes, set_rx: Arc::new(Mutex::new(set_rx)), files };
    (clip, ports)
}

/// Start the clipboard backend for this OS.
/// The returned task only finishes if the backend fails.
pub async fn start(ports: &Ports) -> Result<JoinHandle<Result<()>>> {
    // Drop peer copies that queued up while the backend was down.
    {
        let mut rx = ports.set_rx.lock().await;
        while rx.try_recv().is_ok() {}
    }

    #[cfg(target_os = "macos")]
    let task = macos::spawn(ports.changes.clone(), ports.set_rx.clone(), ports.files.clone());
    #[cfg(target_os = "linux")]
    let task = linux::spawn(ports.changes.clone(), ports.set_rx.clone(), ports.files.clone()).await?;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    compile_error!("rustflow supports macOS and Linux only");

    Ok(task)
}

/// Stops a value we just received from the peer from bouncing straight back.
#[derive(Default)]
pub struct EchoGuard {
    from_peer: Option<String>,
}

impl EchoGuard {
    pub fn received(&mut self, text: &str) {
        self.from_peer = Some(text.to_owned());
    }

    /// True if a local clipboard change should be sent to the peer.
    pub fn should_send(&self, text: &str) -> bool {
        !text.is_empty() && self.from_peer.as_deref() != Some(text)
    }
}
