use wayland_client::protocol::wl_keyboard;
use wayland_client::WEnum;

pub(super) const RIME_SHIFT_MASK: u32 = 1 << 0;
pub(super) const RIME_CONTROL_MASK: u32 = 1 << 2;
pub(super) const RIME_ALT_MASK: u32 = 1 << 3;
pub(super) const RIME_SUPER_MASK: u32 = 1 << 26;
pub(super) const RIME_RELEASE_MASK: u32 = 1 << 30;

pub(super) const XKB_SHIFT_MASK: u32 = 1 << 0;
pub(super) const XKB_CONTROL_MASK: u32 = 1 << 2;
pub(super) const XKB_ALT_MASK: u32 = 1 << 3;
pub(super) const XKB_SUPER_MASK: u32 = 1 << 6;

pub(super) const XK_BACKSPACE: u32 = 0xff08;
pub(super) const XK_TAB: u32 = 0xff09;
pub(super) const XK_RETURN: u32 = 0xff0d;
pub(super) const XK_ESCAPE: u32 = 0xff1b;
pub(super) const XK_DELETE: u32 = 0xffff;
pub(super) const XK_HOME: u32 = 0xff50;
pub(super) const XK_LEFT: u32 = 0xff51;
pub(super) const XK_UP: u32 = 0xff52;
pub(super) const XK_RIGHT: u32 = 0xff53;
pub(super) const XK_DOWN: u32 = 0xff54;
pub(super) const XK_PAGE_UP: u32 = 0xff55;
pub(super) const XK_PAGE_DOWN: u32 = 0xff56;
pub(super) const XK_END: u32 = 0xff57;
pub(super) const XK_SHIFT_L: u32 = 0xffe1;
pub(super) const XK_SHIFT_R: u32 = 0xffe2;
pub(super) const XK_CONTROL_L: u32 = 0xffe3;
pub(super) const XK_CONTROL_R: u32 = 0xffe4;
pub(super) const XK_ALT_L: u32 = 0xffe9;
pub(super) const XK_ALT_R: u32 = 0xffea;
pub(super) const XK_SUPER_L: u32 = 0xffeb;
pub(super) const XK_SUPER_R: u32 = 0xffec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyState {
    Pressed,
    Released,
}

pub(super) fn parse_key_state(state: &str) -> Option<KeyState> {
    match state {
        "pressed" => Some(KeyState::Pressed),
        "released" => Some(KeyState::Released),
        _ => None,
    }
}

pub(super) fn parse_wayland_key_state(state: &WEnum<wl_keyboard::KeyState>) -> Option<KeyState> {
    match state {
        WEnum::Value(wl_keyboard::KeyState::Pressed) => Some(KeyState::Pressed),
        WEnum::Value(wl_keyboard::KeyState::Released) => Some(KeyState::Released),
        WEnum::Unknown(2) => Some(KeyState::Pressed),
        _ => None,
    }
}

pub(super) fn x_keycode_to_keysym(keycode: u8) -> Option<u32> {
    let evdev_key = u32::from(keycode).checked_sub(8)?;
    evdev_key_to_keysym(evdev_key)
}

pub(super) fn evdev_key_to_keysym(key: u32) -> Option<u32> {
    Some(match key {
        1 => XK_ESCAPE,
        2 => '1' as u32,
        3 => '2' as u32,
        4 => '3' as u32,
        5 => '4' as u32,
        6 => '5' as u32,
        7 => '6' as u32,
        8 => '7' as u32,
        9 => '8' as u32,
        10 => '9' as u32,
        11 => '0' as u32,
        12 => '-' as u32,
        13 => '=' as u32,
        14 => XK_BACKSPACE,
        15 => XK_TAB,
        16 => 'q' as u32,
        17 => 'w' as u32,
        18 => 'e' as u32,
        19 => 'r' as u32,
        20 => 't' as u32,
        21 => 'y' as u32,
        22 => 'u' as u32,
        23 => 'i' as u32,
        24 => 'o' as u32,
        25 => 'p' as u32,
        26 => '[' as u32,
        27 => ']' as u32,
        28 => XK_RETURN,
        29 => XK_CONTROL_L,
        30 => 'a' as u32,
        31 => 's' as u32,
        32 => 'd' as u32,
        33 => 'f' as u32,
        34 => 'g' as u32,
        35 => 'h' as u32,
        36 => 'j' as u32,
        37 => 'k' as u32,
        38 => 'l' as u32,
        39 => ';' as u32,
        40 => '\'' as u32,
        41 => '`' as u32,
        42 => XK_SHIFT_L,
        43 => '\\' as u32,
        44 => 'z' as u32,
        45 => 'x' as u32,
        46 => 'c' as u32,
        47 => 'v' as u32,
        48 => 'b' as u32,
        49 => 'n' as u32,
        50 => 'm' as u32,
        51 => ',' as u32,
        52 => '.' as u32,
        53 => '/' as u32,
        54 => XK_SHIFT_R,
        56 => XK_ALT_L,
        57 => ' ' as u32,
        97 => XK_CONTROL_R,
        100 => XK_ALT_R,
        102 => XK_HOME,
        103 => XK_UP,
        104 => XK_PAGE_UP,
        105 => XK_LEFT,
        106 => XK_RIGHT,
        107 => XK_END,
        108 => XK_DOWN,
        109 => XK_PAGE_DOWN,
        111 => XK_DELETE,
        125 => XK_SUPER_L,
        126 => XK_SUPER_R,
        _ => return None,
    })
}

pub(super) fn rime_modifier_mask(xkb_modifiers: u32) -> u32 {
    let mut mask = 0;
    if xkb_modifiers & XKB_SHIFT_MASK != 0 {
        mask |= RIME_SHIFT_MASK;
    }
    if xkb_modifiers & XKB_CONTROL_MASK != 0 {
        mask |= RIME_CONTROL_MASK;
    }
    if xkb_modifiers & XKB_ALT_MASK != 0 {
        mask |= RIME_ALT_MASK;
    }
    if xkb_modifiers & XKB_SUPER_MASK != 0 {
        mask |= RIME_SUPER_MASK;
    }
    mask
}

pub(super) fn rime_effective_keysym(keysym: u32, rime_mask: u32) -> u32 {
    keysym_to_text(keysym, rime_mask)
        .and_then(|text| {
            let mut chars = text.chars();
            let ch = chars.next()?;
            chars.next().is_none().then_some(ch as u32)
        })
        .unwrap_or(keysym)
}

pub(super) fn keysym_to_text(keysym: u32, rime_mask: u32) -> Option<String> {
    let shifted = rime_mask & RIME_SHIFT_MASK != 0;
    if (97..=122).contains(&keysym) {
        let ch = char::from_u32(keysym)?;
        return Some(if shifted { ch.to_ascii_uppercase() } else { ch }.to_string());
    }

    let ch = match keysym {
        49 => {
            if shifted {
                '!'
            } else {
                '1'
            }
        }
        50 => {
            if shifted {
                '@'
            } else {
                '2'
            }
        }
        51 => {
            if shifted {
                '#'
            } else {
                '3'
            }
        }
        52 => {
            if shifted {
                '$'
            } else {
                '4'
            }
        }
        53 => {
            if shifted {
                '%'
            } else {
                '5'
            }
        }
        54 => {
            if shifted {
                '^'
            } else {
                '6'
            }
        }
        55 => {
            if shifted {
                '&'
            } else {
                '7'
            }
        }
        56 => {
            if shifted {
                '*'
            } else {
                '8'
            }
        }
        57 => {
            if shifted {
                '('
            } else {
                '9'
            }
        }
        48 => {
            if shifted {
                ')'
            } else {
                '0'
            }
        }
        45 => {
            if shifted {
                '_'
            } else {
                '-'
            }
        }
        61 => {
            if shifted {
                '+'
            } else {
                '='
            }
        }
        91 => {
            if shifted {
                '{'
            } else {
                '['
            }
        }
        93 => {
            if shifted {
                '}'
            } else {
                ']'
            }
        }
        92 => {
            if shifted {
                '|'
            } else {
                '\\'
            }
        }
        59 => {
            if shifted {
                ':'
            } else {
                ';'
            }
        }
        39 => {
            if shifted {
                '"'
            } else {
                '\''
            }
        }
        96 => {
            if shifted {
                '~'
            } else {
                '`'
            }
        }
        44 => {
            if shifted {
                '<'
            } else {
                ','
            }
        }
        46 => {
            if shifted {
                '>'
            } else {
                '.'
            }
        }
        47 => {
            if shifted {
                '?'
            } else {
                '/'
            }
        }
        32 => ' ',
        _ => return None,
    };

    Some(ch.to_string())
}

pub(super) fn is_empty_state_passthrough_key(keysym: u32) -> bool {
    matches!(keysym, XK_BACKSPACE | XK_DELETE)
}
