use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::action::{parse_niri_action, NiriAction};
use crate::layout::SlotRegistry;
use crate::{
    expand_keyboard_maps, parse_action_steps, Binding, BehaviorRegistry, Keymap, MacroRegistry,
};

#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) action_swipe_left: Option<NiriAction>,
    pub(crate) action_swipe_right: Option<NiriAction>,
    pub(crate) action_swipe_up: Option<NiriAction>,
    pub(crate) action_swipe_down: Option<NiriAction>,
    pub(crate) action_two_finger_tap: Option<NiriAction>,
    pub(crate) tap_radius: f64,
    pub(crate) two_finger_tap_ms: u32,
    pub(crate) exit_tap_ms: u32,
    pub(crate) hold_ms: u32,
    pub(crate) repeat_start_ms: u32,
    pub(crate) repeat_interval_ms: u32,
    pub(crate) double_tap_ms: u32,
    pub(crate) swipe_threshold_ratio: f64,
    pub(crate) swipe_threshold_min: f64,
    pub(crate) swipe_threshold_max: f64,
    pub(crate) debug_alpha: u8,
    pub(crate) debug_draw: bool,
    pub(crate) mode_hint_ms: u32,
    pub(crate) modifier_tap_ms: u32,
    pub(crate) log_touch: bool,
    pub(crate) record_trace_path: Option<PathBuf>,
    pub(crate) xkb_keymap_path: Option<PathBuf>,
    pub(crate) text_output: TextOutputConfig,
    pub(crate) slots: SlotRegistry,
    pub(crate) keymap: Keymap,
    pub(crate) macros: MacroRegistry,
    pub(crate) exit_corner_enabled: bool,
    pub(crate) exit_corner_ratio: f64,
    pub(crate) exit_corner_tap_ms: u32,
}

impl Default for Config {
    fn default() -> Self {
        let mut config = Self {
            action_swipe_left: env_niri_action(
                "TOUCHDECK_ACTION_SWIPE_LEFT",
                "focus-workspace-down",
            ),
            action_swipe_right: env_niri_action(
                "TOUCHDECK_ACTION_SWIPE_RIGHT",
                "focus-workspace-up",
            ),
            action_swipe_up: env_niri_action("TOUCHDECK_ACTION_SWIPE_UP", "focus-column-right"),
            action_swipe_down: env_niri_action("TOUCHDECK_ACTION_SWIPE_DOWN", "focus-column-left"),
            action_two_finger_tap: env_niri_action(
                "TOUCHDECK_ACTION_TWO_FINGER_TAP",
                "toggle-overview",
            ),
            tap_radius: env_f64("TOUCHDECK_TAP_RADIUS", 48.0),
            two_finger_tap_ms: env_u32("TOUCHDECK_TWO_FINGER_TAP_MS", 350),
            exit_tap_ms: env_u32("TOUCHDECK_EXIT_TAP_MS", 450),
            hold_ms: env_u32("TOUCHDECK_HOLD_MS", 180),
            repeat_start_ms: env_u32("TOUCHDECK_REPEAT_START_MS", 520),
            repeat_interval_ms: env_u32("TOUCHDECK_REPEAT_INTERVAL_MS", 45),
            double_tap_ms: env_u32("TOUCHDECK_DOUBLE_TAP_MS", 280),
            swipe_threshold_ratio: env_f64("TOUCHDECK_SWIPE_THRESHOLD_RATIO", 0.08),
            swipe_threshold_min: env_f64("TOUCHDECK_SWIPE_THRESHOLD_MIN", 64.0),
            swipe_threshold_max: env_f64("TOUCHDECK_SWIPE_THRESHOLD_MAX", 140.0),
            debug_alpha: env_u8("TOUCHDECK_DEBUG_ALPHA", 0),
            debug_draw: env_bool("TOUCHDECK_DEBUG_DRAW", false),
            mode_hint_ms: env_u32("TOUCHDECK_MODE_HINT_MS", 400),
            modifier_tap_ms: env_u32("TOUCHDECK_MODIFIER_TAP_MS", 40),
            log_touch: env_bool("TOUCHDECK_LOG_TOUCH", false),
            record_trace_path: env::var_os("TOUCHDECK_RECORD_TRACE").map(PathBuf::from),
            xkb_keymap_path: env::var_os("TOUCHDECK_XKB_KEYMAP").map(PathBuf::from),
            text_output: TextOutputConfig::from_env(),
            slots: SlotRegistry::default(),
            keymap: Keymap::default(),
            macros: MacroRegistry::default(),
            exit_corner_enabled: env_bool("TOUCHDECK_EXIT_CORNER_ENABLED", true),
            exit_corner_ratio: env_f64("TOUCHDECK_EXIT_CORNER_RATIO", 0.12),
            exit_corner_tap_ms: env_u32("TOUCHDECK_EXIT_CORNER_TAP_MS", 350),
        };

        if let Err(err) = config.load_file_overrides() {
            eprintln!("touchdeck: failed to load config file: {err:?}");
        }
        config.apply_env_overrides();

        config
    }
}

impl Config {
    fn apply_env_overrides(&mut self) {
        if let Some(backend) = env_text_output_backend() {
            self.text_output.backend = backend;
        }
        if let Some(socket) = env::var_os("TOUCHDECK_IME_SOCKET") {
            self.text_output.ime_socket = PathBuf::from(socket);
        }
    }

    fn load_file_overrides(&mut self) -> Result<()> {
        let path = if let Some(path) = env::var_os("TOUCHDECK_CONFIG") {
            PathBuf::from(path)
        } else {
            let default_path = PathBuf::from("touchdeck.toml");
            if !default_path.exists() {
                return Ok(());
            }
            default_path
        };

        let source = fs::read_to_string(&path)
            .with_context(|| format!("read config file {}", path.display()))?;
        let file_config: FileConfig = toml::from_str(&source)
            .with_context(|| format!("parse config file {}", path.display()))?;
        let keyboard = file_config.keyboard;

        if let Some(keyboard) = &keyboard {
            if let Some(output) = &keyboard.output {
                self.text_output.backend = parse_text_output_backend(output)?;
            }
            if let Some(socket) = &keyboard.ime_socket {
                self.text_output.ime_socket = resolve_config_relative(&path, socket);
            }
            if let Some(path) = &keyboard.xkb_keymap {
                self.xkb_keymap_path = Some(PathBuf::from(path));
            }
        }

        if let Some(ime) = &file_config.ime {
            if let Some(socket) = &ime.socket {
                self.text_output.ime_socket = resolve_config_relative(&path, socket);
            }
            if let Some(output) = &ime.output {
                self.text_output.backend = parse_text_output_backend(output)?;
            }
        }

        if let Some(layout) = &file_config.layout {
            if let Some(svg) = &layout.svg {
                let svg_path = resolve_config_relative(&path, svg);
                self.slots = SlotRegistry::from_svg_file(&svg_path)?;
            }
        }

        if let Some(macros) = file_config.macros {
            self.macros.clear();
            for (name, macro_config) in macros {
                self.macros
                    .insert(&name, parse_action_steps(macro_config.steps)?);
            }
        }

        let mut behavior_registry = BehaviorRegistry::default();
        if let Some(behaviors) = file_config.behaviors {
            behavior_registry.extend(behaviors);
        }
        if let Some(keyboard) = &keyboard {
            if let Some(behaviors) = &keyboard.behaviors {
                behavior_registry.extend(behaviors.clone());
            }
        }

        if let Some(bindings) = file_config.bindings {
            self.keymap.bindings.clear();
            for binding in bindings {
                self.keymap.bindings.push(Binding::from_file_config(
                    binding,
                    &self.slots,
                    &self.macros,
                    &behavior_registry,
                )?);
            }
        }

        if let Some(keyboard) = keyboard {
            if let Some(maps) = keyboard.layers {
                self.keymap.bindings.extend(expand_keyboard_maps(
                    maps,
                    &self.slots,
                    &self.macros,
                    &behavior_registry,
                )?);
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TextOutputConfig {
    pub(crate) backend: TextOutputBackend,
    pub(crate) ime_socket: PathBuf,
}

impl TextOutputConfig {
    fn from_env() -> Self {
        let backend = env_text_output_backend().unwrap_or(TextOutputBackend::VirtualKeyboard);

        let ime_socket = env::var_os("TOUCHDECK_IME_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(default_ime_socket_path);

        Self {
            backend,
            ime_socket,
        }
    }
}

fn env_text_output_backend() -> Option<TextOutputBackend> {
    env::var("TOUCHDECK_TEXT_OUTPUT")
        .or_else(|_| env::var("TOUCHDECK_KEYBOARD_OUTPUT"))
        .ok()
        .and_then(|value| match parse_text_output_backend(&value) {
            Ok(backend) => Some(backend),
            Err(err) => {
                eprintln!("touchdeck: invalid text output backend {value:?}: {err}");
                None
            }
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextOutputBackend {
    VirtualKeyboard,
    Ime,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyTranslationPolicy {
    Effective,
    Raw,
}

impl KeyTranslationPolicy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Effective => "effective",
            Self::Raw => "raw",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyRoute {
    ImeKey,
    ImeText,
    AppKey,
    ImeOnly,
}

impl KeyRoute {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ImeKey => "ime-key",
            Self::ImeText => "ime-text",
            Self::AppKey => "app-key",
            Self::ImeOnly => "ime-only",
        }
    }
}

impl TextOutputBackend {
    pub(crate) fn uses_virtual_keyboard(self) -> bool {
        matches!(self, Self::VirtualKeyboard | Self::Both)
    }

    pub(crate) fn uses_ime(self) -> bool {
        matches!(self, Self::Ime | Self::Both)
    }
}

pub(crate) fn default_ime_socket_path() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("touchdeck-ime.sock")
}

pub(crate) fn parse_text_output_backend(value: &str) -> Result<TextOutputBackend> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "virtual" | "virtual_keyboard" | "wayland_virtual_keyboard" => {
            Ok(TextOutputBackend::VirtualKeyboard)
        }
        "ime" | "touchdeck_ime" | "ipc" => Ok(TextOutputBackend::Ime),
        "both" | "dual" => Ok(TextOutputBackend::Both),
        other => Err(anyhow!(
            "unsupported text output backend {other}; supported: virtual-keyboard, ime, both"
        )),
    }
}


#[derive(Deserialize)]
pub(crate) struct FileConfig {
    pub(crate) layout: Option<LayoutFileConfig>,
    pub(crate) keyboard: Option<KeyboardFileConfig>,
    pub(crate) ime: Option<ImeFileConfig>,
    pub(crate) behaviors: Option<HashMap<String, BehaviorDefinitionFileConfig>>,
    pub(crate) macros: Option<HashMap<String, MacroFileConfig>>,
    pub(crate) bindings: Option<Vec<BindingFileConfig>>,
}

#[derive(Deserialize)]
pub(crate) struct LayoutFileConfig {
    pub(crate) svg: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct KeyboardFileConfig {
    pub(crate) output: Option<String>,
    pub(crate) ime_socket: Option<String>,
    pub(crate) xkb_keymap: Option<String>,
    pub(crate) behaviors: Option<HashMap<String, BehaviorDefinitionFileConfig>>,
    pub(crate) layers: Option<Vec<KeyboardMapFileConfig>>,
}

#[derive(Deserialize)]
pub(crate) struct ImeFileConfig {
    pub(crate) output: Option<String>,
    pub(crate) socket: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct KeyboardMapFileConfig {
    pub(crate) mode: Option<String>,
    pub(crate) layer: Option<String>,
    pub(crate) tap: Option<HashMap<String, String>>,
    pub(crate) hold: Option<HashMap<String, String>>,
    pub(crate) repeat: Option<HashMap<String, String>>,
    pub(crate) swipe_up: Option<HashMap<String, String>>,
    pub(crate) swipe_down: Option<HashMap<String, String>>,
    pub(crate) swipe_left: Option<HashMap<String, String>>,
    pub(crate) swipe_right: Option<HashMap<String, String>>,
    pub(crate) fingers: Option<usize>,
    pub(crate) max_ms: Option<u32>,
    pub(crate) hold_ms: Option<u32>,
    pub(crate) repeat_start_ms: Option<u32>,
    pub(crate) repeat_interval_ms: Option<u32>,
    pub(crate) min_px: Option<f64>,
    pub(crate) priority: Option<i32>,
    pub(crate) consume: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct MacroFileConfig {
    pub(crate) steps: Vec<ActionStepFileConfig>,
}

#[derive(Clone, Deserialize)]
pub(crate) struct ActionStepFileConfig {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) key: Option<String>,
    pub(crate) keys: Option<String>,
    pub(crate) action: Option<String>,
    pub(crate) ms: Option<u32>,
}

#[derive(Deserialize)]
pub(crate) struct BindingFileConfig {
    pub(crate) mode: Option<String>,
    pub(crate) layer: Option<String>,
    pub(crate) trigger: TriggerFileConfig,
    pub(crate) behavior: BehaviorFileConfig,
    pub(crate) priority: Option<i32>,
    pub(crate) consume: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct TriggerFileConfig {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) target: String,
    pub(crate) direction: Option<String>,
    pub(crate) fingers: Option<usize>,
    pub(crate) min_ms: Option<u32>,
    pub(crate) max_ms: Option<u32>,
    pub(crate) min_px: Option<f64>,
}

#[derive(Deserialize)]
pub(crate) struct BehaviorFileConfig {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) key: Option<String>,
    pub(crate) keys: Option<String>,
    pub(crate) action: Option<String>,
    pub(crate) macro_name: Option<String>,
    #[serde(rename = "macro")]
    pub(crate) macro_alias: Option<String>,
    pub(crate) steps: Option<Vec<ActionStepFileConfig>>,
    pub(crate) mode: Option<String>,
    pub(crate) layer: Option<String>,
    pub(crate) start_ms: Option<u32>,
    pub(crate) interval_ms: Option<u32>,
    pub(crate) translation: Option<String>,
    pub(crate) route: Option<String>,
    pub(crate) bindings: Option<Vec<String>>,
    pub(crate) mods: Option<Vec<String>>,
    #[serde(alias = "keep-mods")]
    pub(crate) keep_mods: Option<Vec<String>>,
    pub(crate) normal: Option<String>,
    pub(crate) morph: Option<String>,
}

#[derive(Clone, Deserialize)]
pub(crate) struct BehaviorDefinitionFileConfig {
    #[serde(rename = "type")]
    pub(crate) kind: Option<String>,
    pub(crate) binding: Option<String>,
    pub(crate) key: Option<String>,
    pub(crate) keys: Option<String>,
    pub(crate) action: Option<String>,
    pub(crate) macro_name: Option<String>,
    #[serde(rename = "macro")]
    pub(crate) macro_alias: Option<String>,
    pub(crate) steps: Option<Vec<ActionStepFileConfig>>,
    pub(crate) mode: Option<String>,
    pub(crate) layer: Option<String>,
    pub(crate) start_ms: Option<u32>,
    pub(crate) interval_ms: Option<u32>,
    pub(crate) translation: Option<String>,
    pub(crate) route: Option<String>,
    pub(crate) bindings: Option<Vec<String>>,
    pub(crate) mods: Option<Vec<String>>,
    #[serde(alias = "keep-mods")]
    pub(crate) keep_mods: Option<Vec<String>>,
    pub(crate) normal: Option<String>,
    pub(crate) morph: Option<String>,
}


pub(crate) fn resolve_config_relative(config_path: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path)
    }
}


fn env_f64(name: &str, default: f64) -> f64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(default)
}

pub(crate) fn env_u32(name: &str, default: u32) -> u32 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
}

fn env_u8(name: &str, default: u8) -> u8 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(default)
}

pub(crate) fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

fn env_niri_action(name: &str, default: &str) -> Option<NiriAction> {
    let value = env::var(name).unwrap_or_else(|_| default.to_string());
    if value.trim().is_empty() {
        return None;
    }

    match parse_niri_action(&value) {
        Ok(action) => Some(action),
        Err(err) => {
            eprintln!("touchdeck: invalid {name}: {err:?}");
            None
        }
    }
}

