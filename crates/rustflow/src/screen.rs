//! Turning this machine's screen on when the peer asks for it.

use std::process::Command;

use anyhow::{Context, Result, bail};

/// Unlock the graphical session on seat0 (GNOME turns the screen on).
#[cfg(target_os = "linux")]
pub fn wake() -> Result<()> {
    let out = Command::new("loginctl").args(["list-sessions", "--no-legend"]).output()?;
    let list = String::from_utf8_lossy(&out.stdout);
    let id = list
        .lines()
        .find(|l| l.split_whitespace().any(|w| w == "seat0"))
        .and_then(|l| l.split_whitespace().next())
        .context("no graphical session on seat0")?;
    run(Command::new("loginctl").args(["unlock-session", id]))
}

/// Wake the display; the user still unlocks the Mac themselves.
#[cfg(target_os = "macos")]
pub fn wake() -> Result<()> {
    run(Command::new("caffeinate").args(["-u", "-t", "2"]))
}

fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd.status().with_context(|| format!("cannot run {cmd:?}"))?;
    if !status.success() {
        bail!("{cmd:?} failed with {status}");
    }
    Ok(())
}
