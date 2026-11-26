// SPDX-License-Identifier: Apache-2.0
//! Hardware keycode → RDP (PC/XT set 1) scancode tables.
//!
//! RDP keyboard input is positional: we send the scancode of the physical key and the
//! server applies its own layout. GDK reports a platform-specific hardware keycode:
//!
//! | GDK backend | `hardware_keycode` | Table |
//! |---|---|---|
//! | Wayland, X11 | evdev code + 8 (XKB) | [`from_xkb`] |
//! | macOS | `kVK_*` virtual keycode | [`from_macos`] |
//! | Windows | virtual-key code (layout dependent) | not a table: the widget asks Windows (`MapVirtualKeyW`) |
//!
//! Keys without a mapping fall back to Unicode input in the widget.

/// An RDP scancode: set-1 code plus the E0 ("extended") prefix flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Scancode {
    pub code: u8,
    pub extended: bool,
}

const fn s(code: u8) -> Option<Scancode> {
    Some(Scancode {
        code,
        extended: false,
    })
}

const fn e(code: u8) -> Option<Scancode> {
    Some(Scancode {
        code,
        extended: true,
    })
}

/// The host platform's keycode convention, picked at compile time.
pub fn from_hardware_keycode(keycode: u32) -> Option<Scancode> {
    #[cfg(target_os = "macos")]
    {
        from_macos(keycode)
    }
    #[cfg(target_os = "windows")]
    {
        let _ = keycode;
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        from_xkb(keycode)
    }
}

/// Left/Right Win (Super, ⌘ on a Mac) become Left/Right Ctrl; other keys pass through.
pub fn command_as_ctrl(sc: Scancode) -> Scancode {
    match (sc.code, sc.extended) {
        (0x5B, true) => Scancode {
            code: 0x1D,
            extended: false,
        },
        (0x5C, true) => Scancode {
            code: 0x1D,
            extended: true,
        },
        _ => sc,
    }
}

/// XKB keycode (evdev + 8), as reported by GDK on Wayland and X11.
pub fn from_xkb(keycode: u32) -> Option<Scancode> {
    keycode.checked_sub(8).and_then(from_evdev)
}

/// Linux evdev key code (`KEY_*` in `linux/input-event-codes.h`).
pub fn from_evdev(code: u32) -> Option<Scancode> {
    match code {
        // KEY_ESC (1) .. KEY_KPDOT (83): the set-1 code equals the evdev code.
        1..=83 => s(code as u8),
        85 => s(0x76),  // KEY_ZENKAKUHANKAKU
        86 => s(0x56),  // KEY_102ND
        87 => s(0x57),  // KEY_F11
        88 => s(0x58),  // KEY_F12
        89 => s(0x73),  // KEY_RO
        90 => s(0x78),  // KEY_KATAKANA
        91 => s(0x77),  // KEY_HIRAGANA
        92 => s(0x79),  // KEY_HENKAN
        93 => s(0x70),  // KEY_KATAKANAHIRAGANA
        94 => s(0x7B),  // KEY_MUHENKAN
        95 => s(0x5C),  // KEY_KPJPCOMMA
        96 => e(0x1C),  // KEY_KPENTER
        97 => e(0x1D),  // KEY_RIGHTCTRL
        98 => e(0x35),  // KEY_KPSLASH
        99 => e(0x37),  // KEY_SYSRQ (Print Screen)
        100 => e(0x38), // KEY_RIGHTALT (AltGr)
        102 => e(0x47), // KEY_HOME
        103 => e(0x48), // KEY_UP
        104 => e(0x49), // KEY_PAGEUP
        105 => e(0x4B), // KEY_LEFT
        106 => e(0x4D), // KEY_RIGHT
        107 => e(0x4F), // KEY_END
        108 => e(0x50), // KEY_DOWN
        109 => e(0x51), // KEY_PAGEDOWN
        110 => e(0x52), // KEY_INSERT
        111 => e(0x53), // KEY_DELETE
        113 => e(0x20), // KEY_MUTE
        114 => e(0x2E), // KEY_VOLUMEDOWN
        115 => e(0x30), // KEY_VOLUMEUP
        116 => e(0x5E), // KEY_POWER
        117 => s(0x59), // KEY_KPEQUAL
        // KEY_PAUSE: FreeRDP turns (0x46, extended) into the E1 1D 45 sequence.
        119 => e(0x46),
        121 => s(0x7E), // KEY_KPCOMMA
        122 => s(0x72), // KEY_HANGEUL
        123 => s(0x71), // KEY_HANJA
        124 => s(0x7D), // KEY_YEN
        125 => e(0x5B), // KEY_LEFTMETA
        126 => e(0x5C), // KEY_RIGHTMETA
        127 => e(0x5D), // KEY_COMPOSE (Menu)
        140 => e(0x21), // KEY_CALC
        142 => e(0x5F), // KEY_SLEEP
        143 => e(0x63), // KEY_WAKEUP
        155 => e(0x6C), // KEY_MAIL
        156 => e(0x66), // KEY_BOOKMARKS
        157 => e(0x6B), // KEY_COMPUTER
        158 => e(0x6A), // KEY_BACK
        159 => e(0x69), // KEY_FORWARD
        163 => e(0x19), // KEY_NEXTSONG
        164 => e(0x22), // KEY_PLAYPAUSE
        165 => e(0x10), // KEY_PREVIOUSSONG
        166 => e(0x24), // KEY_STOPCD
        172 => e(0x32), // KEY_HOMEPAGE
        173 => e(0x67), // KEY_REFRESH
        183 => s(0x64), // KEY_F13
        184 => s(0x65), // KEY_F14
        185 => s(0x66), // KEY_F15
        186 => s(0x67), // KEY_F16
        187 => s(0x68), // KEY_F17
        188 => s(0x69), // KEY_F18
        189 => s(0x6A), // KEY_F19
        190 => s(0x6B), // KEY_F20
        191 => s(0x6C), // KEY_F21
        192 => s(0x6D), // KEY_F22
        193 => s(0x6E), // KEY_F23
        194 => s(0x76), // KEY_F24
        217 => e(0x65), // KEY_SEARCH
        226 => e(0x6D), // KEY_MEDIA
        _ => None,
    }
}

/// macOS virtual keycode (`kVK_*` in `HIToolbox/Events.h`), as reported by GDK on macOS.
pub fn from_macos(vk: u32) -> Option<Scancode> {
    match vk {
        0x00 => s(0x1E), // A
        0x01 => s(0x1F), // S
        0x02 => s(0x20), // D
        0x03 => s(0x21), // F
        0x04 => s(0x23), // H
        0x05 => s(0x22), // G
        0x06 => s(0x2C), // Z
        0x07 => s(0x2D), // X
        0x08 => s(0x2E), // C
        0x09 => s(0x2F), // V
        0x0A => s(0x56), // ISO section (102nd key)
        0x0B => s(0x30), // B
        0x0C => s(0x10), // Q
        0x0D => s(0x11), // W
        0x0E => s(0x12), // E
        0x0F => s(0x13), // R
        0x10 => s(0x15), // Y
        0x11 => s(0x14), // T
        0x12 => s(0x02), // 1
        0x13 => s(0x03), // 2
        0x14 => s(0x04), // 3
        0x15 => s(0x05), // 4
        0x16 => s(0x07), // 6
        0x17 => s(0x06), // 5
        0x18 => s(0x0D), // =
        0x19 => s(0x0A), // 9
        0x1A => s(0x08), // 7
        0x1B => s(0x0C), // -
        0x1C => s(0x09), // 8
        0x1D => s(0x0B), // 0
        0x1E => s(0x1B), // ]
        0x1F => s(0x18), // O
        0x20 => s(0x16), // U
        0x21 => s(0x1A), // [
        0x22 => s(0x17), // I
        0x23 => s(0x19), // P
        0x24 => s(0x1C), // Return
        0x25 => s(0x26), // L
        0x26 => s(0x24), // J
        0x27 => s(0x28), // '
        0x28 => s(0x25), // K
        0x29 => s(0x27), // ;
        0x2A => s(0x2B), // \
        0x2B => s(0x33), // ,
        0x2C => s(0x35), // /
        0x2D => s(0x31), // N
        0x2E => s(0x32), // M
        0x2F => s(0x34), // .
        0x30 => s(0x0F), // Tab
        0x31 => s(0x39), // Space
        0x32 => s(0x29), // `
        0x33 => s(0x0E), // Delete (Backspace)
        0x35 => s(0x01), // Escape
        0x36 => e(0x5C), // Right Command -> Right Win
        0x37 => e(0x5B), // Command -> Left Win
        0x38 => s(0x2A), // Shift
        0x39 => s(0x3A), // Caps Lock
        0x3A => s(0x38), // Option -> Alt
        0x3B => s(0x1D), // Control
        0x3C => s(0x36), // Right Shift
        0x3D => e(0x38), // Right Option -> AltGr
        0x3E => e(0x1D), // Right Control
        0x40 => s(0x68), // F17
        0x41 => s(0x53), // Keypad .
        0x43 => s(0x37), // Keypad *
        0x45 => s(0x4E), // Keypad +
        0x47 => s(0x45), // Keypad Clear -> Num Lock
        0x48 => e(0x30), // Volume Up
        0x49 => e(0x2E), // Volume Down
        0x4A => e(0x20), // Mute
        0x4B => e(0x35), // Keypad /
        0x4C => e(0x1C), // Keypad Enter
        0x4E => s(0x4A), // Keypad -
        0x4F => s(0x69), // F18
        0x50 => s(0x6A), // F19
        0x51 => s(0x59), // Keypad =
        0x52 => s(0x52), // Keypad 0
        0x53 => s(0x4F), // Keypad 1
        0x54 => s(0x50), // Keypad 2
        0x55 => s(0x51), // Keypad 3
        0x56 => s(0x4B), // Keypad 4
        0x57 => s(0x4C), // Keypad 5
        0x58 => s(0x4D), // Keypad 6
        0x59 => s(0x47), // Keypad 7
        0x5A => s(0x6B), // F20
        0x5B => s(0x48), // Keypad 8
        0x5C => s(0x49), // Keypad 9
        0x5D => s(0x7D), // JIS Yen
        0x5E => s(0x73), // JIS Underscore (Ro)
        0x5F => s(0x7E), // JIS Keypad comma
        0x60 => s(0x3F), // F5
        0x61 => s(0x40), // F6
        0x62 => s(0x41), // F7
        0x63 => s(0x3D), // F3
        0x64 => s(0x42), // F8
        0x65 => s(0x43), // F9
        0x66 => s(0x7B), // JIS Eisu -> Muhenkan
        0x67 => s(0x57), // F11
        0x68 => s(0x79), // JIS Kana -> Henkan
        0x69 => s(0x64), // F13
        0x6A => s(0x67), // F16
        0x6B => s(0x65), // F14
        0x6D => s(0x44), // F10
        0x6E => e(0x5D), // Context menu
        0x6F => s(0x58), // F12
        0x71 => s(0x66), // F15
        0x72 => e(0x52), // Help -> Insert
        0x73 => e(0x47), // Home
        0x74 => e(0x49), // Page Up
        0x75 => e(0x53), // Forward Delete
        0x76 => s(0x3E), // F4
        0x77 => e(0x4F), // End
        0x78 => s(0x3C), // F2
        0x79 => e(0x51), // Page Down
        0x7A => s(0x3B), // F1
        0x7B => e(0x4B), // Left
        0x7C => e(0x4D), // Right
        0x7D => e(0x50), // Down
        0x7E => e(0x48), // Up
        _ => None,
    }
}

/// The scancode (and whether Shift is needed) that types `c` on a **US** layout.
/// For scripted input (tests, "type text" actions); returns `None` for characters
/// a US keyboard can't type directly.
pub fn us_ascii(c: char) -> Option<(Scancode, bool)> {
    const ROWS: [(&str, &str, u8); 4] = [
        ("`1234567890-=", "~!@#$%^&*()_+", 0x29),
        ("qwertyuiop[]\\", "QWERTYUIOP{}|", 0x10),
        ("asdfghjkl;'", "ASDFGHJKL:\"", 0x1E),
        ("zxcvbnm,./", "ZXCVBNM<>?", 0x2C),
    ];
    match c {
        ' ' => return Some((s(0x39)?, false)),
        '\n' => return Some((s(0x1C)?, false)),
        '\t' => return Some((s(0x0F)?, false)),
        _ => {}
    }
    for (lower, upper, first) in ROWS {
        for (shift, set) in [(false, lower), (true, upper)] {
            if let Some(i) = set.chars().position(|x| x == c) {
                // The top row starts with ` (0x29) then 1..= (0x02..0x0D).
                let code = if first == 0x29 {
                    if i == 0 { 0x29 } else { 0x01 + i as u8 }
                } else if first == 0x10 && i == 12 {
                    0x2B // backslash
                } else {
                    first + i as u8
                };
                return Some((s(code)?, shift));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn evdev_basics() {
        assert_eq!(from_xkb(9), s(0x01)); // Escape
        assert_eq!(from_xkb(38), s(0x1E)); // A (evdev 30)
        assert_eq!(from_xkb(105), e(0x1D)); // Right Ctrl (evdev 97)
        assert_eq!(from_xkb(119), e(0x53)); // Delete (evdev 111)
        assert_eq!(from_xkb(133), e(0x5B)); // Super_L (evdev 125)
        assert_eq!(from_xkb(0), None);
        assert_eq!(from_xkb(7), None);
    }

    #[test]
    fn us_ascii_typing() {
        let sc = |c| us_ascii(c).map(|(s, sh)| (s.code, sh));
        assert_eq!(sc('a'), Some((0x1E, false)));
        assert_eq!(sc('A'), Some((0x1E, true)));
        assert_eq!(sc('1'), Some((0x02, false)));
        assert_eq!(sc('!'), Some((0x02, true)));
        assert_eq!(sc('0'), Some((0x0B, false)));
        assert_eq!(sc('`'), Some((0x29, false)));
        assert_eq!(sc('\\'), Some((0x2B, false)));
        assert_eq!(sc('|'), Some((0x2B, true)));
        assert_eq!(sc('/'), Some((0x35, false)));
        assert_eq!(sc('"'), Some((0x28, true)));
        assert_eq!(sc(' '), Some((0x39, false)));
        assert_eq!(sc('é'), None);
        // Every printable ASCII character is typeable, and agrees with evdev.
        for c in (0x20u8..0x7F).map(char::from) {
            assert!(us_ascii(c).is_some(), "{c:?}");
        }
        assert_eq!(us_ascii('q').unwrap().0, from_evdev(16).unwrap());
        assert_eq!(us_ascii('m').unwrap().0, from_evdev(50).unwrap());
    }

    #[test]
    fn command_as_ctrl_maps_only_win_keys() {
        assert_eq!(command_as_ctrl(e(0x5B).unwrap()), s(0x1D).unwrap()); // ⌘ -> LCtrl
        assert_eq!(command_as_ctrl(e(0x5C).unwrap()), e(0x1D).unwrap()); // right ⌘ -> RCtrl
        assert_eq!(command_as_ctrl(s(0x1E).unwrap()), s(0x1E).unwrap()); // A unchanged
        assert_eq!(command_as_ctrl(s(0x5B).unwrap()), s(0x5B).unwrap()); // not extended
    }

    #[test]
    fn macos_basics() {
        assert_eq!(from_macos(0x00), s(0x1E)); // A
        assert_eq!(from_macos(0x0C), s(0x10)); // Q
        assert_eq!(from_macos(0x7E), e(0x48)); // Up
        assert_eq!(from_macos(0x34), None);
    }

    /// No two physical keys may produce the same scancode, or the server could
    /// never tell them apart (and key-up tracking would break).
    #[test]
    fn tables_are_injective() {
        // F24 and Zenkaku/Hankaku share 0x76 on real PC keyboards; that's accepted.
        let allowed: &[Scancode] = &[s(0x76).unwrap()];
        for (name, f, range) in [
            (
                "evdev",
                from_evdev as fn(u32) -> Option<Scancode>,
                0..256u32,
            ),
            ("macos", from_macos, 0..128),
        ] {
            let mut seen: HashMap<Scancode, u32> = HashMap::new();
            for k in range {
                if let Some(sc) = f(k)
                    && let Some(prev) = seen.insert(sc, k)
                {
                    assert!(
                        allowed.contains(&sc),
                        "{name}: {prev} and {k} both map to {sc:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn macos_and_evdev_agree_on_letters_and_digits() {
        // kVK -> evdev for a few positional keys; both must yield the same scancode.
        let pairs = [(0x00, 30), (0x0C, 16), (0x12, 2), (0x31, 57), (0x24, 28)];
        for (mac, ev) in pairs {
            assert_eq!(
                from_macos(mac),
                from_evdev(ev),
                "kVK {mac:#x} vs evdev {ev}"
            );
        }
    }
}
