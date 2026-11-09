//! Capture mouse and keyboard with a Quartz event tap.
//!
//! While the pointer is on this Mac every event passes through untouched.
//! When it is pushed past the edge facing the peer, the Mac cursor is hidden
//! and frozen, events are swallowed and forwarded, and we track where the
//! pointer is on the peer until it comes back across.

use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::display::{CGDisplay, CGPoint, CGRect};
use core_graphics::event::{
    CGEvent, CGEventField, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult, EventField,
};
use rustflow_proto::{Message, MouseButton};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::keymap;
use crate::config::{self, Side};
use crate::status::Shared;

const CONTROL: u64 = 0x0004_0000;
const OPTION: u64 = 0x0008_0000;
const COMMAND: u64 = 0x0010_0000;
const CAPS_LOCK_KEY: u16 = 0x39;
/// Trackpad scroll pixels per wheel notch on the peer.
const PIXELS_PER_NOTCH: f64 = 12.0;

pub struct State {
    cfg: config::Input,
    peer_label: String,
    enabled: AtomicBool,
    tx: mpsc::UnboundedSender<Message>,
    rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Message>>,
    shared: Shared,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Peer screen size in pixels; None until it says it can take input.
    peer_screen: Option<(f64, f64)>,
    /// Some while the pointer is on the peer.
    remote: Option<Remote>,
    /// Modifier keys (mac key codes) currently held down while remote.
    held_mods: HashSet<u16>,
}

struct Remote {
    /// Pointer position on the peer, in peer pixels.
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

pub fn start(cfg: &config::Config, shared: &Shared) -> Arc<State> {
    let (tx, rx) = mpsc::unbounded_channel();
    let state = Arc::new(State {
        cfg: cfg.input.clone(),
        peer_label: cfg.peer_label(),
        enabled: AtomicBool::new(cfg.input.enabled),
        tx,
        rx: tokio::sync::Mutex::new(rx),
        shared: shared.clone(),
        inner: Mutex::default(),
    });
    let thread_state = state.clone();
    std::thread::Builder::new()
        .name("rustflow-input".into())
        .spawn(move || run_tap(thread_state))
        .expect("spawn input thread");
    state
}

impl State {
    pub fn on_connect(&self) -> Vec<Message> {
        vec![]
    }

    pub fn on_message(&self, msg: &Message) {
        if let Message::Screen { width, height } = *msg {
            info!("peer screen is {width}x{height}");
            self.inner.lock().unwrap().peer_screen = Some((width as f64, height as f64));
            self.update_line(false, true);
        }
    }

    pub fn on_disconnect(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.peer_screen = None;
        if inner.remote.is_some() {
            self.come_back(&mut inner, None);
        }
        self.update_line(false, false);
    }

    pub async fn next_outgoing(&self) -> Option<Message> {
        self.rx.lock().await.recv().await
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
        if !on {
            let mut inner = self.inner.lock().unwrap();
            if inner.remote.is_some() {
                self.come_back(&mut inner, None);
            }
        }
        let ready = self.inner.lock().unwrap().peer_screen.is_some();
        self.update_line(false, ready);
    }

    fn send(&self, msg: Message) {
        let _ = self.tx.send(msg);
    }

    fn update_line(&self, on_peer: bool, ready: bool) {
        let line = if !self.enabled.load(Ordering::Relaxed) {
            "Mouse & keyboard: off".to_string()
        } else if on_peer {
            format!("Mouse & keyboard: on {} (Ctrl+Option+Cmd to return)", self.peer_label)
        } else if ready {
            "Mouse & keyboard: ready".to_string()
        } else {
            format!("Mouse & keyboard: waiting for {}", self.peer_label)
        };
        self.shared.set_input(line);
    }

    fn on_event(&self, etype: CGEventType, event: &CGEvent) -> CallbackResult {
        if matches!(etype, CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput) {
            reenable_tap();
            return CallbackResult::Keep;
        }
        if !self.enabled.load(Ordering::Relaxed) {
            return CallbackResult::Keep;
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.remote.is_none() {
            if is_motion(etype) {
                if let Some(screen) = inner.peer_screen {
                    if self.try_cross(&mut inner, event, screen) {
                        return CallbackResult::Drop;
                    }
                }
            }
            return CallbackResult::Keep;
        }

        match etype {
            t if is_motion(t) => {
                let dx = field(event, EventField::MOUSE_EVENT_DELTA_X) as f64 * self.cfg.speed;
                let dy = field(event, EventField::MOUSE_EVENT_DELTA_Y) as f64 * self.cfg.speed;
                let remote = inner.remote.as_mut().unwrap();
                remote.x += dx;
                remote.y = (remote.y + dy).clamp(0.0, remote.height - 1.0);
                let back = match self.cfg.peer_side {
                    Side::Left => remote.x > remote.width - 1.0,
                    Side::Right => remote.x < 0.0,
                };
                if back {
                    let frac = remote.y / remote.height;
                    self.come_back(&mut inner, Some(frac));
                } else {
                    remote.x = remote.x.clamp(0.0, remote.width - 1.0);
                    let (x, y) = (remote.x / remote.width, remote.y / remote.height);
                    self.send(Message::PointerTo { x, y });
                }
            }
            CGEventType::LeftMouseDown => self.button(MouseButton::Left, true),
            CGEventType::LeftMouseUp => self.button(MouseButton::Left, false),
            CGEventType::RightMouseDown => self.button(MouseButton::Right, true),
            CGEventType::RightMouseUp => self.button(MouseButton::Right, false),
            CGEventType::OtherMouseDown | CGEventType::OtherMouseUp => {
                let button = match field(event, EventField::MOUSE_EVENT_BUTTON_NUMBER) {
                    2 => MouseButton::Middle,
                    3 => MouseButton::Back,
                    4 => MouseButton::Forward,
                    _ => return CallbackResult::Drop,
                };
                self.button(button, matches!(etype, CGEventType::OtherMouseDown));
            }
            CGEventType::ScrollWheel => {
                let continuous = field(event, EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS) != 0;
                let (dy, dx) = if continuous {
                    (
                        field(event, EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1) as f64 / PIXELS_PER_NOTCH,
                        field(event, EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2) as f64 / PIXELS_PER_NOTCH,
                    )
                } else {
                    (
                        field(event, EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1) as f64,
                        field(event, EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2) as f64,
                    )
                };
                // macOS axis 2 is positive to the left; Linux REL_HWHEEL is positive to the right.
                self.send(Message::Scroll { dx: -dx, dy });
            }
            CGEventType::KeyDown | CGEventType::KeyUp => {
                // Linux repeats held keys itself.
                if matches!(etype, CGEventType::KeyDown) && field(event, EventField::KEYBOARD_EVENT_AUTOREPEAT) != 0 {
                    return CallbackResult::Drop;
                }
                let mac = field(event, EventField::KEYBOARD_EVENT_KEYCODE) as u16;
                if let Some(code) = keymap::to_linux(mac, self.cfg.swap_cmd_ctrl) {
                    self.send(Message::Key { code, pressed: matches!(etype, CGEventType::KeyDown) });
                }
            }
            CGEventType::FlagsChanged => {
                let flags = event.get_flags().bits();
                if flags & (CONTROL | OPTION | COMMAND) == CONTROL | OPTION | COMMAND {
                    info!("escape shortcut pressed, returning pointer");
                    self.come_back(&mut inner, None);
                    return CallbackResult::Drop;
                }
                let mac = field(event, EventField::KEYBOARD_EVENT_KEYCODE) as u16;
                self.modifier(&mut inner, mac, flags);
            }
            _ => return CallbackResult::Keep,
        }
        CallbackResult::Drop
    }

    fn button(&self, button: MouseButton, pressed: bool) {
        self.send(Message::Button { button, pressed });
    }

    fn modifier(&self, inner: &mut Inner, mac: u16, flags: u64) {
        let (Some(code), Some(bit)) = (keymap::to_linux(mac, self.cfg.swap_cmd_ctrl), keymap::modifier_flag(mac))
        else {
            return;
        };
        if mac == CAPS_LOCK_KEY {
            // Each change toggles Caps Lock: tap it on the peer.
            self.send(Message::Key { code, pressed: true });
            self.send(Message::Key { code, pressed: false });
            return;
        }
        // Left and right share a flag bit, so track which physical key is down.
        let pressed = flags & bit != 0 && !inner.held_mods.contains(&mac);
        if pressed {
            inner.held_mods.insert(mac);
        } else {
            inner.held_mods.remove(&mac);
        }
        self.send(Message::Key { code, pressed });
    }

    /// Move the pointer to the peer if it was pushed past our edge.
    fn try_cross(&self, inner: &mut Inner, event: &CGEvent, (width, height): (f64, f64)) -> bool {
        let dx = field(event, EventField::MOUSE_EVENT_DELTA_X);
        let loc = event.location();
        let Some(edge) = edge_display(self.cfg.peer_side) else { return false };
        let pushed = match self.cfg.peer_side {
            Side::Left => dx < 0 && loc.x <= edge.origin.x + 0.5,
            Side::Right => dx > 0 && loc.x >= edge.origin.x + edge.size.width - 1.5,
        };
        if !pushed || !edge.contains(&loc) && !on_edge_row(&edge, loc) {
            return false;
        }
        let frac = ((loc.y - edge.origin.y) / edge.size.height).clamp(0.0, 1.0);
        let x = match self.cfg.peer_side {
            Side::Left => width - 1.0,
            Side::Right => 0.0,
        };
        let y = frac * (height - 1.0);
        inner.remote = Some(Remote { x, y, width, height });
        inner.held_mods.clear();
        self.send(Message::Enter { x: x / width, y: y / height });
        let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(false);
        let _ = CGDisplay::main().hide_cursor();
        debug!("pointer moved to peer");
        self.update_line(true, true);
        true
    }

    /// Bring the pointer back to the Mac. `frac` is the height to return at.
    fn come_back(&self, inner: &mut Inner, frac: Option<f64>) {
        inner.remote = None;
        inner.held_mods.clear();
        self.send(Message::Leave);
        if let Some(edge) = edge_display(self.cfg.peer_side) {
            let frac = frac.unwrap_or(0.5);
            let x = match self.cfg.peer_side {
                Side::Left => edge.origin.x + 2.0,
                Side::Right => edge.origin.x + edge.size.width - 3.0,
            };
            let y = edge.origin.y + frac * (edge.size.height - 1.0);
            let _ = CGDisplay::warp_mouse_cursor_position(CGPoint::new(x, y));
        }
        let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(true);
        let _ = CGDisplay::main().show_cursor();
        debug!("pointer back on Mac");
        self.update_line(false, inner.peer_screen.is_some());
    }
}

fn is_motion(t: CGEventType) -> bool {
    matches!(
        t,
        CGEventType::MouseMoved
            | CGEventType::LeftMouseDragged
            | CGEventType::RightMouseDragged
            | CGEventType::OtherMouseDragged
    )
}

fn field(event: &CGEvent, f: CGEventField) -> i64 {
    event.get_integer_value_field(f)
}

/// The display whose edge faces the peer (leftmost or rightmost).
fn edge_display(side: Side) -> Option<CGRect> {
    let ids = CGDisplay::active_displays().ok()?;
    let rects = ids.into_iter().map(|id| CGDisplay::new(id).bounds());
    match side {
        Side::Left => rects.min_by(|a, b| a.origin.x.total_cmp(&b.origin.x)),
        Side::Right => rects.max_by(|a, b| (a.origin.x + a.size.width).total_cmp(&(b.origin.x + b.size.width))),
    }
}

fn on_edge_row(edge: &CGRect, loc: CGPoint) -> bool {
    loc.y >= edge.origin.y && loc.y < edge.origin.y + edge.size.height
}

// ---- Quartz plumbing ----

static TAP_PORT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapEnable(tap: *mut c_void, enable: bool);
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
    fn _CGSDefaultConnection() -> i32;
    fn CGSSetConnectionProperty(cid: i32, target: i32, key: CFStringRef, value: *const c_void) -> i32;
    fn CGSetLocalEventsSuppressionInterval(seconds: f64) -> i32;
}

/// macOS turns a tap off if a callback is slow; switch it back on.
fn reenable_tap() {
    let port = TAP_PORT.load(Ordering::Relaxed);
    if !port.is_null() {
        warn!("event tap was disabled by macOS, re-enabling");
        unsafe { CGEventTapEnable(port, true) };
    }
}

/// Ask for (and check) the Accessibility permission an event tap needs.
fn accessibility_trusted(prompt: bool) -> bool {
    let key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
    let opts = CFDictionary::from_CFType_pairs(&[(key, CFBoolean::from(prompt))]);
    unsafe { AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef() as *const c_void) }
}

/// Lets a background (menu bar) app hide the cursor.
fn allow_background_cursor_hiding() {
    let key = CFString::from_static_string("SetsCursorInBackground");
    unsafe {
        let cid = _CGSDefaultConnection();
        CGSSetConnectionProperty(cid, cid, key.as_concrete_TypeRef(), CFBoolean::true_value().as_CFTypeRef());
    }
}

fn run_tap(state: Arc<State>) {
    allow_background_cursor_hiding();
    // After warping the cursor back from the peer, macOS ignores the mouse
    // for 0.25 s by default, which feels like the pointer sticks at the edge.
    unsafe { CGSetLocalEventsSuppressionInterval(0.0) };
    let mut prompted = false;
    loop {
        if !accessibility_trusted(!prompted) {
            prompted = true;
            state.shared.set_input("Mouse & keyboard: allow rustflow in Settings › Accessibility".into());
            std::thread::sleep(Duration::from_secs(3));
            continue;
        }
        let events = vec![
            CGEventType::MouseMoved,
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
            CGEventType::RightMouseDown,
            CGEventType::RightMouseUp,
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseUp,
            CGEventType::LeftMouseDragged,
            CGEventType::RightMouseDragged,
            CGEventType::OtherMouseDragged,
            CGEventType::ScrollWheel,
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
        ];
        let cb_state = state.clone();
        let tap = CGEventTap::new(
            CGEventTapLocation::Session,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            events,
            move |_proxy, etype, event| cb_state.on_event(etype, event),
        );
        let Ok(tap) = tap else {
            state.shared.set_input("Mouse & keyboard: cannot capture input (check Accessibility)".into());
            std::thread::sleep(Duration::from_secs(3));
            continue;
        };
        let Ok(source) = tap.mach_port().create_runloop_source(0) else {
            warn!("cannot create run loop source for event tap");
            std::thread::sleep(Duration::from_secs(3));
            continue;
        };
        CFRunLoop::get_current().add_source(&source, unsafe { kCFRunLoopCommonModes });
        tap.enable();
        TAP_PORT.store(tap.mach_port().as_concrete_TypeRef() as *mut c_void, Ordering::Relaxed);
        info!("input capture ready");
        let ready = state.inner.lock().unwrap().peer_screen.is_some();
        state.update_line(false, ready);
        CFRunLoop::run_current();
        TAP_PORT.store(std::ptr::null_mut(), Ordering::Relaxed);
        warn!("input run loop stopped, restarting");
    }
}
