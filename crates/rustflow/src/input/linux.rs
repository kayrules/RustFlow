//! Virtual keyboard + absolute pointer through /dev/uinput.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode, UinputAbsSetup,
};
use rustflow_proto::{Message, MouseButton};
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::status::Shared;

/// Pointer coordinates are sent as 0..=ABS_MAX; GNOME maps that onto the whole desktop.
const ABS_MAX: i32 = 32767;
const HI_RES_PER_NOTCH: f64 = 120.0;

struct Devices {
    keyboard: VirtualDevice,
    pointer: VirtualDevice,
    held_keys: HashSet<u16>,
    held_buttons: HashSet<u16>,
    /// Hi-res wheel units not yet emitted as a whole notch.
    wheel_rest: (f64, f64),
}

pub struct State {
    devices: Mutex<Option<Devices>>,
    screen: Option<(u32, u32)>,
    unlock_on_enter: bool,
}

pub fn start(cfg: &Config, shared: &Shared) -> Arc<State> {
    let screen = screen_size();
    let devices = match Devices::create() {
        Ok(d) => {
            info!("virtual keyboard and pointer ready (screen {screen:?})");
            shared.set_input("Mouse & keyboard: ready".into());
            Some(d)
        }
        Err(e) => {
            warn!("input injection unavailable: {e:#}");
            shared.set_input("Mouse & keyboard: no access to /dev/uinput".into());
            None
        }
    };
    Arc::new(State { devices: Mutex::new(devices), screen, unlock_on_enter: cfg.unlock_on_enter })
}

impl State {
    pub fn on_connect(&self) -> Vec<Message> {
        match (self.devices.lock().unwrap().is_some(), self.screen) {
            (true, Some((width, height))) => vec![Message::Screen { width, height }],
            _ => vec![],
        }
    }

    pub fn on_message(&self, msg: &Message) {
        if self.unlock_on_enter && matches!(msg, Message::Enter { .. }) {
            // Off the network task; unlocking an unlocked screen does nothing.
            std::thread::spawn(|| {
                if let Err(e) = crate::screen::wake() {
                    warn!("unlock on enter failed: {e:#}");
                }
            });
        }
        let mut guard = self.devices.lock().unwrap();
        let Some(dev) = guard.as_mut() else { return };
        if let Err(e) = dev.handle(msg) {
            warn!("cannot inject {msg:?}: {e}");
        }
    }

    pub fn on_disconnect(&self) {
        if let Some(dev) = self.devices.lock().unwrap().as_mut() {
            dev.release_all();
        }
    }

    pub async fn next_outgoing(&self) -> Option<Message> {
        std::future::pending().await
    }
}

impl Devices {
    fn create() -> Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for code in 1..=248u16 {
            keys.insert(KeyCode(code));
        }
        let keyboard = VirtualDevice::builder()
            .context("cannot open /dev/uinput")?
            .name("rustflow keyboard")
            .with_keys(&keys)?
            .build()?;

        // Shaped like QEMU's USB tablet: absolute X/Y, buttons and wheels.
        let mut buttons = AttributeSet::<KeyCode>::new();
        for b in [KeyCode::BTN_LEFT, KeyCode::BTN_RIGHT, KeyCode::BTN_MIDDLE, KeyCode::BTN_SIDE, KeyCode::BTN_EXTRA] {
            buttons.insert(b);
        }
        let mut wheels = AttributeSet::<RelativeAxisCode>::new();
        for w in [
            RelativeAxisCode::REL_WHEEL,
            RelativeAxisCode::REL_HWHEEL,
            RelativeAxisCode::REL_WHEEL_HI_RES,
            RelativeAxisCode::REL_HWHEEL_HI_RES,
        ] {
            wheels.insert(w);
        }
        let axis = AbsInfo::new(0, 0, ABS_MAX, 0, 0, 0);
        let pointer = VirtualDevice::builder()?
            .name("rustflow pointer")
            .with_keys(&buttons)?
            .with_relative_axes(&wheels)?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, axis))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, axis))?
            .build()?;

        Ok(Self { keyboard, pointer, held_keys: HashSet::new(), held_buttons: HashSet::new(), wheel_rest: (0.0, 0.0) })
    }

    fn handle(&mut self, msg: &Message) -> std::io::Result<()> {
        match *msg {
            Message::Enter { x, y } => {
                debug!("pointer entered at ({x:.3}, {y:.3})");
                self.move_to(x, y)
            }
            Message::PointerTo { x, y } => self.move_to(x, y),
            Message::Leave => {
                debug!("pointer left");
                self.release_all();
                Ok(())
            }
            Message::Button { button, pressed } => {
                let code = match button {
                    MouseButton::Left => KeyCode::BTN_LEFT,
                    MouseButton::Right => KeyCode::BTN_RIGHT,
                    MouseButton::Middle => KeyCode::BTN_MIDDLE,
                    MouseButton::Back => KeyCode::BTN_SIDE,
                    MouseButton::Forward => KeyCode::BTN_EXTRA,
                };
                track(&mut self.held_buttons, code.0, pressed);
                self.pointer.emit(&[key_event(code.0, pressed)])
            }
            Message::Scroll { dx, dy } => self.scroll(dx, dy),
            Message::Key { code, pressed } => {
                track(&mut self.held_keys, code, pressed);
                self.keyboard.emit(&[key_event(code, pressed)])
            }
            _ => Ok(()),
        }
    }

    fn move_to(&mut self, x: f64, y: f64) -> std::io::Result<()> {
        let abs = |v: f64| (v.clamp(0.0, 1.0) * ABS_MAX as f64).round() as i32;
        self.pointer.emit(&[
            InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_X.0, abs(x)),
            InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Y.0, abs(y)),
        ])
    }

    fn scroll(&mut self, dx: f64, dy: f64) -> std::io::Result<()> {
        let mut events = Vec::new();
        let mut axis = |delta: f64, rest: &mut f64, hi: RelativeAxisCode, lo: RelativeAxisCode| {
            let units = (delta * HI_RES_PER_NOTCH).round();
            if units == 0.0 {
                return;
            }
            events.push(InputEvent::new(EventType::RELATIVE.0, hi.0, units as i32));
            *rest += units;
            let notches = (*rest / HI_RES_PER_NOTCH).trunc();
            if notches != 0.0 {
                *rest -= notches * HI_RES_PER_NOTCH;
                events.push(InputEvent::new(EventType::RELATIVE.0, lo.0, notches as i32));
            }
        };
        let (rx, ry) = &mut self.wheel_rest;
        axis(dy, ry, RelativeAxisCode::REL_WHEEL_HI_RES, RelativeAxisCode::REL_WHEEL);
        axis(dx, rx, RelativeAxisCode::REL_HWHEEL_HI_RES, RelativeAxisCode::REL_HWHEEL);
        if events.is_empty() { Ok(()) } else { self.pointer.emit(&events) }
    }

    /// Never leave a key or button stuck down on Ubuntu.
    fn release_all(&mut self) {
        for code in std::mem::take(&mut self.held_keys) {
            let _ = self.keyboard.emit(&[key_event(code, false)]);
        }
        for code in std::mem::take(&mut self.held_buttons) {
            let _ = self.pointer.emit(&[key_event(code, false)]);
        }
    }
}

fn key_event(code: u16, pressed: bool) -> InputEvent {
    InputEvent::new(EventType::KEY.0, code, pressed as i32)
}

fn track(set: &mut HashSet<u16>, code: u16, pressed: bool) {
    if pressed {
        set.insert(code);
    } else {
        set.remove(&code);
    }
}

/// Size of the desktop in pixels, from the connected monitors' preferred modes.
/// Monitors are assumed to sit side by side.
fn screen_size() -> Option<(u32, u32)> {
    let mut width = 0;
    let mut height = 0;
    for entry in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let dir = entry.path();
        let connected = std::fs::read_to_string(dir.join("status")).is_ok_and(|s| s.trim() == "connected");
        if !connected {
            continue;
        }
        let modes = std::fs::read_to_string(dir.join("modes")).unwrap_or_default();
        if let Some((w, h)) = modes.lines().next().and_then(|m| m.split_once('x')) {
            let h = h.trim_end_matches(|c: char| !c.is_ascii_digit());
            if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
                width += w;
                height = height.max(h);
            }
        }
    }
    (width > 0).then_some((width, height))
}
