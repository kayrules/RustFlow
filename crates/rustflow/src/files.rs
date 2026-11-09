//! Copy/paste of files between machines.
//!
//! Copying only announces names and paths. When the other machine pastes,
//! the files are copied with `scp` by whichever machine can SSH to the other
//! (the Mac): it pushes its own files, and pulls the peer's.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rustflow_proto::{FileInfo, Message};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

/// Longest we wait for a transfer before giving up on the paste.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// The tokio runtime, for code called from non-async threads (macOS
/// pasteboard callbacks run on the main thread).
pub static RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();

#[derive(Clone)]
pub struct FileBroker(Arc<Inner>);

struct Inner {
    /// SSH host for the peer, if this machine can reach it with scp.
    ssh_host: Option<String>,
    /// Files copied on this machine, by offer id.
    offered: Mutex<Option<(u64, Vec<PathBuf>)>>,
    /// Pastes waiting for the peer to deliver files.
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<(), String>>>>,
    out_tx: mpsc::UnboundedSender<Message>,
    out_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Message>>,
}

impl FileBroker {
    pub fn new(ssh_host: Option<String>) -> Self {
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        Self(Arc::new(Inner {
            ssh_host,
            offered: Mutex::default(),
            pending: Mutex::default(),
            out_tx,
            out_rx: tokio::sync::Mutex::new(out_rx),
        }))
    }

    /// Remember files the user copied here and describe them for the peer.
    pub fn offer_local(&self, paths: Vec<PathBuf>) -> (u64, Vec<FileInfo>) {
        let id = new_id();
        let files = paths
            .iter()
            .map(|p| {
                let meta = std::fs::metadata(p).ok();
                FileInfo {
                    name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    size: meta.as_ref().filter(|m| m.is_file()).map_or(0, |m| m.len()),
                    is_dir: meta.is_some_and(|m| m.is_dir()),
                    path: p.to_string_lossy().into_owned(),
                }
            })
            .collect();
        *self.0.offered.lock().unwrap() = Some((id, paths));
        (id, files)
    }

    /// Bring the peer's offer `id` into `dest` on this machine.
    pub async fn fetch(&self, id: u64, files: &[FileInfo], dest: &Path) -> Result<()> {
        std::fs::create_dir_all(dest).with_context(|| format!("cannot create {}", dest.display()))?;
        match &self.0.ssh_host {
            // We can reach the peer: pull the files ourselves.
            Some(host) => {
                info!(id, count = files.len(), "pulling files with scp");
                tokio::time::timeout(FETCH_TIMEOUT, scp_from(host, files, dest))
                    .await
                    .context("transfer timed out")?
            }
            // Ask the peer to push them.
            None => self.ask_peer(id, dest).await,
        }
    }

    async fn ask_peer(&self, id: u64, dest: &Path) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.0.pending.lock().unwrap().insert(id, tx);
        let _ = self.0.out_tx.send(Message::FetchFiles { id, dest: dest.to_string_lossy().into_owned() });
        let result = tokio::time::timeout(FETCH_TIMEOUT, rx).await;
        self.0.pending.lock().unwrap().remove(&id);
        match result {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) => Err(anyhow!(e)),
            Ok(Err(_)) => bail!("transfer was abandoned"),
            Err(_) => bail!("transfer timed out"),
        }
    }

    /// The peer answered a fetch.
    pub fn resolve(&self, id: u64, result: Result<(), String>) {
        if let Some(tx) = self.0.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(result);
        }
    }

    /// The connection dropped: fail every paste that was waiting.
    pub fn fail_pending(&self) {
        for (_, tx) in self.0.pending.lock().unwrap().drain() {
            let _ = tx.send(Err("connection to peer lost".into()));
        }
    }

    /// The peer is pasting our files: push them over with scp.
    pub fn serve_fetch(&self, id: u64, dest: String) {
        let paths = match &*self.0.offered.lock().unwrap() {
            Some((offered, paths)) if *offered == id => paths.clone(),
            _ => {
                let _ = self.0.out_tx.send(Message::FilesFailed { id, error: "those files are no longer copied".into() });
                return;
            }
        };
        let Some(host) = self.0.ssh_host.clone() else {
            let _ = self.0.out_tx.send(Message::FilesFailed {
                id,
                error: "the machine you copied from has no [files] ssh_host set".into(),
            });
            return;
        };
        let out = self.0.out_tx.clone();
        tokio::spawn(async move {
            info!(id, count = paths.len(), "sending files with scp");
            let msg = match scp_to(&host, &paths, &dest).await {
                Ok(()) => Message::FilesReady { id },
                Err(e) => {
                    warn!("scp failed: {e:#}");
                    Message::FilesFailed { id, error: format!("{e:#}") }
                }
            };
            let _ = out.send(msg);
        });
    }

    pub async fn next_outgoing(&self) -> Option<Message> {
        self.0.out_rx.lock().await.recv().await
    }
}

fn scp() -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("scp");
    cmd.args(["-B", "-r", "-p", "-q", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
    cmd
}

async fn run_scp(mut cmd: tokio::process::Command) -> Result<()> {
    let output = cmd.output().await.context("cannot run scp")?;
    if !output.status.success() {
        bail!("scp: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

async fn scp_to(host: &str, paths: &[PathBuf], dest: &str) -> Result<()> {
    let mut cmd = scp();
    cmd.args(paths).arg(format!("{host}:{dest}/"));
    run_scp(cmd).await
}

async fn scp_from(host: &str, files: &[FileInfo], dest: &Path) -> Result<()> {
    if files.iter().any(|f| f.path.is_empty()) {
        bail!("the peer did not send file paths (update RustFlow on it)");
    }
    let mut cmd = scp();
    // scp's SFTP mode expands remote wildcards; escape them so names are literal.
    cmd.args(files.iter().map(|f| format!("{host}:{}", escape_glob(&f.path))));
    cmd.arg(dest);
    run_scp(cmd).await
}

fn escape_glob(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn new_id() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    nanos ^ (std::process::id() as u64) << 48
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_are_escaped() {
        assert_eq!(escape_glob("/home/k/a [1]*.txt"), "/home/k/a \\[1\\]\\*.txt");
        assert_eq!(escape_glob("/plain/name.txt"), "/plain/name.txt");
    }
}
