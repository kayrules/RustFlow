//! macOS menu bar item (tray-icon on a tao event loop, no Dock icon).

use anyhow::Result;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use tracing::{error, info};

use crate::config::Config;
use crate::status::{ClipState, Shared, Status};
use crate::{icon, wol};

enum UserEvent {
    Status(Status),
    Menu(MenuEvent),
}

const ICON_SIZE: u32 = 44; // 22pt @2x

fn tray_icon(status: &Status) -> Option<Icon> {
    Icon::from_rgba(icon::rgba(ICON_SIZE, status.light()), ICON_SIZE, ICON_SIZE).ok()
}

pub fn run(cfg: Config, token: Result<String>) -> Result<()> {
    let shared = Shared::default();

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::Menu(e));
    }));

    let input = crate::input::Input::start(&cfg, &shared);

    // Network + clipboard live on a tokio runtime off the main thread.
    let proxy = event_loop.create_proxy();
    let core_shared = shared.clone();
    let core_input = input.clone();
    let core_cfg = cfg.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async move {
            let mut rx = core_shared.subscribe();
            tokio::spawn(async move {
                loop {
                    let status = rx.borrow_and_update().clone();
                    if proxy.send_event(UserEvent::Status(status)).is_err() || rx.changed().await.is_err() {
                        break;
                    }
                }
            });
            match token {
                Ok(token) => crate::core::run(core_cfg, token, core_shared, core_input).await,
                Err(e) => {
                    error!("{e:#}");
                    core_shared.set_clipboard(ClipState::Error(format!("{e:#}")));
                    std::future::pending::<()>().await;
                }
            }
        });
    });

    let peer_label = cfg.peer_label();
    let peer_item = MenuItem::new(format!("Waiting for {peer_label}…"), false, None);
    let clip_item = MenuItem::new("Clipboard: starting…", false, None);
    let input_item = MenuItem::new("Mouse & keyboard: starting…", false, None);
    let sync_item = CheckMenuItem::new("Sync clipboard", true, true, None);
    let share_item = CheckMenuItem::new("Share mouse & keyboard", true, cfg.input.enabled, None);
    let wake_item = MenuItem::new(format!("Wake {peer_label}"), true, None);
    let log_item = MenuItem::new("Open log", true, None);
    let config_item = MenuItem::new("Open config folder", true, None);
    let quit_item = MenuItem::new("Quit rustflow", true, None);

    let menu = Menu::new();
    menu.append_items(&[
        &peer_item,
        &clip_item,
        &input_item,
        &PredefinedMenuItem::separator(),
        &sync_item,
        &share_item,
        &wake_item,
        &PredefinedMenuItem::separator(),
        &log_item,
        &config_item,
        &PredefinedMenuItem::separator(),
        &quit_item,
    ])?;

    let mut tray: Option<TrayIcon> = None;
    let mut menu = Some(menu);
    let mut last = Status::default();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            // The tray must be created once the app has finished launching.
            Event::NewEvents(StartCause::Init) => {
                let mut builder = TrayIconBuilder::new().with_tooltip("rustflow");
                if let Some(m) = menu.take() {
                    builder = builder.with_menu(Box::new(m));
                }
                if let Some(i) = tray_icon(&last) {
                    builder = builder.with_icon(i);
                }
                match builder.build() {
                    Ok(t) => tray = Some(t),
                    Err(e) => error!("cannot create menu bar item: {e}"),
                }
                info!("rustflow menu bar app started");
            }
            Event::UserEvent(UserEvent::Status(status)) => {
                peer_item.set_text(status.peer_line(&peer_label));
                clip_item.set_text(status.clipboard_line());
                input_item.set_text(&status.input);
                sync_item.set_checked(status.sync_enabled);
                if let Some(t) = &tray {
                    if status.light() != last.light() || last == Status::default() {
                        let _ = t.set_icon(tray_icon(&status));
                    }
                    let _ = t.set_tooltip(Some(format!("rustflow — {}", status.peer_line(&peer_label))));
                }
                last = status;
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                if e.id == *quit_item.id() {
                    *control_flow = ControlFlow::Exit;
                } else if e.id == *sync_item.id() {
                    shared.set_sync(sync_item.is_checked());
                } else if e.id == *share_item.id() {
                    input.set_enabled(share_item.is_checked());
                } else if e.id == *wake_item.id() {
                    wake(&cfg, &shared);
                } else if e.id == *log_item.id() {
                    crate::open_path(&crate::log_path());
                } else if e.id == *config_item.id() {
                    crate::open_path(&crate::config_dir());
                }
            }
            _ => {}
        }
    })
}

/// Power the peer on (Wake-on-LAN) and ask it to turn its screen on once connected.
fn wake(cfg: &Config, shared: &Shared) {
    if let Some(w) = &cfg.wol {
        match wol::send(&w.mac, &w.broadcast) {
            Ok(()) => info!("magic packet sent to {}", w.mac),
            Err(e) => error!("wake failed: {e:#}"),
        }
    }
    shared.request_wake();
}
