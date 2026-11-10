//! The Mac's Settings window: a small web page in a WKWebView.
//!
//! It edits `config.toml` (keeping its comments). Saving restarts RustFlow,
//! so every setting takes effect the same way as at login.

use anyhow::{Context, Result};
use serde::Deserialize;
use tao::dpi::LogicalSize;
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder, WindowId};
use tracing::{info, warn};
use wry::{WebView, WebViewBuilder};

use crate::config::{Config, MacSettings};

const PAGE: &str = include_str!("page.html");

/// A message from the page.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Request {
    Save { settings: MacSettings },
    Close,
    OpenFile,
}

/// What the tray should do after handling a request.
pub enum Outcome {
    Stay,
    Close,
    /// Settings were saved: restart to apply them.
    Restart,
}

pub struct SettingsWindow {
    window: Window,
    webview: WebView,
    /// The settings as they were when the window opened.
    original: MacSettings,
}

impl SettingsWindow {
    /// `forward` turns a page message into a tray event.
    pub fn open<T: 'static>(
        target: &EventLoopWindowTarget<T>,
        proxy: EventLoopProxy<T>,
        forward: fn(Request) -> T,
    ) -> Result<Self> {
        let cfg = Config::load()?;
        let original = MacSettings::from_config(&cfg);
        let window = WindowBuilder::new()
            .with_title("RustFlow Settings")
            .with_inner_size(LogicalSize::new(600.0, 780.0))
            .with_min_inner_size(LogicalSize::new(520.0, 400.0))
            .build(target)?;
        let json = serde_json::to_string(&original)?;
        let webview = WebViewBuilder::new()
            .with_html(PAGE.replace("/*SETTINGS*/", &json))
            .with_ipc_handler(move |req| match serde_json::from_str::<Request>(req.body()) {
                Ok(request) => {
                    let _ = proxy.send_event(forward(request));
                }
                Err(e) => warn!("settings: bad message from page: {e}"),
            })
            // Links in the page must not navigate away from it.
            .with_navigation_handler(|url| url.starts_with("about:") || url.starts_with("data:"))
            .build(&window)
            .context("cannot create the settings page")?;
        window.set_focus();
        Ok(Self { window, webview, original })
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn focus(&self) {
        self.window.set_focus();
    }

    pub fn handle(&self, request: Request) -> Outcome {
        match request {
            Request::Close => Outcome::Close,
            Request::OpenFile => {
                crate::open_path(&Config::path());
                Outcome::Stay
            }
            Request::Save { settings } => match self.save(&settings) {
                Ok(()) => Outcome::Restart,
                Err((field, message)) => {
                    let js = format!(
                        "window.saveFailed({}, {})",
                        serde_json::to_string(&message).unwrap_or_default(),
                        serde_json::to_string(&field).unwrap_or_default(),
                    );
                    if let Err(e) = self.webview.evaluate_script(&js) {
                        warn!("settings: cannot show error: {e}");
                    }
                    Outcome::Stay
                }
            },
        }
    }

    fn save(&self, settings: &MacSettings) -> Result<(), (Option<&'static str>, String)> {
        let settings = settings.validated().map_err(|(field, msg)| (Some(field), msg))?;
        Config::save_mac_settings(&self.original, &settings).map_err(|e| (None, format!("{e:#}")))?;
        info!("settings saved");
        Ok(())
    }
}

/// Start a new copy of RustFlow once this one has exited.
pub fn relaunch() {
    let Ok(exe) = std::env::current_exe() else { return };
    // .../rustflow.app/Contents/MacOS/rustflow → open the bundle; otherwise run the binary.
    let bundle = exe.ancestors().nth(3).filter(|p| p.extension().is_some_and(|e| e == "app"));
    let target = bundle.unwrap_or(&exe).to_string_lossy().replace('\'', r"'\''");
    let launch = if bundle.is_some() { format!("open '{target}'") } else { format!("'{target}' &") };
    let script = format!("while kill -0 {} 2>/dev/null; do sleep 0.1; done; {launch}", std::process::id());
    if let Err(e) = std::process::Command::new("/bin/sh").args(["-c", &script]).spawn() {
        warn!("cannot restart RustFlow: {e}");
    }
}
