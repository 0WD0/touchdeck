use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::action::{parse_niri_action, ActionStep, NiriAction};
use crate::gesture::SwipeDirection;
use crate::key::{
    normalize_name, parse_key_sequence, parse_single_key, XKB_MOD_ALT, XKB_MOD_CONTROL,
    XKB_MOD_SHIFT, XKB_MOD_SUPER,
};
use crate::keymap::{Binding, Behavior, Keymap, MacroRegistry, Trigger};
use crate::layout::{SlotRegistry, SlotTarget};
use crate::mode::{parse_layer, parse_mode, Layer, Mode};

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

impl Binding {
    pub(crate) fn from_file_config(
        value: BindingFileConfig,
        slots: &SlotRegistry,
        macros: &MacroRegistry,
        behavior_registry: &BehaviorRegistry,
    ) -> Result<Self> {
        let mode = parse_mode(value.mode.as_deref().unwrap_or("base"))?;
        let layer = parse_layer(value.layer.as_deref().unwrap_or("base"))?;
        let trigger = parse_trigger(value.trigger, slots)?;
        let behavior = parse_behavior(value.behavior, macros, behavior_registry)?;

        Ok(Self {
            mode,
            layer,
            trigger,
            behavior,
            priority: value.priority.unwrap_or(0),
            consume: value.consume.unwrap_or(true),
        })
    }
}

#[derive(Clone, Default)]
pub(crate) struct BehaviorRegistry {
    definitions: HashMap<String, BehaviorDefinitionFileConfig>,
}

impl BehaviorRegistry {
    pub(crate) fn extend(&mut self, definitions: HashMap<String, BehaviorDefinitionFileConfig>) {
        self.definitions.extend(
            definitions
                .into_iter()
                .map(|(name, definition)| (normalize_name(&name), definition)),
        );
    }

    fn get(&self, name: &str) -> Option<&BehaviorDefinitionFileConfig> {
        self.definitions.get(&normalize_name(name))
    }
}

pub(crate) fn expand_keyboard_maps(
    maps: Vec<KeyboardMapFileConfig>,
    slots: &SlotRegistry,
    macros: &MacroRegistry,
    behavior_registry: &BehaviorRegistry,
) -> Result<Vec<Binding>> {
    let mut bindings = Vec::new();

    for (map_index, map) in maps.into_iter().enumerate() {
        let mode = parse_mode(map.mode.as_deref().unwrap_or("text"))?;
        let layer = parse_layer(map.layer.as_deref().unwrap_or("base"))?;
        let fingers = map.fingers.unwrap_or(1);
        let max_ms = map.max_ms;
        let hold_ms = map.hold_ms;
        let repeat_start_ms = map.repeat_start_ms;
        let repeat_interval_ms = map.repeat_interval_ms;
        let min_px = map.min_px;
        let priority = map.priority.unwrap_or(0);
        let consume = map.consume.unwrap_or(true);

        expand_keyboard_gesture_map(
            &mut bindings,
            slots,
            macros,
            behavior_registry,
            map_index,
            mode,
            layer,
            "tap",
            map.tap,
            |target| Trigger::Tap {
                target,
                fingers,
                max_ms,
            },
            priority,
            consume,
            repeat_start_ms,
            repeat_interval_ms,
        )?;
        expand_keyboard_hold_map(
            &mut bindings,
            slots,
            macros,
            behavior_registry,
            map_index,
            mode,
            layer,
            map.hold,
            fingers,
            hold_ms,
            priority,
            consume,
            repeat_start_ms,
            repeat_interval_ms,
        )?;
        expand_keyboard_repeat_map(
            &mut bindings,
            slots,
            macros,
            behavior_registry,
            map_index,
            mode,
            layer,
            map.repeat,
            fingers,
            repeat_start_ms,
            repeat_interval_ms,
            priority,
            consume,
        )?;
        expand_keyboard_gesture_map(
            &mut bindings,
            slots,
            macros,
            behavior_registry,
            map_index,
            mode,
            layer,
            "swipe_up",
            map.swipe_up,
            |target| Trigger::Swipe {
                target,
                fingers,
                direction: SwipeDirection::Up,
                min_px,
                max_ms,
            },
            priority,
            consume,
            repeat_start_ms,
            repeat_interval_ms,
        )?;
        expand_keyboard_gesture_map(
            &mut bindings,
            slots,
            macros,
            behavior_registry,
            map_index,
            mode,
            layer,
            "swipe_down",
            map.swipe_down,
            |target| Trigger::Swipe {
                target,
                fingers,
                direction: SwipeDirection::Down,
                min_px,
                max_ms,
            },
            priority,
            consume,
            repeat_start_ms,
            repeat_interval_ms,
        )?;
        expand_keyboard_gesture_map(
            &mut bindings,
            slots,
            macros,
            behavior_registry,
            map_index,
            mode,
            layer,
            "swipe_left",
            map.swipe_left,
            |target| Trigger::Swipe {
                target,
                fingers,
                direction: SwipeDirection::Left,
                min_px,
                max_ms,
            },
            priority,
            consume,
            repeat_start_ms,
            repeat_interval_ms,
        )?;
        expand_keyboard_gesture_map(
            &mut bindings,
            slots,
            macros,
            behavior_registry,
            map_index,
            mode,
            layer,
            "swipe_right",
            map.swipe_right,
            |target| Trigger::Swipe {
                target,
                fingers,
                direction: SwipeDirection::Right,
                min_px,
                max_ms,
            },
            priority,
            consume,
            repeat_start_ms,
            repeat_interval_ms,
        )?;
    }

    Ok(bindings)
}

fn expand_keyboard_hold_map(
    bindings: &mut Vec<Binding>,
    slots: &SlotRegistry,
    macros: &MacroRegistry,
    behavior_registry: &BehaviorRegistry,
    map_index: usize,
    mode: Mode,
    layer: Layer,
    behavior_invocations: Option<HashMap<String, String>>,
    fingers: usize,
    hold_ms: Option<u32>,
    priority: i32,
    consume: bool,
    repeat_start_ms: Option<u32>,
    repeat_interval_ms: Option<u32>,
) -> Result<()> {
    let Some(behavior_invocations) = behavior_invocations else {
        return Ok(());
    };

    for (slot_id, invocation) in behavior_invocations {
        let target = slots
            .get(&slot_id)
            .with_context(|| format!("keyboard map {map_index} hold target {slot_id}"))?;
        let mut behavior = parse_behavior_invocation(&invocation, macros, behavior_registry)
            .with_context(|| {
                format!("parse keyboard map {map_index} hold behavior for {slot_id} ({invocation})")
            })?;
        apply_hold_repeat_defaults(&mut behavior, repeat_start_ms, repeat_interval_ms);
        bindings.push(Binding {
            mode,
            layer,
            trigger: Trigger::Hold {
                target,
                fingers,
                min_ms: hold_ms,
            },
            behavior,
            priority,
            consume,
        });
    }

    Ok(())
}

fn apply_hold_repeat_defaults(
    behavior: &mut Behavior,
    start_ms: Option<u32>,
    interval_ms: Option<u32>,
) {
    if let Behavior::HoldRepeat {
        start_ms: behavior_start_ms,
        interval_ms: behavior_interval_ms,
        ..
    } = behavior
    {
        if behavior_start_ms.is_none() {
            *behavior_start_ms = start_ms;
        }
        if behavior_interval_ms.is_none() {
            *behavior_interval_ms = interval_ms;
        }
    }
}

fn expand_keyboard_repeat_map(
    bindings: &mut Vec<Binding>,
    slots: &SlotRegistry,
    macros: &MacroRegistry,
    behavior_registry: &BehaviorRegistry,
    map_index: usize,
    mode: Mode,
    layer: Layer,
    behavior_invocations: Option<HashMap<String, String>>,
    fingers: usize,
    repeat_start_ms: Option<u32>,
    repeat_interval_ms: Option<u32>,
    priority: i32,
    consume: bool,
) -> Result<()> {
    let Some(behavior_invocations) = behavior_invocations else {
        return Ok(());
    };

    for (slot_id, invocation) in behavior_invocations {
        let target = slots
            .get(&slot_id)
            .with_context(|| format!("keyboard map {map_index} repeat target {slot_id}"))?;
        let mut behavior = parse_behavior_invocation(&invocation, macros, behavior_registry)
            .with_context(|| {
                format!(
                    "parse keyboard map {map_index} repeat behavior for {slot_id} ({invocation})"
                )
            })?;
        apply_hold_repeat_defaults(&mut behavior, repeat_start_ms, repeat_interval_ms);
        bindings.push(Binding {
            mode,
            layer,
            trigger: Trigger::Hold {
                target,
                fingers,
                min_ms: repeat_start_ms,
            },
            behavior,
            priority,
            consume,
        });
    }

    Ok(())
}

fn expand_keyboard_gesture_map<F>(
    bindings: &mut Vec<Binding>,
    slots: &SlotRegistry,
    macros: &MacroRegistry,
    behavior_registry: &BehaviorRegistry,
    map_index: usize,
    mode: Mode,
    layer: Layer,
    gesture_name: &str,
    behavior_invocations: Option<HashMap<String, String>>,
    make_trigger: F,
    priority: i32,
    consume: bool,
    repeat_start_ms: Option<u32>,
    repeat_interval_ms: Option<u32>,
) -> Result<()>
where
    F: Fn(SlotTarget) -> Trigger,
{
    let Some(behavior_invocations) = behavior_invocations else {
        return Ok(());
    };

    for (slot_id, invocation) in behavior_invocations {
        let target = slots
            .get(&slot_id)
            .with_context(|| format!("keyboard map {map_index} {gesture_name} target {slot_id}"))?;
        let mut behavior =
            parse_behavior_invocation(&invocation, macros, behavior_registry).with_context(|| {
                format!(
                    "parse keyboard map {map_index} {gesture_name} behavior for {slot_id} ({invocation})"
                )
            })?;
        apply_hold_repeat_defaults(&mut behavior, repeat_start_ms, repeat_interval_ms);
        bindings.push(Binding {
            mode,
            layer,
            trigger: make_trigger(target),
            behavior,
            priority,
            consume,
        });
    }

    Ok(())
}

fn parse_trigger(value: TriggerFileConfig, slots: &SlotRegistry) -> Result<Trigger> {
    let target = slots.get(&value.target)?;
    let fingers = value.fingers.unwrap_or(1);

    match normalize_name(&value.kind).as_str() {
        "tap" => Ok(Trigger::Tap {
            target,
            fingers,
            max_ms: value.max_ms,
        }),
        "double_tap" | "doubletap" => Ok(Trigger::DoubleTap {
            target,
            fingers,
            max_ms: value.max_ms,
        }),
        "hold" | "long_press" | "longpress" => Ok(Trigger::Hold {
            target,
            fingers,
            min_ms: value.min_ms,
        }),
        "swipe" => Ok(Trigger::Swipe {
            target,
            fingers,
            direction: parse_swipe_direction(
                value
                    .direction
                    .as_deref()
                    .ok_or_else(|| anyhow!("swipe trigger is missing direction"))?,
            )?,
            min_px: value.min_px,
            max_ms: value.max_ms,
        }),
        other => Err(anyhow!("unknown trigger type {other}")),
    }
}

fn parse_swipe_direction(value: &str) -> Result<SwipeDirection> {
    match normalize_name(value).as_str() {
        "left" => Ok(SwipeDirection::Left),
        "right" => Ok(SwipeDirection::Right),
        "up" => Ok(SwipeDirection::Up),
        "down" => Ok(SwipeDirection::Down),
        _ => Err(anyhow!("unknown swipe direction {value}")),
    }
}

fn parse_behavior(
    value: BehaviorFileConfig,
    macros: &MacroRegistry,
    behavior_registry: &BehaviorRegistry,
) -> Result<Behavior> {
    match normalize_name(&value.kind).as_str() {
        "key" | "key_sequence" | "keys" => {
            let keys = value
                .key
                .or(value.keys)
                .ok_or_else(|| anyhow!("key behavior is missing key/keys"))?;
            let sequence = parse_key_sequence(&keys)?;
            let translation = value
                .translation
                .as_deref()
                .map(parse_key_translation_policy)
                .transpose()?;
            let route = value.route.as_deref().map(parse_key_route).transpose()?;
            if translation.is_some() || route.is_some() {
                Ok(Behavior::KeySequenceWithOptions {
                    sequence,
                    translation,
                    route,
                })
            } else {
                Ok(Behavior::KeySequence(sequence))
            }
        }
        "key_hold" | "hold_key" | "modifier" => {
            let key = value
                .key
                .or(value.keys)
                .ok_or_else(|| anyhow!("key_hold behavior is missing key/keys"))?;
            Ok(Behavior::KeyHold(parse_single_key(&key)?))
        }
        "mod_morph" => parse_mod_morph_behavior(
            "inline",
            value.mods.as_deref(),
            value.keep_mods.as_deref(),
            value.bindings.as_deref(),
            value.normal.as_deref(),
            value.morph.as_deref(),
            &[],
            macros,
            behavior_registry,
        ),
        "key_repeat" => {
            if value.key.is_some() || value.keys.is_some() {
                return Err(anyhow!(
                    "key_repeat repeats the previous key and takes no key/keys; use hold_repeat for fixed repeats"
                ));
            }
            Ok(Behavior::KeyRepeat)
        }
        "hold_repeat" | "repeat_key" | "repeat" => {
            let keys = value
                .key
                .or(value.keys)
                .ok_or_else(|| anyhow!("hold_repeat behavior is missing key/keys"))?;
            Ok(Behavior::HoldRepeat {
                sequence: parse_key_sequence(&keys)?,
                start_ms: value.start_ms,
                interval_ms: value.interval_ms,
                translation: value
                    .translation
                    .as_deref()
                    .map(parse_key_translation_policy)
                    .transpose()?,
                route: value.route.as_deref().map(parse_key_route).transpose()?,
            })
        }
        "sequence" => Ok(Behavior::Sequence(parse_action_steps(
            value
                .steps
                .ok_or_else(|| anyhow!("sequence behavior is missing steps"))?,
        )?)),
        "macro" => {
            let name = value
                .macro_alias
                .or(value.macro_name)
                .ok_or_else(|| anyhow!("macro behavior is missing macro name"))?;
            Ok(Behavior::Sequence(macros.get(&name)?))
        }
        "niri" => Ok(Behavior::Niri(parse_niri_action(
            value
                .action
                .as_deref()
                .ok_or_else(|| anyhow!("niri behavior is missing action"))?,
        )?)),
        "mode" | "mode_set" => Ok(Behavior::ModeSet(parse_mode(
            value
                .mode
                .as_deref()
                .ok_or_else(|| anyhow!("mode behavior is missing mode"))?,
        )?)),
        "mode_toggle" => Ok(Behavior::ModeToggle(parse_mode(
            value
                .mode
                .as_deref()
                .ok_or_else(|| anyhow!("mode_toggle behavior is missing mode"))?,
        )?)),
        "mode_momentary" => Ok(Behavior::ModeMomentary(parse_mode(
            value
                .mode
                .as_deref()
                .ok_or_else(|| anyhow!("mode_momentary behavior is missing mode"))?,
        )?)),
        "layer" | "layer_set" => Ok(Behavior::LayerSet(parse_layer(
            value
                .layer
                .as_deref()
                .ok_or_else(|| anyhow!("layer behavior is missing layer"))?,
        )?)),
        "layer_toggle" => Ok(Behavior::LayerToggle(parse_layer(
            value
                .layer
                .as_deref()
                .ok_or_else(|| anyhow!("layer_toggle behavior is missing layer"))?,
        )?)),
        "layer_momentary" => Ok(Behavior::LayerMomentary(parse_layer(
            value
                .layer
                .as_deref()
                .ok_or_else(|| anyhow!("layer_momentary behavior is missing layer"))?,
        )?)),
        "transparent" => Ok(Behavior::Transparent),
        "noop" | "no_op" => Ok(Behavior::NoOp),
        "exit" => Ok(Behavior::Exit),
        other => Err(anyhow!("unknown behavior type {other}")),
    }
}

fn parse_behavior_invocation(
    value: &str,
    macros: &MacroRegistry,
    behavior_registry: &BehaviorRegistry,
) -> Result<Behavior> {
    let value = value.trim();
    let Some(rest) = value.strip_prefix('&') else {
        return Err(anyhow!(
            "behavior binding {value:?} must use ZMK-style '&behavior args' syntax"
        ));
    };

    let mut parts = rest.split_whitespace();
    let name = parts
        .next()
        .ok_or_else(|| anyhow!("empty behavior binding {value:?}"))?;
    let args = parts.collect::<Vec<_>>();

    if let Some(definition) = behavior_registry.get(name) {
        return parse_defined_behavior_invocation(
            name,
            definition,
            &args,
            macros,
            behavior_registry,
        );
    }

    parse_builtin_behavior_invocation(name, &args, macros)
}

fn parse_defined_behavior_invocation(
    name: &str,
    definition: &BehaviorDefinitionFileConfig,
    args: &[&str],
    macros: &MacroRegistry,
    behavior_registry: &BehaviorRegistry,
) -> Result<Behavior> {
    if let Some(binding) = &definition.binding {
        let expanded = expand_behavior_template(binding, args);
        return parse_behavior_invocation(&expanded, macros, behavior_registry)
            .with_context(|| format!("expand behavior {name} as {expanded:?}"));
    }

    let kind = definition.kind.as_deref().unwrap_or(name);
    if normalize_name(kind) == "mod_morph" {
        return parse_mod_morph_behavior(
            name,
            definition.mods.as_deref(),
            definition.keep_mods.as_deref(),
            definition.bindings.as_deref(),
            definition.normal.as_deref(),
            definition.morph.as_deref(),
            args,
            macros,
            behavior_registry,
        );
    }
    parse_behavior_invocation_kind(
        kind,
        args,
        BehaviorFields {
            key: definition.key.as_deref(),
            keys: definition.keys.as_deref(),
            action: definition.action.as_deref(),
            macro_name: definition
                .macro_alias
                .as_deref()
                .or(definition.macro_name.as_deref()),
            steps: definition.steps.clone(),
            mode: definition.mode.as_deref(),
            layer: definition.layer.as_deref(),
            start_ms: definition.start_ms,
            interval_ms: definition.interval_ms,
            translation: definition.translation.as_deref(),
            route: definition.route.as_deref(),
        },
        macros,
    )
    .with_context(|| format!("resolve behavior {name}"))
}

fn parse_builtin_behavior_invocation(
    name: &str,
    args: &[&str],
    macros: &MacroRegistry,
) -> Result<Behavior> {
    parse_behavior_invocation_kind(name, args, BehaviorFields::default(), macros)
}

fn parse_mod_morph_behavior(
    name: &str,
    mods: Option<&[String]>,
    keep_mods: Option<&[String]>,
    bindings: Option<&[String]>,
    normal: Option<&str>,
    morph: Option<&str>,
    args: &[&str],
    macros: &MacroRegistry,
    behavior_registry: &BehaviorRegistry,
) -> Result<Behavior> {
    let mods = parse_modifier_flags(
        mods.ok_or_else(|| anyhow!("mod_morph behavior {name} is missing mods"))?,
    )?;
    let keep_mods = keep_mods
        .map(parse_modifier_flags)
        .transpose()?
        .unwrap_or(0);
    let (normal, morph) = parse_mod_morph_bindings(name, bindings, normal, morph)?;

    Ok(Behavior::ModMorph {
        mods,
        keep_mods,
        normal: Box::new(parse_behavior_invocation(
            &expand_behavior_template(normal, args),
            macros,
            behavior_registry,
        )?),
        morph: Box::new(parse_behavior_invocation(
            &expand_behavior_template(morph, args),
            macros,
            behavior_registry,
        )?),
    })
}

fn parse_mod_morph_bindings<'a>(
    name: &str,
    bindings: Option<&'a [String]>,
    normal: Option<&'a str>,
    morph: Option<&'a str>,
) -> Result<(&'a str, &'a str)> {
    if let Some(bindings) = bindings {
        if normal.is_some() || morph.is_some() {
            return Err(anyhow!(
                "mod_morph behavior {name} must use either bindings or normal/morph, not both"
            ));
        }
        let [normal, morph] = bindings else {
            return Err(anyhow!(
                "mod_morph behavior {name} bindings must contain exactly two behavior bindings"
            ));
        };
        return Ok((normal.as_str(), morph.as_str()));
    }

    Ok((
        normal.ok_or_else(|| anyhow!("mod_morph behavior {name} is missing normal binding"))?,
        morph.ok_or_else(|| anyhow!("mod_morph behavior {name} is missing morph binding"))?,
    ))
}

#[derive(Default)]
struct BehaviorFields<'a> {
    key: Option<&'a str>,
    keys: Option<&'a str>,
    action: Option<&'a str>,
    macro_name: Option<&'a str>,
    steps: Option<Vec<ActionStepFileConfig>>,
    mode: Option<&'a str>,
    layer: Option<&'a str>,
    start_ms: Option<u32>,
    interval_ms: Option<u32>,
    translation: Option<&'a str>,
    route: Option<&'a str>,
}

fn parse_behavior_invocation_kind(
    kind: &str,
    args: &[&str],
    fields: BehaviorFields<'_>,
    macros: &MacroRegistry,
) -> Result<Behavior> {
    let args_joined = args.join(" ");
    let key_arg = fields
        .keys
        .or(fields.key)
        .map(str::to_string)
        .or_else(|| (!args_joined.is_empty()).then_some(args_joined.clone()));
    let translation = fields
        .translation
        .map(parse_key_translation_policy)
        .transpose()?;
    let route = fields.route.map(parse_key_route).transpose()?;

    match normalize_name(kind).as_str() {
        "kp" | "key" | "key_sequence" | "keys" => {
            let keys = key_arg.ok_or_else(|| anyhow!("&{kind} is missing key/keys"))?;
            let sequence = parse_key_sequence(&keys)?;
            if translation.is_some() || route.is_some() {
                Ok(Behavior::KeySequenceWithOptions {
                    sequence,
                    translation,
                    route,
                })
            } else {
                Ok(Behavior::KeySequence(sequence))
            }
        }
        "ime_key" | "ik" => {
            let keys = key_arg.ok_or_else(|| anyhow!("&{kind} is missing key/keys"))?;
            Ok(Behavior::KeySequenceWithOptions {
                sequence: parse_key_sequence(&keys)?,
                translation,
                route: Some(KeyRoute::ImeKey),
            })
        }
        "ime_text" | "it" => {
            let keys = key_arg.ok_or_else(|| anyhow!("&{kind} is missing key/keys"))?;
            Ok(Behavior::KeySequenceWithOptions {
                sequence: parse_key_sequence(&keys)?,
                translation,
                route: Some(KeyRoute::ImeText),
            })
        }
        "app_key" | "ak" => {
            let keys = key_arg.ok_or_else(|| anyhow!("&{kind} is missing key/keys"))?;
            Ok(Behavior::KeySequenceWithOptions {
                sequence: parse_key_sequence(&keys)?,
                translation,
                route: Some(KeyRoute::AppKey),
            })
        }
        "ime_only" | "io" => {
            let keys = key_arg.ok_or_else(|| anyhow!("&{kind} is missing key/keys"))?;
            Ok(Behavior::KeySequenceWithOptions {
                sequence: parse_key_sequence(&keys)?,
                translation,
                route: Some(KeyRoute::ImeOnly),
            })
        }
        "kpe" | "key_effective" | "effective_key" => {
            let keys = key_arg.ok_or_else(|| anyhow!("&{kind} is missing key/keys"))?;
            Ok(Behavior::KeySequenceWithOptions {
                sequence: parse_key_sequence(&keys)?,
                translation: Some(KeyTranslationPolicy::Effective),
                route,
            })
        }
        "kpr" | "key_raw" | "raw_key" => {
            let keys = key_arg.ok_or_else(|| anyhow!("&{kind} is missing key/keys"))?;
            Ok(Behavior::KeySequenceWithOptions {
                sequence: parse_key_sequence(&keys)?,
                translation: Some(KeyTranslationPolicy::Raw),
                route,
            })
        }
        "hold" | "kh" | "key_hold" | "hold_key" | "modifier" => {
            let key = key_arg.ok_or_else(|| anyhow!("&{kind} is missing key"))?;
            Ok(Behavior::KeyHold(parse_single_key(&key)?))
        }
        "key_repeat" => {
            if key_arg.is_some() {
                return Err(anyhow!(
                    "&key_repeat repeats the previous key and takes no arguments; use &hold_repeat KEY for fixed repeats"
                ));
            }
            Ok(Behavior::KeyRepeat)
        }
        "hold_repeat" | "repeat_key" => {
            let keys = key_arg.ok_or_else(|| anyhow!("&{kind} is missing key/keys"))?;
            Ok(Behavior::HoldRepeat {
                sequence: parse_key_sequence(&keys)?,
                start_ms: fields.start_ms,
                interval_ms: fields.interval_ms,
                translation,
                route,
            })
        }
        "macro" => {
            let name = fields
                .macro_name
                .map(str::to_string)
                .or_else(|| args.first().map(|value| (*value).to_string()))
                .ok_or_else(|| anyhow!("&macro is missing macro name"))?;
            Ok(Behavior::Sequence(macros.get(&name)?))
        }
        "sequence" => {
            let steps = fields
                .steps
                .ok_or_else(|| anyhow!("sequence behavior requires steps"))?;
            Ok(Behavior::Sequence(parse_action_steps(steps)?))
        }
        "niri" => {
            let action = fields
                .action
                .map(str::to_string)
                .or_else(|| args.first().map(|value| (*value).to_string()))
                .ok_or_else(|| anyhow!("&niri is missing action"))?;
            Ok(Behavior::Niri(parse_niri_action(&action)?))
        }
        "mode" | "mode_set" => {
            let mode = fields
                .mode
                .map(str::to_string)
                .or_else(|| args.first().map(|value| (*value).to_string()))
                .ok_or_else(|| anyhow!("&mode is missing mode"))?;
            Ok(Behavior::ModeSet(parse_mode(&mode)?))
        }
        "mode_toggle" | "tog_mode" => {
            let mode = fields
                .mode
                .map(str::to_string)
                .or_else(|| args.first().map(|value| (*value).to_string()))
                .ok_or_else(|| anyhow!("&mode_toggle is missing mode"))?;
            Ok(Behavior::ModeToggle(parse_mode(&mode)?))
        }
        "mode_momentary" | "mo_mode" => {
            let mode = fields
                .mode
                .map(str::to_string)
                .or_else(|| args.first().map(|value| (*value).to_string()))
                .ok_or_else(|| anyhow!("&mode_momentary is missing mode"))?;
            Ok(Behavior::ModeMomentary(parse_mode(&mode)?))
        }
        "layer" | "layer_set" | "to" => {
            let layer = fields
                .layer
                .map(str::to_string)
                .or_else(|| args.first().map(|value| (*value).to_string()))
                .ok_or_else(|| anyhow!("&layer is missing layer"))?;
            Ok(Behavior::LayerSet(parse_layer(&layer)?))
        }
        "layer_toggle" | "tog" => {
            let layer = fields
                .layer
                .map(str::to_string)
                .or_else(|| args.first().map(|value| (*value).to_string()))
                .ok_or_else(|| anyhow!("&layer_toggle is missing layer"))?;
            Ok(Behavior::LayerToggle(parse_layer(&layer)?))
        }
        "layer_momentary" | "mo" => {
            let layer = fields
                .layer
                .map(str::to_string)
                .or_else(|| args.first().map(|value| (*value).to_string()))
                .ok_or_else(|| anyhow!("&layer_momentary is missing layer"))?;
            Ok(Behavior::LayerMomentary(parse_layer(&layer)?))
        }
        "trans" | "transparent" => Ok(Behavior::Transparent),
        "none" | "noop" | "no_op" => Ok(Behavior::NoOp),
        "exit" => Ok(Behavior::Exit),
        other => Err(anyhow!("unknown behavior &{other}")),
    }
}

fn expand_behavior_template(template: &str, args: &[&str]) -> String {
    let mut expanded = template.to_string();
    for (index, arg) in args.iter().enumerate() {
        expanded = expanded.replace(&format!("${}", index + 1), arg);
    }
    expanded
}

fn parse_key_translation_policy(value: &str) -> Result<KeyTranslationPolicy> {
    match normalize_name(value).as_str() {
        "effective" | "effective_keysym" | "translated" => Ok(KeyTranslationPolicy::Effective),
        "raw" | "raw_keysym" | "base" => Ok(KeyTranslationPolicy::Raw),
        other => Err(anyhow!("unknown key translation policy {other:?}")),
    }
}

fn parse_key_route(value: &str) -> Result<KeyRoute> {
    match normalize_name(value).as_str() {
        "ime" | "ime_key" | "ime_first" | "rime" | "rime_first" => Ok(KeyRoute::ImeKey),
        "ime_text" | "text" | "commit_text" => Ok(KeyRoute::ImeText),
        "app" | "app_key" | "direct" | "passthrough" | "forward" => Ok(KeyRoute::AppKey),
        "ime_only" | "rime_only" | "consume" | "filter" => Ok(KeyRoute::ImeOnly),
        other => Err(anyhow!("unknown key route {other:?}")),
    }
}

fn parse_modifier_flags(values: &[String]) -> Result<u32> {
    let mut flags = 0;
    for value in values {
        flags |= match value.trim() {
            "MOD_LSFT" | "MOD_RSFT" | "LSFT" | "RSFT" | "LSHIFT" | "LEFT_SHIFT" | "RSHIFT"
            | "RIGHT_SHIFT" => XKB_MOD_SHIFT,
            "MOD_LCTL" | "MOD_RCTL" | "LCTL" | "RCTL" | "LCTRL" | "LEFT_CONTROL" | "RCTRL"
            | "RIGHT_CONTROL" => XKB_MOD_CONTROL,
            "MOD_LALT" | "MOD_RALT" | "LALT" | "LEFT_ALT" | "RALT" | "RIGHT_ALT" => XKB_MOD_ALT,
            "MOD_LGUI" | "MOD_RGUI" | "LGUI" | "LEFT_GUI" | "LEFT_WIN" | "LEFT_META" | "RGUI"
            | "RIGHT_GUI" | "RIGHT_WIN" | "RIGHT_META" => XKB_MOD_SUPER,
            other => return Err(anyhow!("unknown modifier {other:?}")),
        };
    }
    Ok(flags)
}

pub(crate) fn parse_action_steps(steps: Vec<ActionStepFileConfig>) -> Result<Vec<ActionStep>> {
    steps.into_iter().map(parse_action_step).collect()
}

fn parse_action_step(value: ActionStepFileConfig) -> Result<ActionStep> {
    match normalize_name(&value.kind).as_str() {
        "key_down" => Ok(ActionStep::KeyDown(parse_single_key(
            value
                .key
                .as_deref()
                .ok_or_else(|| anyhow!("key_down step is missing key"))?,
        )?)),
        "key_up" => Ok(ActionStep::KeyUp(parse_single_key(
            value
                .key
                .as_deref()
                .ok_or_else(|| anyhow!("key_up step is missing key"))?,
        )?)),
        "tap_key" => Ok(ActionStep::TapKey(parse_single_key(
            value
                .key
                .as_deref()
                .ok_or_else(|| anyhow!("tap_key step is missing key"))?,
        )?)),
        "key_sequence" | "keys" => Ok(ActionStep::KeySequence(parse_key_sequence(
            value
                .keys
                .or(value.key)
                .as_deref()
                .ok_or_else(|| anyhow!("key_sequence step is missing key/keys"))?,
        )?)),
        "niri" => Ok(ActionStep::Niri(parse_niri_action(
            value
                .action
                .as_deref()
                .ok_or_else(|| anyhow!("niri step is missing action"))?,
        )?)),
        "delay" | "delay_ms" => Ok(ActionStep::DelayMs(
            value
                .ms
                .ok_or_else(|| anyhow!("delay step is missing ms"))?,
        )),
        other => Err(anyhow!("unknown action step type {other}")),
    }
}
