use anyhow::{anyhow, Result};

pub(crate) const KEY_ESC: u32 = 1;
pub(crate) const KEY_1: u32 = 2;
pub(crate) const KEY_2: u32 = 3;
pub(crate) const KEY_3: u32 = 4;
pub(crate) const KEY_4: u32 = 5;
pub(crate) const KEY_5: u32 = 6;
pub(crate) const KEY_6: u32 = 7;
pub(crate) const KEY_7: u32 = 8;
pub(crate) const KEY_8: u32 = 9;
pub(crate) const KEY_9: u32 = 10;
pub(crate) const KEY_0: u32 = 11;
pub(crate) const KEY_MINUS: u32 = 12;
pub(crate) const KEY_EQUAL: u32 = 13;
pub(crate) const KEY_BACKSPACE: u32 = 14;
pub(crate) const KEY_TAB: u32 = 15;
pub(crate) const KEY_LEFTCTRL: u32 = 29;
pub(crate) const KEY_Q: u32 = 16;
pub(crate) const KEY_W: u32 = 17;
pub(crate) const KEY_E: u32 = 18;
pub(crate) const KEY_R: u32 = 19;
pub(crate) const KEY_T: u32 = 20;
pub(crate) const KEY_Y: u32 = 21;
pub(crate) const KEY_U: u32 = 22;
pub(crate) const KEY_I: u32 = 23;
pub(crate) const KEY_O: u32 = 24;
pub(crate) const KEY_P: u32 = 25;
pub(crate) const KEY_LEFTBRACE: u32 = 26;
pub(crate) const KEY_RIGHTBRACE: u32 = 27;
pub(crate) const KEY_ENTER: u32 = 28;
pub(crate) const KEY_A: u32 = 30;
pub(crate) const KEY_S: u32 = 31;
pub(crate) const KEY_D: u32 = 32;
pub(crate) const KEY_F: u32 = 33;
pub(crate) const KEY_G: u32 = 34;
pub(crate) const KEY_H: u32 = 35;
pub(crate) const KEY_J: u32 = 36;
pub(crate) const KEY_K: u32 = 37;
pub(crate) const KEY_L: u32 = 38;
pub(crate) const KEY_SEMICOLON: u32 = 39;
pub(crate) const KEY_APOSTROPHE: u32 = 40;
pub(crate) const KEY_GRAVE: u32 = 41;
pub(crate) const KEY_Z: u32 = 44;
pub(crate) const KEY_BACKSLASH: u32 = 43;
pub(crate) const KEY_LEFTSHIFT: u32 = 42;
pub(crate) const KEY_RIGHTSHIFT: u32 = 54;
pub(crate) const KEY_X: u32 = 45;
pub(crate) const KEY_C: u32 = 46;
pub(crate) const KEY_V: u32 = 47;
pub(crate) const KEY_B: u32 = 48;
pub(crate) const KEY_N: u32 = 49;
pub(crate) const KEY_M: u32 = 50;
pub(crate) const KEY_COMMA: u32 = 51;
pub(crate) const KEY_DOT: u32 = 52;
pub(crate) const KEY_SLASH: u32 = 53;
pub(crate) const KEY_SPACE: u32 = 57;
pub(crate) const KEY_LEFTALT: u32 = 56;
pub(crate) const KEY_RIGHTCTRL: u32 = 97;
pub(crate) const KEY_RIGHTALT: u32 = 100;
pub(crate) const KEY_LEFT: u32 = 105;
pub(crate) const KEY_RIGHT: u32 = 106;
pub(crate) const KEY_UP: u32 = 103;
pub(crate) const KEY_DOWN: u32 = 108;
pub(crate) const KEY_HOME: u32 = 102;
pub(crate) const KEY_PAGEUP: u32 = 104;
pub(crate) const KEY_END: u32 = 107;
pub(crate) const KEY_PAGEDOWN: u32 = 109;
pub(crate) const KEY_INSERT: u32 = 110;
pub(crate) const KEY_DELETE: u32 = 111;
pub(crate) const KEY_LEFTMETA: u32 = 125;
pub(crate) const KEY_RIGHTMETA: u32 = 126;

pub(crate) const XKB_MOD_SHIFT: u32 = 1 << 0;
pub(crate) const XKB_MOD_CONTROL: u32 = 1 << 2;
pub(crate) const XKB_MOD_ALT: u32 = 1 << 3;
pub(crate) const XKB_MOD_SUPER: u32 = 1 << 6;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyChord {
    pub(crate) keys: Vec<u32>,
}

pub(crate) fn parse_single_key(value: &str) -> Result<u32> {
    let sequence = parse_key_sequence(value)?;
    if sequence.len() != 1 || sequence[0].keys.len() != 1 {
        return Err(anyhow!("expected a single key, got {value}"));
    }
    Ok(sequence[0].keys[0])
}

pub(crate) fn parse_key_sequence(value: &str) -> Result<Vec<KeyChord>> {
    let sequence = value
        .split_whitespace()
        .map(parse_key_chord)
        .collect::<Result<Vec<_>>>()?;

    if sequence.is_empty() {
        Err(anyhow!("empty key sequence"))
    } else {
        Ok(sequence)
    }
}

pub(crate) fn parse_key_chord(token: &str) -> Result<KeyChord> {
    let keys = parse_zmk_key_expr(token)?;
    Ok(KeyChord { keys })
}

pub(crate) fn parse_zmk_key_expr(value: &str) -> Result<Vec<u32>> {
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow!("empty ZMK key token"));
    }

    if let Some((modifier, inner)) = parse_zmk_modifier_call(value)? {
        let mut keys = parse_zmk_key_expr(inner)?;
        if !keys.contains(&modifier) {
            keys.insert(0, modifier);
        }
        return Ok(keys);
    }

    let (key, implicit_modifiers) =
        parse_zmk_key_name(value).ok_or_else(|| anyhow!("unknown ZMK key token {value}"))?;
    let mut keys = implicit_modifiers;
    if !keys.contains(&key) {
        keys.push(key);
    }
    Ok(keys)
}

pub(crate) fn parse_zmk_modifier_call(value: &str) -> Result<Option<(u32, &str)>> {
    let Some(open) = value.find('(') else {
        return Ok(None);
    };
    if !value.ends_with(')') {
        return Err(anyhow!(
            "invalid ZMK modifier expression {value}; expected MOD(KEY)"
        ));
    }
    let name = &value[..open];
    let inner = &value[open + 1..value.len() - 1];
    let modifier = match name {
        "LC" => KEY_LEFTCTRL,
        "LS" => KEY_LEFTSHIFT,
        "LA" => KEY_LEFTALT,
        "LG" => KEY_LEFTMETA,
        "RC" => KEY_RIGHTCTRL,
        "RS" => KEY_RIGHTSHIFT,
        "RA" => KEY_RIGHTALT,
        "RG" => KEY_RIGHTMETA,
        _ => {
            return Err(anyhow!(
                "unknown ZMK modifier wrapper {name}; expected LC/LS/LA/LG/RC/RS/RA/RG"
            ))
        }
    };
    Ok(Some((modifier, inner)))
}

pub(crate) fn parse_zmk_key_name(value: &str) -> Option<(u32, Vec<u32>)> {
    let shifted = |key| Some((key, vec![KEY_LEFTSHIFT]));
    let plain = |key| Some((key, Vec::new()));

    match value.trim() {
        "A" => plain(KEY_A),
        "B" => plain(KEY_B),
        "C" => plain(KEY_C),
        "D" => plain(KEY_D),
        "E" => plain(KEY_E),
        "F" => plain(KEY_F),
        "G" => plain(KEY_G),
        "H" => plain(KEY_H),
        "I" => plain(KEY_I),
        "J" => plain(KEY_J),
        "K" => plain(KEY_K),
        "L" => plain(KEY_L),
        "M" => plain(KEY_M),
        "N" => plain(KEY_N),
        "O" => plain(KEY_O),
        "P" => plain(KEY_P),
        "Q" => plain(KEY_Q),
        "R" => plain(KEY_R),
        "S" => plain(KEY_S),
        "T" => plain(KEY_T),
        "U" => plain(KEY_U),
        "V" => plain(KEY_V),
        "W" => plain(KEY_W),
        "X" => plain(KEY_X),
        "Y" => plain(KEY_Y),
        "Z" => plain(KEY_Z),
        "N1" | "NUMBER_1" => plain(KEY_1),
        "N2" | "NUMBER_2" => plain(KEY_2),
        "N3" | "NUMBER_3" => plain(KEY_3),
        "N4" | "NUMBER_4" => plain(KEY_4),
        "N5" | "NUMBER_5" => plain(KEY_5),
        "N6" | "NUMBER_6" => plain(KEY_6),
        "N7" | "NUMBER_7" => plain(KEY_7),
        "N8" | "NUMBER_8" => plain(KEY_8),
        "N9" | "NUMBER_9" => plain(KEY_9),
        "N0" | "NUMBER_0" => plain(KEY_0),
        "EXCLAMATION" | "EXCL" | "BANG" => shifted(KEY_1),
        "AT_SIGN" | "AT" => shifted(KEY_2),
        "HASH" | "POUND" => shifted(KEY_3),
        "DOLLAR" => shifted(KEY_4),
        "PERCENT" => shifted(KEY_5),
        "CARET" => shifted(KEY_6),
        "AMPERSAND" | "AMPS" => shifted(KEY_7),
        "ASTERISK" | "STAR" => shifted(KEY_8),
        "LEFT_PARENTHESIS" | "LPAR" => shifted(KEY_9),
        "RIGHT_PARENTHESIS" | "RPAR" => shifted(KEY_0),
        "RET" | "RETURN" | "ENTER" => plain(KEY_ENTER),
        "ESC" | "ESCAPE" => plain(KEY_ESC),
        "BACKSPACE" | "BSPC" | "BKSP" => plain(KEY_BACKSPACE),
        "TAB" => plain(KEY_TAB),
        "SPACE" | "SPC" => plain(KEY_SPACE),
        "MINUS" => plain(KEY_MINUS),
        "UNDERSCORE" | "UNDER" => shifted(KEY_MINUS),
        "EQUAL" | "EQL" => plain(KEY_EQUAL),
        "PLUS" => shifted(KEY_EQUAL),
        "LEFT_BRACKET" | "LBKT" => plain(KEY_LEFTBRACE),
        "LEFT_BRACE" | "LBRC" => shifted(KEY_LEFTBRACE),
        "RIGHT_BRACKET" | "RBKT" => plain(KEY_RIGHTBRACE),
        "RIGHT_BRACE" | "RBRC" => shifted(KEY_RIGHTBRACE),
        "BACKSLASH" | "BSLH" => plain(KEY_BACKSLASH),
        "PIPE" => shifted(KEY_BACKSLASH),
        "SEMICOLON" | "SEMI" => plain(KEY_SEMICOLON),
        "COLON" => shifted(KEY_SEMICOLON),
        "SINGLE_QUOTE" | "SQT" | "APOSTROPHE" => plain(KEY_APOSTROPHE),
        "DOUBLE_QUOTES" | "DQT" => shifted(KEY_APOSTROPHE),
        "GRAVE" => plain(KEY_GRAVE),
        "TILDE" => shifted(KEY_GRAVE),
        "COMMA" => plain(KEY_COMMA),
        "LESS_THAN" | "LT" => shifted(KEY_COMMA),
        "PERIOD" | "DOT" => plain(KEY_DOT),
        "GREATER_THAN" | "GT" => shifted(KEY_DOT),
        "SLASH" | "FSLH" => plain(KEY_SLASH),
        "QUESTION" | "QMARK" => shifted(KEY_SLASH),
        "DELETE" | "DEL" => plain(KEY_DELETE),
        "INSERT" | "INS" => plain(KEY_INSERT),
        "HOME" => plain(KEY_HOME),
        "END" => plain(KEY_END),
        "PAGE_UP" | "PG_UP" | "PGUP" => plain(KEY_PAGEUP),
        "PAGE_DOWN" | "PG_DN" | "PGDN" => plain(KEY_PAGEDOWN),
        "LEFT" | "LEFT_ARROW" => plain(KEY_LEFT),
        "RIGHT" | "RIGHT_ARROW" => plain(KEY_RIGHT),
        "UP" | "UP_ARROW" => plain(KEY_UP),
        "DOWN" | "DOWN_ARROW" => plain(KEY_DOWN),
        "LCTRL" | "LEFT_CONTROL" => plain(KEY_LEFTCTRL),
        "LSHIFT" | "LEFT_SHIFT" => plain(KEY_LEFTSHIFT),
        "LALT" | "LEFT_ALT" => plain(KEY_LEFTALT),
        "LGUI" | "LEFT_GUI" | "LEFT_WIN" | "LEFT_META" => plain(KEY_LEFTMETA),
        "RCTRL" | "RIGHT_CONTROL" => plain(KEY_RIGHTCTRL),
        "RSHIFT" | "RIGHT_SHIFT" => plain(KEY_RIGHTSHIFT),
        "RALT" | "RIGHT_ALT" => plain(KEY_RIGHTALT),
        "RGUI" | "RIGHT_GUI" | "RIGHT_WIN" | "RIGHT_META" => plain(KEY_RIGHTMETA),
        _ => None,
    }
}

pub(crate) fn key_sequence_label(sequence: &[KeyChord]) -> Option<String> {
    let labels = sequence
        .iter()
        .map(key_chord_label)
        .collect::<Option<Vec<_>>>()?;
    Some(labels.join(" "))
}

pub(crate) fn key_chord_label(chord: &KeyChord) -> Option<String> {
    let base = *chord.keys.last()?;
    let mut modifiers = chord.keys[..chord.keys.len().saturating_sub(1)].to_vec();

    let mut label = if remove_modifier(&mut modifiers, KEY_LEFTSHIFT) {
        shifted_zmk_key_label(base)
            .map(str::to_string)
            .unwrap_or_else(|| format!("LS({})", key_code_label(base).unwrap_or("?")))
    } else {
        key_code_label(base)?.to_string()
    };

    for modifier in modifiers.into_iter().rev() {
        let wrapper = zmk_modifier_wrapper_label(modifier)?;
        label = format!("{wrapper}({label})");
    }

    Some(label)
}

pub(crate) fn remove_modifier(modifiers: &mut Vec<u32>, key: u32) -> bool {
    if let Some(index) = modifiers.iter().position(|modifier| *modifier == key) {
        modifiers.remove(index);
        true
    } else {
        false
    }
}

pub(crate) fn shifted_zmk_key_label(key: u32) -> Option<&'static str> {
    match key {
        KEY_1 => Some("EXCLAMATION"),
        KEY_2 => Some("AT_SIGN"),
        KEY_3 => Some("HASH"),
        KEY_4 => Some("DOLLAR"),
        KEY_5 => Some("PERCENT"),
        KEY_6 => Some("CARET"),
        KEY_7 => Some("AMPERSAND"),
        KEY_8 => Some("ASTERISK"),
        KEY_9 => Some("LEFT_PARENTHESIS"),
        KEY_0 => Some("RIGHT_PARENTHESIS"),
        KEY_MINUS => Some("UNDERSCORE"),
        KEY_EQUAL => Some("PLUS"),
        KEY_LEFTBRACE => Some("LEFT_BRACE"),
        KEY_RIGHTBRACE => Some("RIGHT_BRACE"),
        KEY_SEMICOLON => Some("COLON"),
        KEY_APOSTROPHE => Some("DOUBLE_QUOTES"),
        KEY_GRAVE => Some("TILDE"),
        KEY_BACKSLASH => Some("PIPE"),
        KEY_COMMA => Some("LESS_THAN"),
        KEY_DOT => Some("GREATER_THAN"),
        KEY_SLASH => Some("QUESTION"),
        _ => None,
    }
}

pub(crate) fn zmk_modifier_wrapper_label(key: u32) -> Option<&'static str> {
    match key {
        KEY_LEFTCTRL => Some("LC"),
        KEY_LEFTSHIFT => Some("LS"),
        KEY_LEFTALT => Some("LA"),
        KEY_LEFTMETA => Some("LG"),
        KEY_RIGHTCTRL => Some("RC"),
        KEY_RIGHTSHIFT => Some("RS"),
        KEY_RIGHTALT => Some("RA"),
        KEY_RIGHTMETA => Some("RG"),
        _ => None,
    }
}

pub(crate) fn modifier_mask_for_key(key: u32) -> Option<u32> {
    match key {
        KEY_LEFTSHIFT | KEY_RIGHTSHIFT => Some(XKB_MOD_SHIFT),
        KEY_LEFTCTRL | KEY_RIGHTCTRL => Some(XKB_MOD_CONTROL),
        KEY_LEFTALT | KEY_RIGHTALT => Some(XKB_MOD_ALT),
        KEY_LEFTMETA | KEY_RIGHTMETA => Some(XKB_MOD_SUPER),
        _ => None,
    }
}

pub(crate) fn key_code_label(key: u32) -> Option<&'static str> {
    match key {
        KEY_LEFTCTRL => Some("LCTRL"),
        KEY_RIGHTCTRL => Some("RCTRL"),
        KEY_LEFTSHIFT => Some("LSHIFT"),
        KEY_RIGHTSHIFT => Some("RSHIFT"),
        KEY_LEFTALT => Some("LALT"),
        KEY_RIGHTALT => Some("RALT"),
        KEY_LEFTMETA => Some("LGUI"),
        KEY_RIGHTMETA => Some("RGUI"),
        KEY_ESC => Some("ESC"),
        KEY_ENTER => Some("RET"),
        KEY_BACKSPACE => Some("BSPC"),
        KEY_DELETE => Some("DELETE"),
        KEY_TAB => Some("TAB"),
        KEY_SPACE => Some("SPC"),
        KEY_HOME => Some("HOME"),
        KEY_END => Some("END"),
        KEY_PAGEUP => Some("PAGE_UP"),
        KEY_PAGEDOWN => Some("PAGE_DOWN"),
        KEY_INSERT => Some("INSERT"),
        KEY_LEFT => Some("LEFT"),
        KEY_RIGHT => Some("RIGHT"),
        KEY_UP => Some("UP"),
        KEY_DOWN => Some("DOWN"),
        KEY_1 => Some("N1"),
        KEY_2 => Some("N2"),
        KEY_3 => Some("N3"),
        KEY_4 => Some("N4"),
        KEY_5 => Some("N5"),
        KEY_6 => Some("N6"),
        KEY_7 => Some("N7"),
        KEY_8 => Some("N8"),
        KEY_9 => Some("N9"),
        KEY_0 => Some("N0"),
        KEY_MINUS => Some("MINUS"),
        KEY_EQUAL => Some("EQUAL"),
        KEY_LEFTBRACE => Some("LEFT_BRACKET"),
        KEY_RIGHTBRACE => Some("RIGHT_BRACKET"),
        KEY_SEMICOLON => Some("SEMICOLON"),
        KEY_APOSTROPHE => Some("SINGLE_QUOTE"),
        KEY_GRAVE => Some("GRAVE"),
        KEY_BACKSLASH => Some("BACKSLASH"),
        KEY_COMMA => Some("COMMA"),
        KEY_DOT => Some("PERIOD"),
        KEY_SLASH => Some("SLASH"),
        KEY_A => Some("A"),
        KEY_B => Some("B"),
        KEY_C => Some("C"),
        KEY_D => Some("D"),
        KEY_E => Some("E"),
        KEY_F => Some("F"),
        KEY_G => Some("G"),
        KEY_H => Some("H"),
        KEY_I => Some("I"),
        KEY_J => Some("J"),
        KEY_K => Some("K"),
        KEY_L => Some("L"),
        KEY_M => Some("M"),
        KEY_N => Some("N"),
        KEY_O => Some("O"),
        KEY_P => Some("P"),
        KEY_Q => Some("Q"),
        KEY_R => Some("R"),
        KEY_S => Some("S"),
        KEY_T => Some("T"),
        KEY_U => Some("U"),
        KEY_V => Some("V"),
        KEY_W => Some("W"),
        KEY_X => Some("X"),
        KEY_Y => Some("Y"),
        KEY_Z => Some("Z"),
        _ => None,
    }
}

pub(crate) fn normalize_name(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn zmk_key_parser_supports_named_symbols() {
        assert_eq!(
            parse_key_sequence("MINUS").unwrap(),
            vec![KeyChord {
                keys: vec![KEY_MINUS],
            }]
        );
        assert_eq!(
            parse_key_sequence("EXCLAMATION").unwrap(),
            vec![KeyChord {
                keys: vec![KEY_LEFTSHIFT, KEY_1],
            }]
        );
    }
}
