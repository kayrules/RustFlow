//! Tray / menu bar app.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
pub use linux::run;
#[cfg(target_os = "macos")]
pub use macos::run;
