//! macOS: receive the Linux screen while its window is open.

use anyhow::{Context, Result};
use futures::StreamExt;
use tracing::{debug, info};

use super::{Event, Packet, RETRY_AFTER, View, parse_avcc, view_addr};
use crate::config::Config;
use crate::status::Shared;

pub async fn run(cfg: Config, token: String, _shared: Shared, view: View) -> ! {
    let label = cfg.peer_label();
    let addr = cfg.peer_addr().map(|a| view_addr(&a));
    let mut wanted = view.0.wanted.subscribe();
    loop {
        // Sleep until the window opens.
        while !*wanted.borrow_and_update() {
            if wanted.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
        let addr = match &addr {
            Some(Ok(addr)) => addr,
            Some(Err(e)) => {
                view.emit(Event::Status(format!("Bad peer address: {e}")));
                idle().await
            }
            None => {
                view.emit(Event::Status("No peer set in config.toml".into()));
                idle().await
            }
        };
        view.emit(Event::Status(format!("Connecting to {label}…")));
        let closed = async {
            while *wanted.borrow_and_update() {
                if wanted.changed().await.is_err() {
                    break;
                }
            }
        };
        let restart = view.0.restart.notified();
        let result = tokio::select! {
            r = receive(addr, &token, &view) => r,
            () = closed => {
                debug!("screen: window closed");
                continue;
            }
            () = restart => {
                info!("screen: restarting stream");
                continue;
            }
        };
        let line = match result {
            Ok(()) => format!("{label} stopped sharing its screen"),
            Err(e) => format!("Cannot reach {label}'s screen: {e:#}"),
        };
        debug!("screen: {line}");
        view.emit(Event::Status(line));
        tokio::time::sleep(RETRY_AFTER).await;
    }
}

/// Nothing to do until RustFlow restarts with a fixed config.
async fn idle() -> ! {
    loop {
        std::future::pending::<()>().await;
    }
}

async fn receive(addr: &str, token: &str, view: &View) -> Result<()> {
    let mut conn = crate::net::side_connect(addr, token).await?;
    info!("screen: connected");
    while let Some(frame) = conn.next().await {
        match Packet::decode(frame.context("screen stream broke")?.freeze())? {
            Packet::Status(text) => view.emit(Event::Status(text)),
            Packet::Format(avcc) => view.emit(Event::Format(parse_avcc(&avcc)?)),
            Packet::Frame { key, data } => view.emit(Event::Frame { key, data }),
        }
    }
    Ok(())
}
