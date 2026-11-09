//! Turn Mac key presses into Linux key presses, in one of two styles.
//!
//! * **Linux style**: keys map by position, like a PC keyboard
//!   (Command = Super, Option = Alt, Control = Ctrl).
//! * **Mac style**: Mac shortcuts do on Linux what they do on a Mac. Command
//!   and Option send nothing until the next key shows what they are for:
//!   ⌘C → Ctrl+C, ⌘Tab → Alt+Tab, ⌘← → Home, ⌥← → Ctrl+←, and so on.
//!   Screenshots too: ⌘⇧3 → Shift+Print (whole screen), ⌘⇧4 / ⌘⇧5 → Print
//!   (GNOME's screenshot picker).

use std::collections::HashMap;

use super::keymap;
use crate::config::KeyboardStyle;

// macOS virtual key codes.
const MAC_CMD: [u16; 2] = [0x37, 0x36];
const MAC_OPTION: [u16; 2] = [0x3A, 0x3D];
const MAC_SHIFT: [u16; 2] = [0x38, 0x3C];
const MAC_3: u16 = 0x14;
const MAC_4: u16 = 0x15;
const MAC_5: u16 = 0x17;
const MAC_TAB: u16 = 0x30;
const MAC_GRAVE: u16 = 0x32;
const MAC_SPACE: u16 = 0x31;
const MAC_BACKSPACE: u16 = 0x33;
const MAC_FORWARD_DELETE: u16 = 0x75;
const MAC_LEFT: u16 = 0x7B;
const MAC_RIGHT: u16 = 0x7C;
const MAC_DOWN: u16 = 0x7D;
const MAC_UP: u16 = 0x7E;

// Linux evdev key codes.
const CTRL: u16 = 29;
const ALT: u16 = 56;
const SUPER: u16 = 125;
const HOME: u16 = 102;
const END: u16 = 107;
const PRINT: u16 = 99;

/// Linux key events to send: (key code, pressed).
pub type Out = Vec<(u16, bool)>;

pub struct Translator {
    style: KeyboardStyle,
    cmd_down: bool,
    option_down: bool,
    /// Linux Shift keys currently held (they pass straight through).
    shifts_held: Vec<u16>,
    /// Linux modifier currently held down on behalf of Command / Option.
    cmd_holds: Option<u16>,
    option_holds: Option<u16>,
    /// Linux keys each held Mac key produced, released when it comes up.
    pressed: HashMap<u16, Vec<u16>>,
}

impl Translator {
    pub fn new(style: KeyboardStyle) -> Self {
        Self {
            style,
            cmd_down: false,
            option_down: false,
            shifts_held: Vec::new(),
            cmd_holds: None,
            option_holds: None,
            pressed: HashMap::new(),
        }
    }

    pub fn style(&self) -> KeyboardStyle {
        self.style
    }

    /// Forget all state (the peer releases every key on its side).
    pub fn reset(&mut self, style: KeyboardStyle) {
        *self = Self::new(style);
    }

    /// A modifier key (from a FlagsChanged event) went down or up.
    pub fn modifier(&mut self, mac: u16, pressed: bool) -> Out {
        if self.style == KeyboardStyle::Mac {
            if MAC_CMD.contains(&mac) {
                self.cmd_down = pressed;
                return if pressed { vec![] } else { release(&mut self.cmd_holds) };
            }
            if MAC_OPTION.contains(&mac) {
                self.option_down = pressed;
                return if pressed { vec![] } else { release(&mut self.option_holds) };
            }
            if MAC_SHIFT.contains(&mac) {
                if let Some(code) = keymap::to_linux(mac) {
                    self.shifts_held.retain(|&s| s != code);
                    if pressed {
                        self.shifts_held.push(code);
                    }
                }
            }
        }
        keymap::to_linux(mac).map(|code| vec![(code, pressed)]).unwrap_or_default()
    }

    /// A regular key went down or up.
    pub fn key(&mut self, mac: u16, pressed: bool) -> Out {
        if !pressed {
            let keys = self.pressed.remove(&mac).unwrap_or_default();
            return keys.into_iter().rev().map(|k| (k, false)).collect();
        }
        let Some(code) = keymap::to_linux(mac) else { return vec![] };
        let mut out = Out::new();
        let keys = match self.style {
            KeyboardStyle::Mac => self.mac_shortcut(mac, code, &mut out),
            KeyboardStyle::Linux => vec![code],
        };
        out.extend(keys.iter().map(|&k| (k, true)));
        self.pressed.insert(mac, keys);
        out
    }

    /// A mouse button is about to be pressed: ⌘-click is Ctrl-click, ⌥-click is Alt-click.
    pub fn click(&mut self) -> Out {
        let mut out = Out::new();
        if self.style == KeyboardStyle::Mac {
            if self.cmd_down && self.cmd_holds.is_none() {
                hold(&mut self.cmd_holds, CTRL, &mut out);
            }
            if self.option_down && self.option_holds.is_none() {
                hold(&mut self.option_holds, ALT, &mut out);
            }
        }
        out
    }

    /// Decide what Command/Option mean for this key; returns the keys to press.
    fn mac_shortcut(&mut self, mac: u16, code: u16, out: &mut Out) -> Vec<u16> {
        if self.cmd_down {
            return match mac {
                // App and window switching: hold Alt for as long as Command is held.
                MAC_TAB | MAC_GRAVE => {
                    hold(&mut self.cmd_holds, ALT, out);
                    vec![code]
                }
                // Screenshots. Shift is already down on Linux, so ⌘⇧3 is Shift+Print.
                MAC_3 if !self.shifts_held.is_empty() => {
                    out.extend(release(&mut self.cmd_holds));
                    out.extend([(PRINT, true), (PRINT, false)]);
                    vec![]
                }
                // ⌘⇧4 / ⌘⇧5: plain Print opens the picker, so lift Shift around it.
                MAC_4 | MAC_5 if !self.shifts_held.is_empty() => {
                    out.extend(release(&mut self.cmd_holds));
                    out.extend(self.shifts_held.iter().map(|&s| (s, false)));
                    out.extend([(PRINT, true), (PRINT, false)]);
                    out.extend(self.shifts_held.iter().map(|&s| (s, true)));
                    vec![]
                }
                // ⌘Space opens search: tap Super.
                MAC_SPACE => {
                    out.extend(release(&mut self.cmd_holds));
                    out.extend([(SUPER, true), (SUPER, false)]);
                    vec![]
                }
                MAC_LEFT | MAC_RIGHT | MAC_UP | MAC_DOWN => {
                    out.extend(release(&mut self.cmd_holds));
                    match mac {
                        MAC_LEFT => vec![HOME],
                        MAC_RIGHT => vec![END],
                        MAC_UP => vec![CTRL, HOME],
                        _ => vec![CTRL, END],
                    }
                }
                _ => {
                    hold(&mut self.cmd_holds, CTRL, out);
                    vec![code]
                }
            };
        }
        if self.option_down {
            return match mac {
                // Word-wise moves and deletes.
                MAC_LEFT | MAC_RIGHT | MAC_BACKSPACE | MAC_FORWARD_DELETE => {
                    out.extend(release(&mut self.option_holds));
                    vec![CTRL, code]
                }
                _ => {
                    hold(&mut self.option_holds, ALT, out);
                    vec![code]
                }
            };
        }
        vec![code]
    }
}

/// Make `slot` hold `modifier`, swapping out a different one if needed.
fn hold(slot: &mut Option<u16>, modifier: u16, out: &mut Out) {
    match *slot {
        Some(held) if held == modifier => {}
        Some(held) => {
            out.push((held, false));
            out.push((modifier, true));
            *slot = Some(modifier);
        }
        None => {
            out.push((modifier, true));
            *slot = Some(modifier);
        }
    }
}

fn release(slot: &mut Option<u16>) -> Out {
    slot.take().map(|held| vec![(held, false)]).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: u16 = 0x08; // mac C
    const LINUX_C: u16 = 46;
    const LINUX_TAB: u16 = 15;
    const LINUX_LEFT: u16 = 105;

    fn mac() -> Translator {
        Translator::new(KeyboardStyle::Mac)
    }

    #[test]
    fn cmd_c_is_ctrl_c() {
        let mut t = mac();
        assert_eq!(t.modifier(0x37, true), vec![]);
        assert_eq!(t.key(C, true), vec![(CTRL, true), (LINUX_C, true)]);
        assert_eq!(t.key(C, false), vec![(LINUX_C, false)]);
        assert_eq!(t.modifier(0x37, false), vec![(CTRL, false)]);
    }

    #[test]
    fn cmd_tab_holds_alt_until_cmd_is_released() {
        let mut t = mac();
        t.modifier(0x37, true);
        assert_eq!(t.key(MAC_TAB, true), vec![(ALT, true), (LINUX_TAB, true)]);
        assert_eq!(t.key(MAC_TAB, false), vec![(LINUX_TAB, false)]);
        assert_eq!(t.key(MAC_TAB, true), vec![(LINUX_TAB, true)]);
        assert_eq!(t.key(MAC_TAB, false), vec![(LINUX_TAB, false)]);
        assert_eq!(t.modifier(0x37, false), vec![(ALT, false)]);
    }

    #[test]
    fn cmd_space_taps_super() {
        let mut t = mac();
        t.modifier(0x37, true);
        assert_eq!(t.key(MAC_SPACE, true), vec![(SUPER, true), (SUPER, false)]);
        assert_eq!(t.key(MAC_SPACE, false), vec![]);
        assert_eq!(t.modifier(0x37, false), vec![]);
    }

    #[test]
    fn cmd_arrows_go_to_line_and_document_ends() {
        let mut t = mac();
        t.modifier(0x37, true);
        assert_eq!(t.key(MAC_LEFT, true), vec![(HOME, true)]);
        assert_eq!(t.key(MAC_LEFT, false), vec![(HOME, false)]);
        assert_eq!(t.key(MAC_DOWN, true), vec![(CTRL, true), (END, true)]);
        assert_eq!(t.key(MAC_DOWN, false), vec![(END, false), (CTRL, false)]);
    }

    #[test]
    fn arrow_after_copy_drops_ctrl_first() {
        let mut t = mac();
        t.modifier(0x37, true);
        t.key(C, true);
        t.key(C, false);
        assert_eq!(t.key(MAC_RIGHT, true), vec![(CTRL, false), (END, true)]);
        assert_eq!(t.modifier(0x37, false), vec![]);
    }

    #[test]
    fn option_arrow_moves_by_word() {
        let mut t = mac();
        assert_eq!(t.modifier(0x3A, true), vec![]);
        assert_eq!(t.key(MAC_LEFT, true), vec![(CTRL, true), (LINUX_LEFT, true)]);
        assert_eq!(t.key(MAC_LEFT, false), vec![(LINUX_LEFT, false), (CTRL, false)]);
        assert_eq!(t.modifier(0x3A, false), vec![]);
    }

    #[test]
    fn control_stays_control_in_mac_style() {
        let mut t = mac();
        assert_eq!(t.modifier(0x3B, true), vec![(CTRL, true)]);
        assert_eq!(t.key(C, true), vec![(LINUX_C, true)]);
    }

    #[test]
    fn cmd_click_is_ctrl_click() {
        let mut t = mac();
        t.modifier(0x37, true);
        assert_eq!(t.click(), vec![(CTRL, true)]);
        assert_eq!(t.modifier(0x37, false), vec![(CTRL, false)]);
    }

    const SHIFT: u16 = 42;

    #[test]
    fn cmd_shift_3_is_shift_print() {
        let mut t = mac();
        t.modifier(0x37, true);
        assert_eq!(t.modifier(0x38, true), vec![(SHIFT, true)]);
        assert_eq!(t.key(MAC_3, true), vec![(PRINT, true), (PRINT, false)]);
        assert_eq!(t.key(MAC_3, false), vec![]);
    }

    #[test]
    fn cmd_shift_4_is_plain_print() {
        let mut t = mac();
        t.modifier(0x37, true);
        t.modifier(0x38, true);
        assert_eq!(t.key(MAC_4, true), vec![(SHIFT, false), (PRINT, true), (PRINT, false), (SHIFT, true)]);
        assert_eq!(t.modifier(0x38, false), vec![(SHIFT, false)]);
        assert_eq!(t.modifier(0x37, false), vec![]);
    }

    #[test]
    fn cmd_3_without_shift_is_ctrl_3() {
        let mut t = mac();
        t.modifier(0x37, true);
        assert_eq!(t.key(MAC_3, true), vec![(CTRL, true), (4, true)]);
    }

    #[test]
    fn linux_style_maps_by_position() {
        let mut t = Translator::new(KeyboardStyle::Linux);
        assert_eq!(t.modifier(0x37, true), vec![(SUPER, true)]);
        assert_eq!(t.key(MAC_TAB, true), vec![(LINUX_TAB, true)]);
        assert_eq!(t.modifier(0x3A, true), vec![(ALT, true)]);
        assert_eq!(t.click(), vec![]);
    }
}
