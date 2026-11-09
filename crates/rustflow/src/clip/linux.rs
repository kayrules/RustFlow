//! Linux (GNOME Wayland) clipboard through the xdg-desktop-portal.
//!
//! GNOME doesn't let background apps touch the clipboard directly, so we
//! open a RemoteDesktop session and use the Clipboard portal attached to it.
//! The first run shows a "Remote Desktop" prompt; after that the restore
//! token lets the session come back without asking.
//!
//! Files copied on the peer are offered as a file list. Nothing is
//! transferred until an app (Files) actually asks for it on paste.

use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ashpd::desktop::PersistMode;
use ashpd::desktop::Session;
use ashpd::desktop::clipboard::{Clipboard, SetSelectionOptions};
use ashpd::desktop::remote_desktop::{DeviceType, RemoteDesktop, SelectDevicesOptions};
use futures::StreamExt;
use rustflow_proto::FileInfo;
use tokio::sync::{Mutex, OnceCell, broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use super::{ClipEvent, EchoGuard};
use crate::files::FileBroker;

pub const APP_ID: &str = "dev.rustflow.Rustflow";

/// Text formats we offer and accept, best first.
const TEXT_MIMES: &[&str] = &["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT"];
/// File list formats GNOME Files understands.
const GNOME_FILES_MIME: &str = "x-special/gnome-copied-files";
const URI_LIST_MIME: &str = "text/uri-list";
const FILE_MIMES: &[&str] = &[GNOME_FILES_MIME, URI_LIST_MIME];

const READ_TIMEOUT: Duration = Duration::from_secs(3);
/// Files and the Desktop Icons extension read a new clipboard right away
/// (to update Paste and mark cut files). Reads this soon after we offer
/// files are answered without fetching; a real paste comes later.
const EAGER_READ_WINDOW: Duration = Duration::from_secs(1);
/// Only show the progress window for transfers slower than this.
const PROGRESS_AFTER: Duration = Duration::from_millis(1500);

type Portal = Arc<(Clipboard, Session<RemoteDesktop>)>;

fn token_path() -> PathBuf {
    crate::home().join(".local/state/rustflow/portal-restore-token")
}

fn paste_cache() -> PathBuf {
    crate::home().join(".cache/rustflow/clipboard")
}

pub async fn spawn(
    changes: broadcast::Sender<ClipEvent>,
    set_rx: Arc<Mutex<mpsc::UnboundedReceiver<ClipEvent>>>,
    files: FileBroker,
) -> Result<JoinHandle<Result<()>>> {
    // Lets the portal remember our permission under a stable app id.
    // Must happen before any other portal call on this connection.
    match APP_ID.try_into() {
        Ok(id) => {
            if let Err(e) = ashpd::register_host_app(id).await {
                warn!("could not register app id with the portal: {e}");
            }
        }
        Err(e) => warn!("invalid app id: {e}"),
    }

    let rd = RemoteDesktop::new().await.context("RemoteDesktop portal unavailable")?;
    let cb = Clipboard::new().await.context("Clipboard portal unavailable")?;
    let session = rd.create_session(Default::default()).await?;
    cb.request(&session, Default::default()).await?;

    let saved = std::fs::read_to_string(token_path()).ok();
    rd.select_devices(
        &session,
        SelectDevicesOptions::default()
            .set_devices(DeviceType::KEYBOARD)
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(saved.as_deref().map(str::trim)),
    )
    .await?;

    info!("waiting for Remote Desktop permission (click Share on the Ubuntu screen if asked)");
    let started = rd
        .start(&session, None, Default::default())
        .await?
        .response()
        .context("Remote Desktop permission was refused")?;
    if !started.is_clipboard_enabled() {
        bail!("the portal started the session without clipboard access");
    }
    if let Some(token) = started.restore_token() {
        save_token(token);
    }
    info!("clipboard ready (GNOME portal)");

    let portal: Portal = Arc::new((cb, session));
    Ok(tokio::spawn(run(rd, portal, changes, set_rx, files)))
}

fn save_token(token: &str) {
    let path = token_path();
    let res = path
        .parent()
        .map(std::fs::create_dir_all)
        .unwrap_or(Ok(()))
        .and_then(|_| std::fs::write(&path, token));
    if let Err(e) = res {
        warn!("cannot save portal restore token: {e}");
    }
}

/// What we currently own on the GNOME clipboard (set from the peer).
enum Offered {
    None,
    Text(String),
    Files(Arc<RemoteFiles>),
}

/// Files copied on the peer, fetched at most once, on the first paste.
struct RemoteFiles {
    id: u64,
    files: Vec<FileInfo>,
    offered_at: Instant,
    fetched: OnceCell<Result<Vec<PathBuf>, String>>,
}

impl RemoteFiles {
    fn dest(&self) -> PathBuf {
        paste_cache().join(self.id.to_string())
    }

    /// Where the files will be once fetched.
    fn paths(&self) -> Vec<PathBuf> {
        let dest = self.dest();
        self.files.iter().map(|f| dest.join(&f.name)).collect()
    }
}

async fn run(
    _rd: RemoteDesktop,
    portal: Portal,
    changes: broadcast::Sender<ClipEvent>,
    set_rx: Arc<Mutex<mpsc::UnboundedReceiver<ClipEvent>>>,
    broker: FileBroker,
) -> Result<()> {
    let mut set_rx = set_rx.lock().await;
    let (cb, session) = (&portal.0, &portal.1);
    let mut owner_changed = Box::pin(cb.receive_selection_owner_changed::<RemoteDesktop>().await?);
    let mut transfers = Box::pin(cb.receive_selection_transfer::<RemoteDesktop>().await?);
    let mut closed = Box::pin(session.receive_closed().await?);

    let mut guard = EchoGuard::default();
    let mut offered = Offered::None;

    loop {
        tokio::select! {
            Some((_, ev)) = owner_changed.next() => {
                if ev.session_is_owner() == Some(true) {
                    continue; // our own set_selection
                }
                let Some(mime) = TEXT_MIMES
                    .iter()
                    .find(|m| ev.mime_types().iter().any(|t| t == *m))
                else {
                    debug!("clipboard changed to non-text content {:?}", ev.mime_types());
                    continue;
                };
                match read_selection(cb, session, mime).await {
                    Ok(text) if guard.should_send(&text) => {
                        debug!(len = text.len(), "local copy");
                        let _ = changes.send(ClipEvent::Text(text));
                    }
                    Ok(_) => {}
                    Err(e) => warn!("cannot read clipboard: {e:#}"),
                }
            }
            Some((_, mime, serial)) = transfers.next() => {
                // An app is pasting what we offered.
                match &offered {
                    Offered::Files(remote) if FILE_MIMES.contains(&mime.as_str()) => {
                        if remote.offered_at.elapsed() < EAGER_READ_WINDOW {
                            debug!(%mime, "early clipboard read, answering without fetching");
                            let body = file_list(&mime, &remote.paths());
                            write_selection(cb, session, serial, body.into_bytes()).await;
                        } else {
                            // Fetching can take minutes; don't block the clipboard loop.
                            tokio::spawn(serve_files(portal.clone(), broker.clone(), remote.clone(), mime, serial));
                        }
                    }
                    Offered::Text(text) => {
                        let ok = write_selection(cb, session, serial, text.clone().into_bytes()).await;
                        debug!(%mime, ok, "served text paste");
                    }
                    _ => {
                        let _ = cb.selection_write_done(session, serial, false).await;
                    }
                }
            }
            event = set_rx.recv() => {
                let Some(event) = event else { return Ok(()) };
                let mimes: &[&str] = match event {
                    ClipEvent::Text(text) => {
                        guard.received(&text);
                        offered = Offered::Text(text);
                        TEXT_MIMES
                    }
                    ClipEvent::Files { id, files } => {
                        info!(id, count = files.len(), "peer copied files; ready to paste");
                        clear_cache();
                        offered = Offered::Files(Arc::new(RemoteFiles {
                            id,
                            files,
                            offered_at: Instant::now(),
                            fetched: OnceCell::new(),
                        }));
                        FILE_MIMES
                    }
                };
                cb.set_selection(session, SetSelectionOptions::default().set_mime_types(mimes)).await?;
            }
            _ = closed.next() => bail!("portal session was closed"),
        }
    }
}

/// Answer a paste of peer files: fetch them (once), then hand Files the paths.
async fn serve_files(portal: Portal, broker: FileBroker, remote: Arc<RemoteFiles>, mime: String, serial: u32) {
    let (cb, session) = (&portal.0, &portal.1);
    let fetched = remote
        .fetched
        .get_or_init(|| async {
            info!(id = remote.id, "paste requested, fetching files from peer");
            let dest = remote.dest();
            let files = remote.files.clone();
            let progress = tokio::spawn(async move {
                tokio::time::sleep(PROGRESS_AFTER).await;
                Progress::show(&files)
            });
            let result = async {
                std::fs::create_dir_all(&dest).with_context(|| format!("cannot create {}", dest.display()))?;
                broker.fetch(remote.id, &dest).await?;
                Ok::<_, anyhow::Error>(remote.paths())
            }
            .await;
            // Close the window if it opened; otherwise make sure it never does.
            progress.abort();
            drop(progress.await);
            result.map_err(|e| {
                warn!("paste failed: {e:#}");
                notify("Paste failed", &format!("{e:#}"));
                format!("{e:#}")
            })
        })
        .await;

    let ok = match fetched {
        Ok(paths) => {
            let body = file_list(&mime, paths);
            write_selection(cb, session, serial, body.into_bytes()).await
        }
        Err(_) => {
            let _ = cb.selection_write_done(session, serial, false).await;
            false
        }
    };
    debug!(%mime, ok, "served file paste");
}

async fn write_selection(cb: &Clipboard, session: &Session<RemoteDesktop>, serial: u32, bytes: Vec<u8>) -> bool {
    let ok = match cb.selection_write(session, serial).await {
        Ok(fd) => {
            let fd: OwnedFd = fd.into();
            let res = tokio::task::spawn_blocking(move || std::fs::File::from(fd).write_all(&bytes)).await;
            matches!(res, Ok(Ok(())))
        }
        Err(e) => {
            warn!("SelectionWrite failed: {e}");
            false
        }
    };
    let _ = cb.selection_write_done(session, serial, ok).await;
    ok
}

async fn read_selection(cb: &Clipboard, session: &Session<RemoteDesktop>, mime: &str) -> Result<String> {
    let fd: OwnedFd = cb.selection_read(session, mime).await?.into();
    // The source app writes into a pipe; read it off the async runtime.
    let read = tokio::task::spawn_blocking(move || {
        let mut buf = Vec::new();
        std::fs::File::from(fd).read_to_end(&mut buf)?;
        Ok::<_, std::io::Error>(buf)
    });
    let bytes = timeout(READ_TIMEOUT, read).await.context("clipboard read timed out")???;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Files pasted earlier were copied out by the file manager; drop the old copies.
fn clear_cache() {
    let _ = std::fs::remove_dir_all(paste_cache());
}

fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => uri.push(b as char),
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri
}

fn file_list(mime: &str, paths: &[PathBuf]) -> String {
    if mime == GNOME_FILES_MIME { gnome_copied_files(paths) } else { uri_list(paths) }
}

fn gnome_copied_files(paths: &[PathBuf]) -> String {
    let mut out = String::from("copy");
    for p in paths {
        out.push('\n');
        out.push_str(&file_uri(p));
    }
    out
}

fn uri_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| format!("{}\r\n", file_uri(p))).collect()
}

/// A pulsing "Copying from Mac…" window, closed when dropped.
struct Progress(Option<std::process::Child>);

impl Progress {
    fn show(files: &[FileInfo]) -> Self {
        let total: u64 = files.iter().map(|f| f.size).sum();
        let what = match files {
            [one] => one.name.clone(),
            many => format!("{} items", many.len()),
        };
        let text = format!("Copying {what} ({}) from your Mac…", human_size(total));
        let child = std::process::Command::new("zenity")
            .args(["--progress", "--pulsate", "--no-cancel", "--auto-close", "--title=rustflow"])
            .arg(format!("--text={text}"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| warn!("cannot show progress window: {e}"))
            .ok();
        Self(child)
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn notify(title: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=rustflow", title, body])
        .spawn();
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{size:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_are_escaped() {
        let p = PathBuf::from("/home/k/.cache/rustflow/clipboard/1/My File (1).pdf");
        assert_eq!(file_uri(&p), "file:///home/k/.cache/rustflow/clipboard/1/My%20File%20%281%29.pdf");
        assert_eq!(gnome_copied_files(&[p.clone()]), format!("copy\n{}", file_uri(&p)));
        assert_eq!(uri_list(&[p.clone()]), format!("{}\r\n", file_uri(&p)));
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
    }
}
