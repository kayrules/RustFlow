//! State shared between the network/clipboard core and the tray UI.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, watch};

/// A wake request is forgotten if the peer doesn't show up within this time.
const WAKE_VALID_FOR: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ClipState {
    #[default]
    Starting,
    Ready,
    Error(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Host name of the connected peer.
    pub peer: Option<String>,
    pub clipboard: ClipState,
    pub sync_enabled: bool,
    /// Mouse & keyboard sharing state, ready to show.
    pub input: String,
    /// Audio forwarding state, ready to show.
    pub audio: String,
}

/// What the tray shows, reduced to one state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Light {
    Connected,
    Waiting,
    Error,
    Paused,
}

impl Status {
    pub fn light(&self) -> Light {
        match (&self.clipboard, &self.peer) {
            (ClipState::Error(_), _) => Light::Error,
            _ if !self.sync_enabled => Light::Paused,
            (ClipState::Ready, Some(_)) => Light::Connected,
            _ => Light::Waiting,
        }
    }

    pub fn peer_line(&self, peer_label: &str) -> String {
        match &self.peer {
            Some(host) => format!("Connected to {host}"),
            None => format!("Waiting for {peer_label}…"),
        }
    }

    pub fn clipboard_line(&self) -> String {
        match &self.clipboard {
            ClipState::Starting => "Clipboard: starting…".into(),
            ClipState::Ready => "Clipboard: ready".into(),
            ClipState::Error(e) => format!("Clipboard error: {}", short(e)),
        }
    }
}

fn short(s: &str) -> String {
    let first = s.lines().next().unwrap_or(s);
    if first.chars().count() > 70 {
        format!("{}…", first.chars().take(70).collect::<String>())
    } else {
        first.to_owned()
    }
}

#[derive(Clone)]
pub struct Shared {
    status: Arc<watch::Sender<Status>>,
    sync: Arc<AtomicBool>,
    wake_at: Arc<Mutex<Option<Instant>>>,
    wake_signal: Arc<Notify>,
    /// Open peer connections; a reconnect can overlap the old one closing.
    connections: Arc<AtomicUsize>,
    /// Whether the peer may unlock this machine's screen.
    pub allow_remote_unlock: bool,
}

impl Default for Shared {
    fn default() -> Self {
        let status = Status {
            sync_enabled: true,
            input: "Mouse & keyboard: starting…".into(),
            audio: "Audio: starting…".into(),
            ..Default::default()
        };
        Self {
            status: Arc::new(watch::channel(status).0),
            sync: Arc::new(AtomicBool::new(true)),
            wake_at: Arc::default(),
            wake_signal: Arc::default(),
            connections: Arc::default(),
            allow_remote_unlock: false,
        }
    }
}

impl Shared {
    pub fn subscribe(&self) -> watch::Receiver<Status> {
        self.status.subscribe()
    }

    pub fn set_peer(&self, peer: Option<String>) {
        self.status.send_if_modified(|s| replace(&mut s.peer, peer));
    }

    pub fn peer_connected(&self, host: String) {
        self.connections.fetch_add(1, Ordering::Relaxed);
        self.set_peer(Some(host));
    }

    /// Only shows "waiting" once no connection is left.
    pub fn peer_disconnected(&self) {
        if self.connections.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.set_peer(None);
        }
    }

    pub fn set_audio(&self, line: String) {
        self.status.send_if_modified(|s| replace(&mut s.audio, line));
    }

    pub fn set_input(&self, line: String) {
        self.status.send_if_modified(|s| replace(&mut s.input, line));
    }

    pub fn set_clipboard(&self, state: ClipState) {
        self.status.send_if_modified(|s| replace(&mut s.clipboard, state));
    }

    pub fn sync_enabled(&self) -> bool {
        self.sync.load(Ordering::Relaxed)
    }

    pub fn set_sync(&self, on: bool) {
        self.sync.store(on, Ordering::Relaxed);
        self.status.send_if_modified(|s| replace(&mut s.sync_enabled, on));
    }

    /// Ask the peer to turn its screen on, now or as soon as it connects.
    pub fn request_wake(&self) {
        *self.wake_at.lock().unwrap() = Some(Instant::now());
        self.wake_signal.notify_waiters();
    }

    /// Resolves when a wake is requested.
    pub async fn wake_requested(&self) {
        self.wake_signal.notified().await
    }

    /// Consume a pending, still-fresh wake request.
    pub fn take_wake(&self) -> bool {
        self.wake_at
            .lock()
            .unwrap()
            .take()
            .is_some_and(|at| at.elapsed() < WAKE_VALID_FOR)
    }
}

fn replace<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        false
    } else {
        *slot = value;
        true
    }
}
