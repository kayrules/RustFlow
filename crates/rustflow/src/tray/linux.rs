//! Linux tray via StatusNotifierItem (shown by GNOME's AppIndicator extension).

use anyhow::Result;
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::{MenuItem, ToolTip, TrayMethods};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::status::{ClipState, Shared, Status};
use crate::{icon, wol};

struct Tray {
    status: Status,
    shared: Shared,
    cfg: Config,
    /// "Mac (RustFlow)" is the default output.
    routing_audio: bool,
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "rustflow".into()
    }

    fn title(&self) -> String {
        "rustflow".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        [22, 32, 48]
            .into_iter()
            .map(|s| ksni::Icon {
                width: s as i32,
                height: s as i32,
                data: icon::argb(s, self.status.light()),
            })
            .collect()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "rustflow".into(),
            description: self.status.peer_line(&self.cfg.peer_label()),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: self.status.peer_line(&self.cfg.peer_label()),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.status.clipboard_line(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.status.input.clone(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.status.audio.clone(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            CheckmarkItem {
                label: "Sync clipboard".into(),
                checked: self.status.sync_enabled,
                activate: Box::new(|t: &mut Self| {
                    let on = !t.status.sync_enabled;
                    t.shared.set_sync(on);
                    t.status.sync_enabled = on;
                }),
                ..Default::default()
            }
            .into(),
        ];
        if self.cfg.audio.enabled {
            items.push(
                CheckmarkItem {
                    label: format!("Send audio to {}", self.cfg.peer_label()),
                    checked: self.routing_audio,
                    activate: Box::new(|t: &mut Self| {
                        let on = !t.routing_audio;
                        t.routing_audio = on;
                        tokio::spawn(async move {
                            if let Err(e) = crate::audio::route_to_mac(on).await {
                                error!("cannot switch audio output: {e:#}");
                            }
                        });
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(
            StandardItem {
                label: format!("Wake {}", self.cfg.peer_label()),
                activate: Box::new(|t: &mut Self| {
                    if let Some(w) = &t.cfg.wol {
                        if let Err(e) = wol::send(&w.mac, &w.broadcast) {
                            error!("wake failed: {e:#}");
                        }
                    }
                    t.shared.request_wake();
                }),
                ..Default::default()
            }
            .into(),
        );
        items.extend([
            MenuItem::Separator,
            StandardItem {
                label: "Open log".into(),
                activate: Box::new(|_: &mut Self| crate::open_path(&crate::log_path())),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Open config folder".into(),
                activate: Box::new(|_: &mut Self| crate::open_path(&crate::config_dir())),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit rustflow".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|_: &mut Self| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
}

pub fn run(cfg: Config, token: Result<String>) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(async move {
        let shared = Shared::default();
        let input = crate::input::Input::start(&cfg, &shared);
        let tray = Tray {
            status: shared.subscribe().borrow().clone(),
            shared: shared.clone(),
            cfg: cfg.clone(),
            routing_audio: false,
        };
        // Wait for a tray host instead of failing if the AppIndicator extension is off.
        match tray.assume_sni_available(true).spawn().await {
            Ok(handle) => {
                let mut rx = shared.subscribe();
                tokio::spawn(async move {
                    while rx.changed().await.is_ok() {
                        let status = rx.borrow_and_update().clone();
                        handle.update(|t| t.status = status).await;
                    }
                });
                info!("rustflow tray started");
            }
            // Keep syncing even if there is no tray host (e.g. extension disabled).
            Err(e) => warn!("cannot show tray icon: {e}"),
        }

        match token {
            Ok(token) => {
                let audio = crate::audio::Audio::new(cfg.audio.enabled);
                crate::core::run(cfg, token, shared, input, audio).await
            }
            Err(e) => {
                error!("{e:#}");
                shared.set_clipboard(ClipState::Error(format!("{e:#}")));
                std::future::pending().await
            }
        }
    })
}
