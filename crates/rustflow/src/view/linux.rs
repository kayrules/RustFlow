//! Linux: record the screen with the ScreenCast portal and stream it.
//!
//! The portal hands us a PipeWire stream; `gst-launch-1.0` encodes it with
//! x264 into streamable Matroska on its stdout, and we forward each frame.
//! The first time, GNOME asks which screen to share; the restore token lets
//! later sessions start without asking.

use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::{PersistMode, Session};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::mkv::{Item, Reader};
use super::{Packet, RETRY_AFTER, View, view_addr};
use crate::config::{self, Config, Encoder};
use crate::net::Conn;
use crate::status::Shared;

/// The fd number the PipeWire remote gets in gst-launch.
const PIPEWIRE_FD: i32 = 3;

/// Set when NVENC failed to produce a picture; x264 is used from then on.
static NVENC_FAILED: AtomicBool = AtomicBool::new(false);

fn token_path() -> PathBuf {
    crate::home().join(".local/state/rustflow/screencast-restore-token")
}

pub async fn run(cfg: Config, token: String, _shared: Shared, _view: View) -> ! {
    loop {
        if let Err(e) = serve(&cfg, &token).await {
            warn!("screen: {e:#}");
        }
        tokio::time::sleep(RETRY_AFTER).await;
    }
}

async fn serve(cfg: &Config, token: &str) -> Result<()> {
    if !cfg.screen.enabled {
        return std::future::pending().await;
    }
    let addr = view_addr(&cfg.listen)?;
    let listener = TcpListener::bind(&addr).await.with_context(|| format!("cannot listen on {addr}"))?;
    info!("screen: listening on {addr}");
    let mut current: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        let (stream, peer) = listener.accept().await?;
        let conn = match crate::net::side_accept(stream, token).await {
            Ok(conn) => conn,
            Err(e) => {
                warn!(%peer, "screen: {e:#}");
                continue;
            }
        };
        // One viewer at a time; a new one replaces a stale connection.
        if let Some(old) = current.take() {
            old.abort();
        }
        let settings = cfg.screen.clone();
        current = Some(tokio::spawn(async move {
            info!(%peer, "screen: Mac is watching");
            match stream_to(conn, &settings).await {
                Ok(()) => info!("screen: Mac stopped watching"),
                Err(e) => warn!("screen: stream ended: {e:#}"),
            }
        }));
    }
}

async fn stream_to(mut conn: Conn, settings: &config::Screen) -> Result<()> {
    let host = crate::net::hostname();
    send(&mut conn, Packet::Status(format!("Waiting for permission — click Share on {host}"))).await?;
    let cast = match Cast::start().await {
        Ok(cast) => cast,
        Err(e) => {
            let _ = send(&mut conn, Packet::Status(format!("Cannot share the screen: {e:#}"))).await;
            return Err(e);
        }
    };
    send(&mut conn, Packet::Status("Starting video…".into())).await?;
    let kind = pick_encoder(settings.encoder).await;
    info!(encoder = ?kind, "screen: encoding");
    let mut encoder = encoder(&cast, kind, settings.bitrate_kbps)?;
    let stdout = encoder.stdout.take().context("no gst-launch output")?.into_owned_fd()?;
    let pictures = Arc::new(AtomicBool::new(false));
    let result = forward(&mut conn, stdout, pictures.clone()).await;
    if result.is_err() && kind == Encoder::Nvenc && !pictures.load(Ordering::Relaxed) {
        warn!("screen: NVENC produced no picture; switching to x264");
        NVENC_FAILED.store(true, Ordering::Relaxed);
    }
    let _ = encoder.start_kill();
    drop(cast);
    result
}

/// Pass frames from the encoder to the Mac until either side stops.
async fn forward(conn: &mut Conn, stdout: OwnedFd, pictures: Arc<AtomicBool>) -> Result<()> {
    // A small queue: when the network is slow, the encoder waits and its
    // input queue drops whole pictures instead of piling up delay.
    let (tx, mut rx) = mpsc::channel::<Packet>(2);
    let reader = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut mkv = Reader::new(std::io::BufReader::new(std::fs::File::from(stdout)));
        while let Some(item) = mkv.next_item().context("bad video from gst-launch")? {
            let packet = match item {
                Item::CodecPrivate(avcc) => Packet::Format(Bytes::from(avcc)),
                Item::Frame { key, data } => {
                    pictures.store(true, Ordering::Relaxed);
                    Packet::Frame { key, data: Bytes::from(data) }
                }
            };
            if tx.blocking_send(packet).is_err() {
                return Ok(()); // the Mac went away
            }
        }
        anyhow::bail!("gst-launch stopped (see `journalctl --user` or run it by hand)")
    });
    let result = loop {
        tokio::select! {
            packet = rx.recv() => match packet {
                Some(p) => {
                    if let Err(e) = send(conn, p).await {
                        break Err(e);
                    }
                }
                None => break Ok(()),
            },
            // The Mac sends nothing on this connection; a read ends when it closes.
            msg = conn.next() => match msg {
                Some(Ok(_)) => {}
                _ => break Ok(()),
            },
        }
    };
    drop(rx);
    match reader.await? {
        Err(e) if result.is_ok() => Err(e),
        _ => result,
    }
}

async fn send(conn: &mut Conn, packet: Packet) -> Result<()> {
    conn.send(packet.encode()).await.context("Mac disconnected")
}

/// An open ScreenCast portal session, closed when dropped (which also
/// removes GNOME's "sharing" indicator).
struct Cast {
    _portal: Screencast,
    session: Option<Session<Screencast>>,
    node: u32,
    fd: OwnedFd,
}

impl Cast {
    async fn start() -> Result<Self> {
        let portal = Screencast::new().await.context("ScreenCast portal unavailable")?;
        let session = portal.create_session(Default::default()).await?;
        let saved = std::fs::read_to_string(token_path()).ok();
        portal
            .select_sources(
                &session,
                SelectSourcesOptions::default()
                    // The Mac draws its own pointer over the picture.
                    .set_cursor_mode(CursorMode::HIDDEN)
                    .set_sources(SourceType::MONITOR)
                    .set_multiple(false)
                    .set_persist_mode(PersistMode::ExplicitlyRevoked)
                    .set_restore_token(saved.as_deref().map(str::trim)),
            )
            .await?;
        info!("screen: waiting for ScreenCast permission (click Share on the Ubuntu screen if asked)");
        let streams = portal
            .start(&session, None, Default::default())
            .await?
            .response()
            .context("screen sharing was refused")?;
        if let Some(token) = streams.restore_token() {
            save_token(token);
        }
        let stream = streams.streams().first().context("the portal shared no screen")?;
        let node = stream.pipe_wire_node_id();
        info!(node, size = ?stream.size(), "screen: sharing started");
        let fd = portal.open_pipe_wire_remote(&session, Default::default()).await?;
        Ok(Self { _portal: portal, session: Some(session), node, fd })
    }
}

impl Drop for Cast {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            tokio::spawn(async move {
                let _ = session.close().await;
            });
        }
    }
}

fn save_token(token: &str) {
    let path = token_path();
    let res = path
        .parent()
        .map(std::fs::create_dir_all)
        .unwrap_or(Ok(()))
        .and_then(|_| std::fs::write(&path, token));
    if let Err(e) = res {
        warn!("cannot save screencast restore token: {e}");
    }
}

/// Auto picks NVENC when GStreamer has it and it hasn't failed before.
async fn pick_encoder(wanted: Encoder) -> Encoder {
    match wanted {
        Encoder::Auto if !NVENC_FAILED.load(Ordering::Relaxed) && has_element("nvh264enc").await => Encoder::Nvenc,
        Encoder::Auto => Encoder::X264,
        other => other,
    }
}

async fn has_element(name: &str) -> bool {
    Command::new("gst-inspect-1.0")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

/// The encoding part of the pipeline. NVENC takes the screen's BGRx as is
/// (converting on the GPU); x264 needs a CPU conversion to I420 first.
fn encode_step(kind: Encoder, bitrate: u32) -> String {
    match kind {
        Encoder::Nvenc => format!(
            "nvh264enc preset=p1 tune=ultra-low-latency rc-mode=cbr bitrate={bitrate} \
             zerolatency=true bframes=0 gop-size=600"
        ),
        _ => format!(
            "videoconvert n-threads=4 ! video/x-raw,format=I420 \
             ! x264enc tune=zerolatency speed-preset=ultrafast bitrate={bitrate} key-int-max=600"
        ),
    }
}

/// gst-launch reading the portal's PipeWire stream and writing H.264 in Matroska.
fn encoder(cast: &Cast, kind: Encoder, bitrate: u32) -> Result<Child> {
    let pipeline = format!(
        "pipewiresrc fd={PIPEWIRE_FD} path={node} always-copy=true \
         ! queue leaky=downstream max-size-buffers=1 max-size-bytes=0 max-size-time=0 \
         ! {encode} ! video/x-h264,stream-format=avc,alignment=au \
         ! matroskamux streamable=true ! fdsink fd=1 sync=false",
        node = cast.node,
        encode = encode_step(kind, bitrate),
    );
    let mut cmd = Command::new("gst-launch-1.0");
    cmd.arg("-q").args(pipeline.split_whitespace());
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit()).kill_on_drop(true);
    let fd = cast.fd.as_raw_fd();
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            if fd == PIPEWIRE_FD {
                // dup2 onto itself keeps close-on-exec; clear it instead.
                let flags = libc::fcntl(fd, libc::F_GETFD);
                libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            } else if libc::dup2(fd, PIPEWIRE_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().context("cannot run gst-launch-1.0 (sudo apt install gstreamer1.0-tools gstreamer1.0-pipewire gstreamer1.0-plugins-ugly)")
}
