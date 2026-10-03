//! Keyboard translation. Keys travel as USB HID usage codes (keyboard page), which is what SDL
//! scancodes are; they identify physical keys, so the host's own layout decides the character.

/// HID keyboard usage -> Linux evdev key code.
pub(crate) fn hid_to_evdev(usage: u16) -> Option<u16> {
    const LETTERS: [u16; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,
    ];
    // Usages 40..=101, in order.
    const MAIN: [u16; 62] = [
        28, 1, 14, 15, 57, 12, 13, 26, 27, 43, 43, 39, 40, 41, 51, 52, 53, 58, // Enter .. CapsLock
        59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 87, 88, // F1 .. F12
        99, 70, 119, 110, 102, 104, 111, 107, 109, 106, 105, 108, 103, // PrintScreen .. Up
        69, 98, 55, 74, 78, 96, 79, 80, 81, 75, 76, 77, 71, 72, 73, 82, 83, // NumLock, keypad
        86, 127, // non-US backslash, Menu
    ];
    const MODIFIERS: [u16; 8] = [29, 42, 56, 125, 97, 54, 100, 126];

    Some(match usage {
        4..=29 => LETTERS[usage as usize - 4],
        30..=38 => usage - 28, // 1..9
        39 => 11,              // 0
        40..=101 => MAIN[usage as usize - 40],
        224..=231 => MODIFIERS[usage as usize - 224],
        _ => return None,
    })
}

/// Linux evdev key code -> PC "set 1" scancode and whether it carries the E0 (extended) prefix.
/// The two numberings coincide for the main block; only the keys added later differ.
#[cfg(any(windows, test))]
pub(crate) fn evdev_to_set1(code: u16) -> Option<(u16, bool)> {
    Some(match code {
        69 => (0x45, true), // NumLock (unprefixed 0x45 is Pause to Windows)
        1..=88 => (code, false),
        96 => (0x1c, true),  // keypad Enter
        97 => (0x1d, true),  // right Ctrl
        98 => (0x35, true),  // keypad /
        99 => (0x37, true),  // PrintScreen
        100 => (0x38, true), // right Alt
        102 => (0x47, true), // Home
        103 => (0x48, true), // Up
        104 => (0x49, true), // PageUp
        105 => (0x4b, true), // Left
        106 => (0x4d, true), // Right
        107 => (0x4f, true), // End
        108 => (0x50, true), // Down
        109 => (0x51, true), // PageDown
        110 => (0x52, true), // Insert
        111 => (0x53, true), // Delete
        119 => (0x45, false), // Pause
        125 => (0x5b, true), // left Super
        126 => (0x5c, true), // right Super
        127 => (0x5d, true), // Menu
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_keys() {
        assert_eq!(hid_to_evdev(4), Some(30)); // A
        assert_eq!(hid_to_evdev(29), Some(44)); // Z
        assert_eq!(hid_to_evdev(30), Some(2)); // 1
        assert_eq!(hid_to_evdev(39), Some(11)); // 0
        assert_eq!(hid_to_evdev(40), Some(28)); // Enter
        assert_eq!(hid_to_evdev(44), Some(57)); // Space
        assert_eq!(hid_to_evdev(57), Some(58)); // CapsLock
        assert_eq!(hid_to_evdev(58), Some(59)); // F1
        assert_eq!(hid_to_evdev(69), Some(88)); // F12
        assert_eq!(hid_to_evdev(76), Some(111)); // Delete
        assert_eq!(hid_to_evdev(82), Some(103)); // Up
        assert_eq!(hid_to_evdev(88), Some(96)); // keypad Enter
        assert_eq!(hid_to_evdev(98), Some(82)); // keypad 0
        assert_eq!(hid_to_evdev(101), Some(127)); // Menu
        assert_eq!(hid_to_evdev(224), Some(29)); // left Ctrl
        assert_eq!(hid_to_evdev(231), Some(126)); // right Super
        assert_eq!(hid_to_evdev(0), None);
        assert_eq!(hid_to_evdev(200), None);
    }

    #[test]
    fn every_mapped_key_has_a_windows_scancode() {
        for usage in 0..256 {
            if let Some(code) = hid_to_evdev(usage) {
                assert!(evdev_to_set1(code).is_some(), "usage {} -> evdev {}", usage, code);
            }
        }
        assert_eq!(evdev_to_set1(30), Some((0x1e, false))); // A
        assert_eq!(evdev_to_set1(105), Some((0x4b, true))); // Left arrow
    }
}
