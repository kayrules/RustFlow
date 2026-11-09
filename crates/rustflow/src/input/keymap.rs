//! macOS virtual key codes (kVK_*) → Linux evdev key codes (KEY_*).

/// Map a macOS key code. `swap_cmd_ctrl` sends Command as Ctrl and
/// Control as Super, so Mac shortcuts (Cmd+C, Cmd+V…) work on Linux.
pub fn to_linux(mac: u16, swap_cmd_ctrl: bool) -> Option<u16> {
    let code = match mac {
        0x00 => 30,  // A
        0x01 => 31,  // S
        0x02 => 32,  // D
        0x03 => 33,  // F
        0x04 => 35,  // H
        0x05 => 34,  // G
        0x06 => 44,  // Z
        0x07 => 45,  // X
        0x08 => 46,  // C
        0x09 => 47,  // V
        0x0A => 86,  // ISO § → KEY_102ND
        0x0B => 48,  // B
        0x0C => 16,  // Q
        0x0D => 17,  // W
        0x0E => 18,  // E
        0x0F => 19,  // R
        0x10 => 21,  // Y
        0x11 => 20,  // T
        0x12 => 2,   // 1
        0x13 => 3,   // 2
        0x14 => 4,   // 3
        0x15 => 5,   // 4
        0x16 => 7,   // 6
        0x17 => 6,   // 5
        0x18 => 13,  // =
        0x19 => 10,  // 9
        0x1A => 8,   // 7
        0x1B => 12,  // -
        0x1C => 9,   // 8
        0x1D => 11,  // 0
        0x1E => 27,  // ]
        0x1F => 24,  // O
        0x20 => 22,  // U
        0x21 => 26,  // [
        0x22 => 23,  // I
        0x23 => 25,  // P
        0x24 => 28,  // Return
        0x25 => 38,  // L
        0x26 => 36,  // J
        0x27 => 40,  // '
        0x28 => 37,  // K
        0x29 => 39,  // ;
        0x2A => 43,  // backslash
        0x2B => 51,  // ,
        0x2C => 53,  // /
        0x2D => 49,  // N
        0x2E => 50,  // M
        0x2F => 52,  // .
        0x30 => 15,  // Tab
        0x31 => 57,  // Space
        0x32 => 41,  // `
        0x33 => 14,  // Delete → Backspace
        0x35 => 1,   // Escape
        0x36 => if swap_cmd_ctrl { 97 } else { 126 },  // Right Command
        0x37 => if swap_cmd_ctrl { 29 } else { 125 },  // Command
        0x38 => 42,  // Shift
        0x39 => 58,  // Caps Lock
        0x3A => 56,  // Option → Alt
        0x3B => if swap_cmd_ctrl { 125 } else { 29 },  // Control
        0x3C => 54,  // Right Shift
        0x3D => 100, // Right Option → AltGr
        0x3E => if swap_cmd_ctrl { 126 } else { 97 },  // Right Control
        0x40 => 187, // F17
        0x41 => 83,  // Keypad .
        0x43 => 55,  // Keypad *
        0x45 => 78,  // Keypad +
        0x47 => 69,  // Keypad Clear → Num Lock
        0x48 => 115, // Volume Up
        0x49 => 114, // Volume Down
        0x4A => 113, // Mute
        0x4B => 98,  // Keypad /
        0x4C => 96,  // Keypad Enter
        0x4E => 74,  // Keypad -
        0x4F => 188, // F18
        0x50 => 189, // F19
        0x51 => 117, // Keypad =
        0x52 => 82,  // Keypad 0
        0x53 => 79,  // Keypad 1
        0x54 => 80,  // Keypad 2
        0x55 => 81,  // Keypad 3
        0x56 => 75,  // Keypad 4
        0x57 => 76,  // Keypad 5
        0x58 => 77,  // Keypad 6
        0x59 => 71,  // Keypad 7
        0x5A => 190, // F20
        0x5B => 72,  // Keypad 8
        0x5C => 73,  // Keypad 9
        0x5D => 124, // JIS Yen
        0x5E => 89,  // JIS Underscore → KEY_RO
        0x5F => 121, // JIS Keypad ,
        0x60 => 63,  // F5
        0x61 => 64,  // F6
        0x62 => 65,  // F7
        0x63 => 61,  // F3
        0x64 => 66,  // F8
        0x65 => 67,  // F9
        0x67 => 87,  // F11
        0x69 => 183, // F13
        0x6A => 186, // F16
        0x6B => 184, // F14
        0x6D => 68,  // F10
        0x6E => 127, // Context menu → Compose
        0x6F => 88,  // F12
        0x71 => 185, // F15
        0x72 => 110, // Help → Insert
        0x73 => 102, // Home
        0x74 => 104, // Page Up
        0x75 => 111, // Forward Delete
        0x76 => 62,  // F4
        0x77 => 107, // End
        0x78 => 60,  // F2
        0x79 => 109, // Page Down
        0x7A => 59,  // F1
        0x7B => 105, // Left
        0x7C => 106, // Right
        0x7D => 108, // Down
        0x7E => 103, // Up
        _ => return None,
    };
    Some(code)
}

/// Modifier keys arrive as FlagsChanged events; this is the flag bit each sets.
pub fn modifier_flag(mac: u16) -> Option<u64> {
    match mac {
        0x38 | 0x3C => Some(0x0002_0000), // Shift
        0x3B | 0x3E => Some(0x0004_0000), // Control
        0x3A | 0x3D => Some(0x0008_0000), // Option
        0x37 | 0x36 => Some(0x0010_0000), // Command
        0x39 => Some(0x0001_0000),        // Caps Lock
        0x3F => Some(0x0080_0000),        // Fn
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_and_swap() {
        assert_eq!(to_linux(0x00, true), Some(30)); // A
        assert_eq!(to_linux(0x37, true), Some(29)); // Cmd → Ctrl
        assert_eq!(to_linux(0x37, false), Some(125)); // Cmd → Super
        assert_eq!(to_linux(0x3B, true), Some(125)); // Control → Super
        assert_eq!(to_linux(0x3F, true), None); // Fn has no Linux key
    }
}
