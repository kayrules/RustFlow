//! Network and clipboard, each restarted on its own when it fails.
//!
//! They are independent on purpose: on GNOME the clipboard can't start
//! while the screen is locked, but the network must still accept the Mac
//! so that "Wake" can unlock the screen.

use std::time::Duration;

use anyhow::{Context, Result};
use tracing::warn;

use crate::clip::{self, Ports};
use crate::config::{Config, Role};
use crate::input::Input;
use crate::net;
use crate::status::{ClipState, Shared};

const RETRY_AFTER: Duration = Duration::from_secs(10);

pub async fn run(cfg: Config, token: String, mut shared: Shared, input: Input) -> ! {
    shared.allow_remote_unlock = cfg.allow_remote_unlock;
    let _ = crate::files::RUNTIME.set(tokio::runtime::Handle::current());
    let broker = crate::files::FileBroker::new(cfg.files.ssh_host.clone().filter(|h| !h.trim().is_empty()));
    let (clip, ports) = clip::channels(broker);
    tokio::spawn(clipboard_loop(ports, shared.clone()));
    network_loop(cfg, token, net::Link { clip, input }, shared).await
}

async fn clipboard_loop(ports: Ports, shared: Shared) {
    loop {
        shared.set_clipboard(ClipState::Starting);
        let result = async {
            let task = clip::start(&ports).await?;
            shared.set_clipboard(ClipState::Ready);
            task.await.context("clipboard backend panicked")?.context("clipboard backend stopped")
        }
        .await;
        if let Err(e) = result {
            warn!("clipboard: {e:#}");
            shared.set_clipboard(ClipState::Error(format!("{e:#}")));
        }
        tokio::time::sleep(RETRY_AFTER).await;
    }
}

async fn network_loop(cfg: Config, token: String, link: net::Link, shared: Shared) -> ! {
    loop {
        let result: Result<()> = match cfg.role {
            Role::Serve => net::serve(&cfg.listen, &token, link.clone(), shared.clone()).await,
            Role::Connect => match cfg.peer_addr() {
                Some(peer) => net::connect_loop(&peer, &token, link.clone(), shared.clone()).await,
                None => Err(anyhow::anyhow!("no peer set in config.toml")),
            },
        };
        if let Err(e) = result {
            warn!("network: {e:#}");
        }
        shared.set_peer(None);
        tokio::time::sleep(RETRY_AFTER).await;
    }
}
