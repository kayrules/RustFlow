//! Linux: a "Mac (RustFlow)" output device, captured with pw-record.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use futures::SinkExt;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::process::{Child, ChildStdin, Command};
use tracing::{debug, info, warn};

use super::{Audio, CHANNELS, RETRY_AFTER, SAMPLE_RATE, audio_addr, handshake_server};
use crate::config::Config;
use crate::net::Conn;
use crate::status::Shared;

const SINK_NAME: &str = "rustflow_mac";
const SINK_LABEL: &str = "Mac (RustFlow)";
/// Bytes in one 10 ms chunk of 16-bit stereo.
const CHUNK_BYTES: usize = (SAMPLE_RATE as usize / 100) * CHANNELS * 2;

pub async fn run(cfg: Config, token: String, shared: Shared, _audio: Audio) -> ! {
    loop {
        if let Err(e) = serve(&cfg, &token, &shared).await {
            warn!("audio: {e:#}");
            shared.set_audio(format!("Audio error: {e}"));
        }
        tokio::time::sleep(RETRY_AFTER).await;
    }
}

async fn serve(cfg: &Config, token: &str, shared: &Shared) -> Result<()> {
    if !cfg.audio.enabled {
        shared.set_audio("Audio: off".into());
        return Ok(());
    }
    let _sink = VirtualSink::create().await?;
    let addr = audio_addr(&cfg.listen)?;
    let listener = TcpListener::bind(&addr).await.with_context(|| format!("cannot listen on {addr}"))?;
    info!("audio: \"{SINK_LABEL}\" output ready, listening on {addr}");
    shared.set_audio(format!("Audio: choose \"{SINK_LABEL}\" to play on the Mac"));

    loop {
        let (stream, peer) = listener.accept().await?;
        let conn = match handshake_server(stream, token).await {
            Ok(conn) => conn,
            Err(e) => {
                warn!(%peer, "audio: {e:#}");
                continue;
            }
        };
        info!(%peer, "audio: streaming to Mac");
        shared.set_audio(format!("Audio: \"{SINK_LABEL}\" plays on the Mac"));
        match stream_to(conn).await {
            Ok(()) => info!("audio: Mac disconnected"),
            Err(e) => info!("audio: stream ended: {e:#}"),
        }
        shared.set_audio(format!("Audio: choose \"{SINK_LABEL}\" to play on the Mac"));
    }
}

/// Capture the sink and send it in 10 ms chunks until the Mac goes away.
async fn stream_to(mut conn: Conn) -> Result<()> {
    let mut rec = helper("pw-record")
        .args(["--target", SINK_NAME, "-P", "stream.capture.sink=true", "--format", "s16"])
        .args(["--rate", &SAMPLE_RATE.to_string(), "--channels", &CHANNELS.to_string()])
        .args(["--latency", "10ms", "--raw", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("cannot run pw-record")?;
    let mut out = rec.stdout.take().context("no pw-record output")?;
    let mut chunk = vec![0u8; CHUNK_BYTES];
    loop {
        out.read_exact(&mut chunk).await.context("pw-record stopped")?;
        // Silence costs nothing to skip; the Mac plays silence when starved.
        if chunk.iter().all(|&b| b == 0) {
            continue;
        }
        conn.send(Bytes::copy_from_slice(&chunk)).await?;
    }
}

/// The "Mac (RustFlow)" output. It exists while the `pw-cli` process that
/// created it runs; dropping this (or RustFlow dying) removes it.
struct VirtualSink {
    child: Child,
    /// Held open so pw-cli keeps running.
    _stdin: ChildStdin,
}

impl VirtualSink {
    async fn create() -> Result<Self> {
        let mut child = helper("pw-cli")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("cannot run pw-cli (is PipeWire installed?)")?;
        let cmd = format!(
            "create-node adapter {{ factory.name=support.null-audio-sink node.name={SINK_NAME} \
             node.description=\"{SINK_LABEL}\" media.class=Audio/Sink audio.position=[FL FR] \
             audio.rate={SAMPLE_RATE} }}\n"
        );
        let mut stdin = child.stdin.take().context("no pw-cli input")?;
        tokio::io::AsyncWriteExt::write_all(&mut stdin, cmd.as_bytes()).await?;
        // Wait until PipeWire lists the node.
        for _ in 0..20 {
            if sink_exists().await {
                debug!("audio: virtual output created");
                return Ok(Self { child, _stdin: stdin });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        bail!("PipeWire did not create the \"{SINK_LABEL}\" output")
    }
}

impl Drop for VirtualSink {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

async fn sink_exists() -> bool {
    sink_id().await.is_some()
}

/// PipeWire object id of the "Mac (RustFlow)" output.
async fn sink_id() -> Option<u64> {
    let out = Command::new("pw-dump").output().await.ok()?;
    let objects: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    objects.as_array()?.iter().find_map(|o| {
        (o.pointer("/info/props/node.name").and_then(|v| v.as_str()) == Some(SINK_NAME))
            .then(|| o.get("id").and_then(|v| v.as_u64()))
            .flatten()
    })
}

/// The output that was the default before we switched to the Mac.
static PREVIOUS_DEFAULT: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);

/// Make "Mac (RustFlow)" the default output (on), or switch back (off).
pub async fn route_to_mac(on: bool) -> Result<()> {
    if on {
        let ours = sink_id().await.context("the \"Mac (RustFlow)\" output is not running")?;
        let current = default_sink_id().await;
        if current != Some(ours) {
            *PREVIOUS_DEFAULT.lock().unwrap() = current;
        }
        set_default(ours).await
    } else {
        let previous = PREVIOUS_DEFAULT.lock().unwrap().take();
        match previous {
            Some(id) => set_default(id).await,
            None => Ok(()),
        }
    }
}

async fn default_sink_id() -> Option<u64> {
    let out = Command::new("wpctl").args(["inspect", "@DEFAULT_AUDIO_SINK@"]).output().await.ok()?;
    // First line: "id 56, type PipeWire:Interface:Node"
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().nth(1)?.trim_end_matches(',').parse().ok()
}

async fn set_default(id: u64) -> Result<()> {
    let status = Command::new("wpctl").args(["set-default", &id.to_string()]).status().await?;
    if !status.success() {
        bail!("wpctl set-default {id} failed");
    }
    Ok(())
}

/// A helper process that the kernel stops if RustFlow dies, so the virtual
/// output never outlives the app.
fn helper(program: &str) -> Command {
    let mut cmd = Command::new(program);
    // SAFETY: prctl is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    cmd
}
