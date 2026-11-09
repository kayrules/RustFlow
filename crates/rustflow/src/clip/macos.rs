//! macOS clipboard: poll NSPasteboard's change count; text through arboard,
//! copied Finder files through NSPasteboard file URLs.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL};
use objc2_foundation::NSURL;
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, info};

use super::{ClipEvent, EchoGuard};
use crate::files::FileBroker;

const POLL_EVERY: Duration = Duration::from_millis(300);

pub fn spawn(
    changes: broadcast::Sender<ClipEvent>,
    set_rx: Arc<Mutex<mpsc::UnboundedReceiver<ClipEvent>>>,
    files: FileBroker,
) -> JoinHandle<Result<()>> {
    // arboard's handle is not Send-friendly, so it lives on one blocking thread.
    tokio::task::spawn_blocking(move || {
        let mut set_rx = set_rx.blocking_lock();
        let mut board = arboard::Clipboard::new().context("cannot open the macOS clipboard")?;
        let mut guard = EchoGuard::default();
        // Don't push whatever was already copied before we started.
        let mut seen = change_count();
        info!("clipboard ready (macOS)");

        loop {
            loop {
                match set_rx.try_recv() {
                    Ok(ClipEvent::Text(text)) => {
                        guard.received(&text);
                        board.set_text(text).context("cannot write clipboard")?;
                        seen = change_count();
                    }
                    Ok(ClipEvent::Files { files, .. }) => {
                        // Pasting peer files into Finder comes in a later step.
                        debug!(count = files.len(), "peer copied files (not yet pasteable on macOS)");
                    }
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => return Ok(()),
                }
            }

            let now = change_count();
            if now != seen {
                seen = now;
                let paths = copied_files();
                if !paths.is_empty() {
                    let (id, list) = files.offer_local(paths);
                    debug!(id, count = list.len(), "local file copy");
                    let _ = changes.send(ClipEvent::Files { id, files: list });
                } else if let Ok(text) = board.get_text() {
                    // Non-text content (images) reads as an error; ignore it.
                    if guard.should_send(&text) {
                        debug!(len = text.len(), "local copy");
                        let _ = changes.send(ClipEvent::Text(text));
                    }
                }
            }
            std::thread::sleep(POLL_EVERY);
        }
    })
}

fn change_count() -> isize {
    NSPasteboard::generalPasteboard().changeCount()
}

/// Local paths of files copied in Finder, if any.
fn copied_files() -> Vec<PathBuf> {
    let pb = NSPasteboard::generalPasteboard();
    let Some(items) = pb.pasteboardItems() else { return vec![] };
    items
        .iter()
        .filter_map(|item| {
            let url = item.stringForType(unsafe { NSPasteboardTypeFileURL })?;
            let url = NSURL::URLWithString(&url)?;
            Some(PathBuf::from(url.path()?.to_string()))
        })
        .collect()
}
