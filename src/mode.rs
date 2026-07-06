use anyhow::{anyhow, Result};

use crate::key::normalize_name;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Base,
    Text,
    NiriMomentary,
    NiriLocked,
    Passthrough,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layer {
    Base,
    Niri,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotGestureKind {
    Tap,
    Hold,
    SwipeUp,
    SwipeDown,
    SwipeLeft,
    SwipeRight,
}


impl Default for Mode {
    fn default() -> Self {
        Self::Base
    }
}

impl Default for Layer {
    fn default() -> Self {
        Self::Base
    }
}

pub(crate) fn default_layer_stack_for_mode(mode: Mode) -> Vec<Layer> {
    match mode {
        Mode::NiriMomentary | Mode::NiriLocked => vec![Layer::Niri],
        Mode::Base | Mode::Text | Mode::Passthrough => vec![Layer::Base],
    }
}

pub(crate) fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Base => "base",
        Mode::Text => "text",
        Mode::NiriMomentary => "niri-momentary",
        Mode::NiriLocked => "niri-locked",
        Mode::Passthrough => "passthrough",
    }
}

pub(crate) fn mode_hint_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Base => "BASE",
        Mode::Text => "TEXT",
        Mode::NiriMomentary => "NIRI",
        Mode::NiriLocked => "NIRI-LK",
        Mode::Passthrough => "PASS",
    }
}

pub(crate) fn mode_hint_color(mode: Mode) -> [u8; 4] {
    match mode {
        Mode::Base => [0xff, 0xff, 0xff, 0xb0],
        Mode::Text => [0x40, 0xff, 0xb0, 0xd0],
        Mode::NiriMomentary => [0x30, 0xa0, 0xff, 0xd0],
        Mode::NiriLocked => [0xff, 0x90, 0x30, 0xd8],
        Mode::Passthrough => [0xb0, 0xb0, 0xb0, 0xc0],
    }
}

pub(crate) fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Base => "base",
        Layer::Niri => "niri",
    }
}


pub(crate) fn parse_mode(value: &str) -> Result<Mode> {
    match normalize_name(value).as_str() {
        "base" => Ok(Mode::Base),
        "text" | "keyboard" => Ok(Mode::Text),
        "niri_momentary" | "niri" => Ok(Mode::NiriMomentary),
        "niri_locked" => Ok(Mode::NiriLocked),
        "passthrough" => Ok(Mode::Passthrough),
        _ => Err(anyhow!("unknown mode {value}")),
    }
}

pub(crate) fn parse_layer(value: &str) -> Result<Layer> {
    match normalize_name(value).as_str() {
        "base" => Ok(Layer::Base),
        "niri" => Ok(Layer::Niri),
        _ => Err(anyhow!("unknown layer {value}")),
    }
}

