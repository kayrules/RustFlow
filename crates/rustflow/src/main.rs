mod audio;
mod clip;
mod config;
mod core;
mod files;
mod icon;
mod input;
mod net;
mod screen;
#[cfg(target_os = "macos")]
mod settings;
mod status;
mod tray;
mod view;
mod wol;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::config::{Config, Role};
use crate::status::Shared;

#[derive(Parser)]
#[command(
    name = "rustflow",
    version,
    about = "Share clipboard (and soon keyboard/mouse) between machines.\nWith no command, runs as a tray app."
)]
struct Cli {
    /// File holding the shared secret. Both machines must use the same value.
    #[arg(long, global = true)]
    token_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Clipboard sync without the tray (for terminals and services).
    Clip {
        #[command(subcommand)]
        mode: ClipMode,
    },
    /// Send a Wake-on-LAN packet to the peer set in config.toml.
    Wake,
}

#[derive(Subcommand)]
enum ClipMode {
    /// Wait for the peer to connect.
    Serve {
        #[arg(long)]
        listen: Option<String>,
    },
    /// Connect to a peer and keep reconnecting.
    Connect {
        /// host or host:port of the serving peer.
        server: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut cfg = Config::load()?;

    match cli.command {
        None => {
            init_logging(true)?;
            tray::run(cfg, load_token(cli.token_file))
        }
        Some(Command::Wake) => {
            let wol = cfg.wol.context("no [wol] section in config.toml")?;
            wol::send(&wol.mac, &wol.broadcast)?;
            println!("magic packet sent to {}", wol.mac);
            Ok(())
        }
        Some(Command::Clip { mode }) => {
            init_logging(false)?;
            match mode {
                ClipMode::Serve { listen } => {
                    cfg.role = Role::Serve;
                    if let Some(l) = listen {
                        cfg.listen = l;
                    }
                }
                ClipMode::Connect { server } => {
                    cfg.role = Role::Connect;
                    cfg.peer = Some(server);
                }
            }
            let token = load_token(cli.token_file)?;
            let rt = tokio::runtime::Runtime::new()?;
            let shared = Shared::default();
            let input = rt.block_on(async { input::Input::start(&cfg, &shared) });
            let audio = audio::Audio::new(cfg.audio.enabled);
            rt.block_on(core::run(cfg, token, shared, input, audio, view::View::new(|_| {})))
        }
    }
}

/// Tray mode logs to a file (there is no terminal); CLI mode logs to stderr.
fn init_logging(to_file: bool) -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,rustflow=debug".into());
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if to_file {
        let path = log_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        builder.with_ansi(false).with_writer(std::sync::Mutex::new(file)).init();
    } else {
        builder.init();
    }
    Ok(())
}

fn load_token(path: Option<PathBuf>) -> Result<String> {
    if let Ok(t) = std::env::var("RUSTFLOW_TOKEN") {
        return Ok(t);
    }
    let path = path.unwrap_or_else(|| config_dir().join("token"));
    let token = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read token file {}", path.display()))?
        .trim()
        .to_owned();
    if token.len() < 16 {
        bail!("token in {} is shorter than 16 characters", path.display());
    }
    Ok(token)
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_dir() -> PathBuf {
    home().join(".config/rustflow")
}

pub fn log_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/Logs/rustflow/rustflow.log")
    } else {
        home().join(".local/state/rustflow/rustflow.log")
    }
}

/// Open a file or folder with the desktop's default app.
pub fn open_path(path: &std::path::Path) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    if let Err(e) = std::process::Command::new(opener).arg(path).spawn() {
        tracing::warn!("cannot open {}: {e}", path.display());
    }
}
