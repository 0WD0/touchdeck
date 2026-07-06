use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
#[derive(Clone, Debug)]
pub(super) struct ImeRuntimeConfig {
    pub(super) key_translation: KeyTranslationPolicy,
    pub(super) popup: PopupConfig,
}

impl Default for ImeRuntimeConfig {
    fn default() -> Self {
        Self {
            key_translation: KeyTranslationPolicy::Effective,
            popup: PopupConfig::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyTranslationPolicy {
    Effective,
    Raw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyRoute {
    ImeKey,
    ImeText,
    AppKey,
    ImeOnly,
}

#[derive(Clone, Debug)]
pub(super) struct PopupConfig {
    pub(super) width: u32,
    pub(super) max_candidates: usize,
    pub(super) height_empty: u32,
    pub(super) height_candidates: u32,
    pub(super) header_height: i32,
    pub(super) padding_x: i32,
    pub(super) header_y: i32,
    pub(super) candidate_gap: i32,
    pub(super) candidate_min_width: i32,
    pub(super) candidate_max_width: i32,
    pub(super) candidate_unit_width: i32,
    pub(super) candidate_extra_width: i32,
    pub(super) preedit_font_size: f32,
    pub(super) candidate_font_size: f32,
    pub(super) background_color: Rgba,
    pub(super) border_color: Rgba,
    pub(super) separator_color: Rgba,
    pub(super) preedit_color: Rgba,
    pub(super) candidate_text_color: Rgba,
    pub(super) highlight_background_color: Rgba,
    pub(super) first_candidate_background_color: Rgba,
}

impl Default for PopupConfig {
    fn default() -> Self {
        Self {
            width: 560,
            max_candidates: 6,
            height_empty: 48,
            height_candidates: 88,
            header_height: 32,
            padding_x: 10,
            header_y: 5,
            candidate_gap: 6,
            candidate_min_width: 48,
            candidate_max_width: 154,
            candidate_unit_width: 8,
            candidate_extra_width: 26,
            preedit_font_size: 15.5,
            candidate_font_size: 16.0,
            background_color: Rgba::new(0x1a, 0x22, 0x26, 0xe6),
            border_color: Rgba::new(0x79, 0x8b, 0x86, 0x96),
            separator_color: Rgba::new(0x6c, 0x78, 0x72, 0x70),
            preedit_color: Rgba::new(0xd8, 0xde, 0xe8, 0xee),
            candidate_text_color: Rgba::new(0xff, 0xff, 0xff, 0xf0),
            highlight_background_color: Rgba::new(0x3b, 0x86, 0xf2, 0xdc),
            first_candidate_background_color: Rgba::new(0x2e, 0x3d, 0x44, 0x70),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Rgba {
    pub(super) r: u8,
    pub(super) g: u8,
    pub(super) b: u8,
    pub(super) a: u8,
}

impl Rgba {
    pub(super) const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub(super) fn rgba(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    pub(super) fn bgra(self) -> [u8; 4] {
        [self.b, self.g, self.r, self.a]
    }
}

pub(super) fn load_ime_config() -> Result<ImeRuntimeConfig> {
    let Some(path) = config_path() else {
        return Ok(ImeRuntimeConfig::default());
    };
    let source = fs::read_to_string(&path)
        .with_context(|| format!("read touchdeck config {}", path.display()))?;
    let file_config: TouchDeckImeConfigFile = toml::from_str(&source)
        .with_context(|| format!("parse touchdeck config {}", path.display()))?;
    let mut config = ImeRuntimeConfig::default();
    if let Some(ime) = file_config.ime {
        if let Some(policy) = ime.key_translation {
            config.key_translation = parse_key_translation_policy(&policy)?;
        }
        if let Some(popup) = ime.popup {
            config.popup.apply(popup)?;
        }
    }
    Ok(config)
}

pub(super) fn config_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("TOUCHDECK_CONFIG") {
        return Some(PathBuf::from(path));
    }
    let default_path = PathBuf::from("touchdeck.toml");
    default_path.exists().then_some(default_path)
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct TouchDeckImeConfigFile {
    pub(super) ime: Option<ImeConfigFile>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct ImeConfigFile {
    pub(super) key_translation: Option<String>,
    pub(super) popup: Option<PopupConfigFile>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct PopupConfigFile {
    pub(super) width: Option<u32>,
    pub(super) max_candidates: Option<usize>,
    pub(super) height_empty: Option<u32>,
    pub(super) height_candidates: Option<u32>,
    pub(super) header_height: Option<i32>,
    pub(super) padding_x: Option<i32>,
    pub(super) header_y: Option<i32>,
    pub(super) candidate_gap: Option<i32>,
    pub(super) candidate_min_width: Option<i32>,
    pub(super) candidate_max_width: Option<i32>,
    pub(super) candidate_unit_width: Option<i32>,
    pub(super) candidate_extra_width: Option<i32>,
    pub(super) preedit_font_size: Option<f32>,
    pub(super) candidate_font_size: Option<f32>,
    pub(super) background_color: Option<String>,
    pub(super) border_color: Option<String>,
    pub(super) separator_color: Option<String>,
    pub(super) preedit_color: Option<String>,
    pub(super) candidate_text_color: Option<String>,
    pub(super) highlight_background_color: Option<String>,
    pub(super) first_candidate_background_color: Option<String>,
}

pub(super) fn parse_key_translation_policy(value: &str) -> Result<KeyTranslationPolicy> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "effective" | "effective_keysym" | "translated" => Ok(KeyTranslationPolicy::Effective),
        "raw" | "raw_keysym" | "base" => Ok(KeyTranslationPolicy::Raw),
        other => Err(anyhow!("unknown ime.key_translation {other:?}")),
    }
}

pub(super) fn parse_key_route(value: &str) -> Result<KeyRoute> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "ime" | "ime_key" | "ime_first" | "rime" | "rime_first" => Ok(KeyRoute::ImeKey),
        "ime_text" | "text" | "commit_text" => Ok(KeyRoute::ImeText),
        "app" | "app_key" | "direct" | "passthrough" | "forward" => Ok(KeyRoute::AppKey),
        "ime_only" | "rime_only" | "consume" | "filter" => Ok(KeyRoute::ImeOnly),
        other => Err(anyhow!("unknown key route {other:?}")),
    }
}

impl PopupConfig {
    fn apply(&mut self, value: PopupConfigFile) -> Result<()> {
        if let Some(width) = value.width {
            self.width = width;
        }
        if let Some(max_candidates) = value.max_candidates {
            self.max_candidates = max_candidates.max(1);
        }
        if let Some(height_empty) = value.height_empty {
            self.height_empty = height_empty;
        }
        if let Some(height_candidates) = value.height_candidates {
            self.height_candidates = height_candidates;
        }
        if let Some(header_height) = value.header_height {
            self.header_height = header_height.max(1);
        }
        if let Some(padding_x) = value.padding_x {
            self.padding_x = padding_x.max(0);
        }
        if let Some(header_y) = value.header_y {
            self.header_y = header_y.max(0);
        }
        if let Some(candidate_gap) = value.candidate_gap {
            self.candidate_gap = candidate_gap.max(0);
        }
        if let Some(candidate_min_width) = value.candidate_min_width {
            self.candidate_min_width = candidate_min_width.max(1);
        }
        if let Some(candidate_max_width) = value.candidate_max_width {
            self.candidate_max_width = candidate_max_width.max(self.candidate_min_width);
        }
        if let Some(candidate_unit_width) = value.candidate_unit_width {
            self.candidate_unit_width = candidate_unit_width.max(1);
        }
        if let Some(candidate_extra_width) = value.candidate_extra_width {
            self.candidate_extra_width = candidate_extra_width.max(0);
        }
        if let Some(preedit_font_size) = value.preedit_font_size {
            self.preedit_font_size = preedit_font_size.max(1.0);
        }
        if let Some(candidate_font_size) = value.candidate_font_size {
            self.candidate_font_size = candidate_font_size.max(1.0);
        }
        if let Some(color) = value.background_color {
            self.background_color = parse_hex_color(&color, "ime.popup.background_color")?;
        }
        if let Some(color) = value.border_color {
            self.border_color = parse_hex_color(&color, "ime.popup.border_color")?;
        }
        if let Some(color) = value.separator_color {
            self.separator_color = parse_hex_color(&color, "ime.popup.separator_color")?;
        }
        if let Some(color) = value.preedit_color {
            self.preedit_color = parse_hex_color(&color, "ime.popup.preedit_color")?;
        }
        if let Some(color) = value.candidate_text_color {
            self.candidate_text_color = parse_hex_color(&color, "ime.popup.candidate_text_color")?;
        }
        if let Some(color) = value.highlight_background_color {
            self.highlight_background_color =
                parse_hex_color(&color, "ime.popup.highlight_background_color")?;
        }
        if let Some(color) = value.first_candidate_background_color {
            self.first_candidate_background_color =
                parse_hex_color(&color, "ime.popup.first_candidate_background_color")?;
        }
        Ok(())
    }
}

pub(super) fn parse_hex_color(value: &str, name: &str) -> Result<Rgba> {
    let hex = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if hex.len() != 6 && hex.len() != 8 {
        return Err(anyhow!(
            "{name} must be #RRGGBB or #RRGGBBAA, got {value:?}"
        ));
    }
    let r = parse_hex_byte(hex, 0, name)?;
    let g = parse_hex_byte(hex, 2, name)?;
    let b = parse_hex_byte(hex, 4, name)?;
    let a = if hex.len() == 8 {
        parse_hex_byte(hex, 6, name)?
    } else {
        0xff
    };
    Ok(Rgba::new(r, g, b, a))
}

pub(super) fn parse_hex_byte(hex: &str, start: usize, name: &str) -> Result<u8> {
    u8::from_str_radix(&hex[start..start + 2], 16)
        .with_context(|| format!("parse {name} color component"))
}
