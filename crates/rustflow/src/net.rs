//! TCP transport: handshake, keepalive and clipboard relay.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use rustflow_proto::{MAX_FRAME_LEN, Message, PROTOCOL_VERSION};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{Instant, interval, sleep, timeout};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tracing::{debug, info, warn};

use crate::clip::{Clip, ClipEvent};
use crate::input::{self, Input};
use crate::status::Shared;

type Conn = Framed<TcpStream, LengthDelimitedCodec>;

/// Everything a connection relays: clipboard and input.
#[derive(Clone)]
pub struct Link {
    pub clip: Clip,
    pub input: Input,
}

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const PING_EVERY: Duration = Duration::from_secs(10);
/// No frame for this long means the peer is gone (e.g. it went to sleep).
const PEER_TIMEOUT: Duration = Duration::from_secs(30);

fn frame(stream: TcpStream) -> Conn {
    let _ = stream.set_nodelay(true);
    let codec = LengthDelimitedCodec::builder()
        .max_frame_length(MAX_FRAME_LEN)
        .new_codec();
    Framed::new(stream, codec)
}

async fn send(conn: &mut Conn, msg: &Message) -> Result<()> {
    conn.send(Bytes::from(msg.encode())).await?;
    Ok(())
}

async fn recv(conn: &mut Conn) -> Result<Message> {
    let bytes = conn.next().await.context("connection closed")??;
    Ok(Message::decode(&bytes)?)
}

fn hostname() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}

/// Accept peers forever. Each connection gets its own relay task.
pub async fn serve(addr: &str, token: &str, link: Link, shared: Shared) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot listen on {addr}"))?;
    info!("listening on {addr}");
    loop {
        let (stream, peer) = listener.accept().await?;
        let token = token.to_owned();
        let link = link.clone();
        let shared = shared.clone();
        tokio::spawn(async move {
            match accept(stream, &token, link, &shared).await {
                Ok(()) => info!(%peer, "peer disconnected"),
                Err(e) => warn!(%peer, "connection ended: {e:#}"),
            }
        });
    }
}

async fn accept(stream: TcpStream, token: &str, link: Link, shared: &Shared) -> Result<()> {
    let mut conn = frame(stream);
    let hello = timeout(HANDSHAKE_TIMEOUT, recv(&mut conn))
        .await
        .context("handshake timed out")??;
    let Message::Hello { version, host, token: theirs } = hello else {
        bail!("expected hello, got {hello:?}");
    };
    if version != PROTOCOL_VERSION {
        bail!("{host} speaks protocol v{version}, we speak v{PROTOCOL_VERSION}");
    }
    if theirs != token {
        bail!("{host} sent a wrong token");
    }
    send(&mut conn, &Message::Welcome { host: hostname() }).await?;
    info!("connected to {host}");
    run_relay(conn, link, shared, host).await
}

/// Keep a connection to `server` up, retrying with backoff.
pub async fn connect_loop(server: &str, token: &str, link: Link, shared: Shared) -> Result<()> {
    let mut backoff = Duration::from_secs(1);
    loop {
        match connect(server, token, link.clone(), &shared).await {
            Ok(()) => {
                info!("disconnected from {server}");
                backoff = Duration::from_secs(1);
            }
            Err(e) => debug!("connect to {server} failed: {e:#}"),
        }
        sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(10));
    }
}

async fn connect(server: &str, token: &str, link: Link, shared: &Shared) -> Result<()> {
    let stream = timeout(HANDSHAKE_TIMEOUT, TcpStream::connect(server))
        .await
        .context("connect timed out")??;
    let mut conn = frame(stream);
    let hello = Message::Hello {
        version: PROTOCOL_VERSION,
        host: hostname(),
        token: token.to_owned(),
    };
    send(&mut conn, &hello).await?;
    let reply = timeout(HANDSHAKE_TIMEOUT, recv(&mut conn))
        .await
        .context("handshake timed out")??;
    let Message::Welcome { host } = reply else {
        bail!("expected welcome, got {reply:?}");
    };
    info!("connected to {host}");
    run_relay(conn, link, shared, host).await
}

/// Show the peer as connected for as long as the relay runs.
async fn run_relay(conn: Conn, link: Link, shared: &Shared, host: String) -> Result<()> {
    shared.set_peer(Some(host));
    let result = relay(conn, &link, shared).await;
    link.input.on_disconnect();
    link.clip.files.fail_pending();
    shared.set_peer(None);
    result
}

/// Forward clipboard and input between this machine and the peer.
async fn relay(mut conn: Conn, link: &Link, shared: &Shared) -> Result<()> {
    let clip = &link.clip;
    let mut local = clip.changes.subscribe();
    for msg in link.input.on_connect() {
        send(&mut conn, &msg).await?;
    }
    let mut ping = interval(PING_EVERY);
    let mut last_seen = Instant::now();

    // "Wake" may have been clicked before the peer was reachable.
    if shared.take_wake() {
        info!("asking peer to wake its screen");
        send(&mut conn, &Message::Wake).await?;
    }

    loop {
        tokio::select! {
            frame = conn.next() => {
                let Some(frame) = frame else { return Ok(()) };
                last_seen = Instant::now();
                match Message::decode(&frame?)? {
                    Message::ClipboardText { .. } | Message::ClipboardFiles { .. } if !shared.sync_enabled() => {
                        debug!("clipboard sync paused, ignoring peer copy");
                    }
                    Message::ClipboardText { text } => {
                        debug!(len = text.len(), "clipboard from peer");
                        clip.set.send(ClipEvent::Text(text)).context("clipboard backend gone")?;
                    }
                    Message::ClipboardFiles { id, files } => {
                        debug!(id, count = files.len(), "files copied on peer");
                        clip.set.send(ClipEvent::Files { id, files }).context("clipboard backend gone")?;
                    }
                    Message::FetchFiles { id, dest } => clip.files.serve_fetch(id, dest),
                    Message::FilesReady { id } => clip.files.resolve(id, Ok(())),
                    Message::FilesFailed { id, error } => clip.files.resolve(id, Err(error)),
                    Message::Wake if shared.allow_remote_unlock => {
                        info!("peer asked to wake the screen");
                        if let Err(e) = tokio::task::spawn_blocking(crate::screen::wake).await? {
                            warn!("cannot wake screen: {e:#}");
                        }
                    }
                    Message::Wake => warn!("peer asked to wake the screen, but allow_remote_unlock is off"),
                    msg if input::is_input_message(&msg) => link.input.on_message(&msg),
                    Message::Ping => send(&mut conn, &Message::Pong).await?,
                    Message::Pong => {}
                    other => warn!("unexpected message {other:?}"),
                }
            }
            change = local.recv() => match change {
                Ok(_) if !shared.sync_enabled() => {}
                Ok(ClipEvent::Text(text)) if text.len() > MAX_FRAME_LEN - 1024 => {
                    warn!(len = text.len(), "clipboard too large, not sent");
                }
                Ok(ClipEvent::Text(text)) => {
                    debug!(len = text.len(), "clipboard to peer");
                    send(&mut conn, &Message::ClipboardText { text }).await?;
                }
                Ok(ClipEvent::Files { id, files }) => {
                    debug!(id, count = files.len(), "file list to peer");
                    send(&mut conn, &Message::ClipboardFiles { id, files }).await?;
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => bail!("clipboard backend gone"),
            },
            Some(msg) = link.input.next_outgoing() => send(&mut conn, &msg).await?,
            Some(msg) = clip.files.next_outgoing() => send(&mut conn, &msg).await?,
            _ = shared.wake_requested() => {
                if shared.take_wake() {
                    info!("asking peer to wake its screen");
                    send(&mut conn, &Message::Wake).await?;
                }
            }
            _ = ping.tick() => {
                if last_seen.elapsed() > PEER_TIMEOUT {
                    bail!("peer stopped responding");
                }
                send(&mut conn, &Message::Ping).await?;
            }
        }
    }
}
