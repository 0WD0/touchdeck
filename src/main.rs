use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use memmap2::MmapMut;
use serde::{Deserialize, Serialize};
use tempfile::tempfile;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_region, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface, wl_touch,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1, zwlr_layer_surface_v1,
};

const NAMESPACE: &str = "touchdeck-prototype";
const KEY_ESC: u32 = 1;
const KEY_1: u32 = 2;
const KEY_2: u32 = 3;
const KEY_3: u32 = 4;
const KEY_4: u32 = 5;
const KEY_5: u32 = 6;
const KEY_6: u32 = 7;
const KEY_7: u32 = 8;
const KEY_8: u32 = 9;
const KEY_9: u32 = 10;
const KEY_0: u32 = 11;
const KEY_MINUS: u32 = 12;
const KEY_EQUAL: u32 = 13;
const KEY_BACKSPACE: u32 = 14;
const KEY_TAB: u32 = 15;
const KEY_LEFTCTRL: u32 = 29;
const KEY_Q: u32 = 16;
const KEY_W: u32 = 17;
const KEY_E: u32 = 18;
const KEY_R: u32 = 19;
const KEY_T: u32 = 20;
const KEY_Y: u32 = 21;
const KEY_U: u32 = 22;
const KEY_I: u32 = 23;
const KEY_O: u32 = 24;
const KEY_P: u32 = 25;
const KEY_LEFTBRACE: u32 = 26;
const KEY_RIGHTBRACE: u32 = 27;
const KEY_ENTER: u32 = 28;
const KEY_A: u32 = 30;
const KEY_S: u32 = 31;
const KEY_D: u32 = 32;
const KEY_F: u32 = 33;
const KEY_G: u32 = 34;
const KEY_H: u32 = 35;
const KEY_J: u32 = 36;
const KEY_K: u32 = 37;
const KEY_L: u32 = 38;
const KEY_SEMICOLON: u32 = 39;
const KEY_APOSTROPHE: u32 = 40;
const KEY_GRAVE: u32 = 41;
const KEY_Z: u32 = 44;
const KEY_BACKSLASH: u32 = 43;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_X: u32 = 45;
const KEY_C: u32 = 46;
const KEY_V: u32 = 47;
const KEY_B: u32 = 48;
const KEY_N: u32 = 49;
const KEY_M: u32 = 50;
const KEY_COMMA: u32 = 51;
const KEY_DOT: u32 = 52;
const KEY_SLASH: u32 = 53;
const KEY_SPACE: u32 = 57;
const KEY_LEFTALT: u32 = 56;
const KEY_LEFT: u32 = 105;
const KEY_RIGHT: u32 = 106;
const KEY_UP: u32 = 103;
const KEY_DOWN: u32 = 108;
const KEY_DELETE: u32 = 111;
const KEY_LEFTMETA: u32 = 125;

struct App {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    virtual_keyboard_manager: Option<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1>,
    virtual_keyboard: Option<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1>,
    virtual_keyboard_keymap: Option<File>,
    seat: Option<wl_seat::WlSeat>,
    touch: Option<wl_touch::WlTouch>,
    surface: Option<wl_surface::WlSurface>,
    layer_surface: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    buffers: VecDeque<BufferBacking>,
    config: Config,
    width: u32,
    height: u32,
    engine: Engine,
    capture_policy: CapturePolicy,
    trace: Option<TraceRecorder>,
    started_at: Option<Instant>,
    mode_hint: Option<ModeHint>,
    last_presented_mode: Mode,
    running: bool,
}

impl Default for App {
    fn default() -> Self {
        let config = Config::default();
        let engine = Engine::default();
        let capture_policy = engine.capture_policy(&config);
        Self {
            compositor: None,
            shm: None,
            layer_shell: None,
            virtual_keyboard_manager: None,
            virtual_keyboard: None,
            virtual_keyboard_keymap: None,
            seat: None,
            touch: None,
            surface: None,
            layer_surface: None,
            buffers: VecDeque::new(),
            config,
            width: 0,
            height: 0,
            capture_policy,
            engine,
            trace: None,
            started_at: None,
            mode_hint: None,
            last_presented_mode: Mode::Base,
            running: false,
        }
    }
}

struct BufferBacking {
    _file: File,
    _mmap: MmapMut,
    _pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
    released: bool,
}

#[derive(Clone, Copy, Debug)]
struct ModeHint {
    mode: Mode,
    until_ms: u64,
}

#[derive(Clone)]
struct Config {
    action_swipe_left: Option<NiriAction>,
    action_swipe_right: Option<NiriAction>,
    action_swipe_up: Option<NiriAction>,
    action_swipe_down: Option<NiriAction>,
    action_two_finger_tap: Option<NiriAction>,
    tap_radius: f64,
    two_finger_tap_ms: u32,
    exit_tap_ms: u32,
    hold_ms: u32,
    double_tap_ms: u32,
    swipe_threshold_ratio: f64,
    swipe_threshold_min: f64,
    swipe_threshold_max: f64,
    debug_alpha: u8,
    debug_draw: bool,
    mode_hint_ms: u32,
    log_touch: bool,
    record_trace_path: Option<PathBuf>,
    xkb_keymap_path: Option<PathBuf>,
    slots: SlotRegistry,
    keymap: Keymap,
    macros: MacroRegistry,
    exit_corner_enabled: bool,
    exit_corner_ratio: f64,
    exit_corner_tap_ms: u32,
}

impl Default for Config {
    fn default() -> Self {
        let mut config = Self {
            action_swipe_left: env_niri_action("TOUCHDECK_ACTION_SWIPE_LEFT", "focus-column-left"),
            action_swipe_right: env_niri_action("TOUCHDECK_ACTION_SWIPE_RIGHT", "focus-column-right"),
            action_swipe_up: env_niri_action("TOUCHDECK_ACTION_SWIPE_UP", "focus-workspace-up"),
            action_swipe_down: env_niri_action("TOUCHDECK_ACTION_SWIPE_DOWN", "focus-workspace-down"),
            action_two_finger_tap: env_niri_action("TOUCHDECK_ACTION_TWO_FINGER_TAP", "toggle-overview"),
            tap_radius: env_f64("TOUCHDECK_TAP_RADIUS", 48.0),
            two_finger_tap_ms: env_u32("TOUCHDECK_TWO_FINGER_TAP_MS", 350),
            exit_tap_ms: env_u32("TOUCHDECK_EXIT_TAP_MS", 450),
            hold_ms: env_u32("TOUCHDECK_HOLD_MS", 180),
            double_tap_ms: env_u32("TOUCHDECK_DOUBLE_TAP_MS", 280),
            swipe_threshold_ratio: env_f64("TOUCHDECK_SWIPE_THRESHOLD_RATIO", 0.08),
            swipe_threshold_min: env_f64("TOUCHDECK_SWIPE_THRESHOLD_MIN", 64.0),
            swipe_threshold_max: env_f64("TOUCHDECK_SWIPE_THRESHOLD_MAX", 140.0),
            debug_alpha: env_u8("TOUCHDECK_DEBUG_ALPHA", 0),
            debug_draw: env_bool("TOUCHDECK_DEBUG_DRAW", false),
            mode_hint_ms: env_u32("TOUCHDECK_MODE_HINT_MS", 400),
            log_touch: env_bool("TOUCHDECK_LOG_TOUCH", false),
            record_trace_path: env::var_os("TOUCHDECK_RECORD_TRACE").map(PathBuf::from),
            xkb_keymap_path: env::var_os("TOUCHDECK_XKB_KEYMAP").map(PathBuf::from),
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

        config
    }
}

impl Config {
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
            if let Some(path) = &keyboard.xkb_keymap {
                self.xkb_keymap_path = Some(PathBuf::from(path));
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

        if let Some(bindings) = file_config.bindings {
            self.keymap.bindings.clear();
            for binding in bindings {
                self.keymap
                    .bindings
                    .push(Binding::from_file_config(binding, &self.slots, &self.macros)?);
            }
        }

        if let Some(keyboard) = keyboard {
            if let Some(maps) = keyboard.maps {
                self.keymap
                    .bindings
                    .extend(expand_keyboard_maps(maps, &self.slots)?);
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Base,
    Text,
    NiriMomentary,
    NiriLocked,
    Passthrough,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layer {
    Base,
    Niri,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotGestureKind {
    Tap,
    Hold,
    SwipeUp,
    SwipeDown,
    SwipeLeft,
    SwipeRight,
}

#[derive(Clone, Debug)]
struct Keymap {
    bindings: Vec<Binding>,
}

impl Default for Keymap {
    fn default() -> Self {
        let slots = SlotRegistry::default();
        let mut bindings = vec![
                Binding {
                    mode: Mode::Base,
                    layer: Layer::Base,
                    trigger: Trigger::Hold {
                        target: slots.get("left_bottom").unwrap(),
                        fingers: 1,
                        min_ms: None,
                    },
                    behavior: Behavior::ModeMomentary(Mode::NiriMomentary),
                    priority: 0,
                    consume: true,
                },
                Binding {
                    mode: Mode::Passthrough,
                    layer: Layer::Base,
                    trigger: Trigger::Hold {
                        target: slots.get("left_bottom").unwrap(),
                        fingers: 1,
                        min_ms: None,
                    },
                    behavior: Behavior::ModeMomentary(Mode::NiriMomentary),
                    priority: 0,
                    consume: true,
                },
                Binding {
                    mode: Mode::Base,
                    layer: Layer::Base,
                    trigger: Trigger::Swipe {
                        target: slots.get("bottom_edge").unwrap(),
                        fingers: 1,
                        direction: SwipeDirection::Up,
                        min_px: None,
                        max_ms: None,
                    },
                    behavior: Behavior::ModeToggle(Mode::Text),
                    priority: 0,
                    consume: true,
                },
                Binding {
                    mode: Mode::Passthrough,
                    layer: Layer::Base,
                    trigger: Trigger::Swipe {
                        target: slots.get("bottom_edge").unwrap(),
                        fingers: 1,
                        direction: SwipeDirection::Up,
                        min_px: None,
                        max_ms: None,
                    },
                    behavior: Behavior::ModeSet(Mode::Text),
                    priority: 0,
                    consume: true,
                },
                Binding {
                    mode: Mode::Text,
                    layer: Layer::Base,
                    trigger: Trigger::Swipe {
                        target: slots.get("bottom_edge").unwrap(),
                        fingers: 1,
                        direction: SwipeDirection::Down,
                        min_px: None,
                        max_ms: None,
                    },
                    behavior: Behavior::ModeSet(Mode::Base),
                    priority: 100,
                    consume: true,
                },
                Binding {
                    mode: Mode::Text,
                    layer: Layer::Base,
                    trigger: Trigger::Tap {
                        target: slots.get("top_left").unwrap(),
                        fingers: 1,
                        max_ms: None,
                    },
                    behavior: Behavior::Exit,
                    priority: 100,
                    consume: true,
                },
                Binding {
                    mode: Mode::Base,
                    layer: Layer::Base,
                    trigger: Trigger::DoubleTap {
                        target: slots.get("left_bottom").unwrap(),
                        fingers: 1,
                        max_ms: None,
                    },
                    behavior: Behavior::ModeToggle(Mode::NiriLocked),
                    priority: 0,
                    consume: true,
                },
                Binding {
                    mode: Mode::NiriLocked,
                    layer: Layer::Niri,
                    trigger: Trigger::DoubleTap {
                        target: slots.get("left_bottom").unwrap(),
                        fingers: 1,
                        max_ms: None,
                    },
                    behavior: Behavior::ModeSet(Mode::Base),
                    priority: 0,
                    consume: true,
                },
                Binding {
                    mode: Mode::Base,
                    layer: Layer::Base,
                    trigger: Trigger::DoubleTap {
                        target: slots.get("bottom_edge").unwrap(),
                        fingers: 1,
                        max_ms: None,
                    },
                    behavior: Behavior::ModeToggle(Mode::Passthrough),
                    priority: 0,
                    consume: true,
                },
                Binding {
                    mode: Mode::Passthrough,
                    layer: Layer::Base,
                    trigger: Trigger::DoubleTap {
                        target: slots.get("bottom_edge").unwrap(),
                        fingers: 1,
                        max_ms: None,
                    },
                    behavior: Behavior::ModeSet(Mode::Base),
                    priority: 0,
                    consume: true,
                },
                Binding {
                    mode: Mode::Base,
                    layer: Layer::Base,
                    trigger: Trigger::Tap {
                        target: slots.get("top_left").unwrap(),
                        fingers: 1,
                        max_ms: None,
                    },
                    behavior: Behavior::Exit,
                    priority: 0,
                    consume: true,
                },
                Binding {
                    mode: Mode::Passthrough,
                    layer: Layer::Base,
                    trigger: Trigger::Tap {
                        target: slots.get("top_left").unwrap(),
                        fingers: 1,
                        max_ms: None,
                    },
                    behavior: Behavior::Exit,
                    priority: 0,
                    consume: true,
                },
                Binding {
                    mode: Mode::NiriLocked,
                    layer: Layer::Niri,
                    trigger: Trigger::Tap {
                        target: slots.get("top_left").unwrap(),
                        fingers: 1,
                        max_ms: None,
                    },
                    behavior: Behavior::Exit,
                    priority: 0,
                    consume: true,
                },
            ];

        bindings.extend(expand_keyboard_maps(default_keyboard_maps(), &slots).expect("default keyboard map"));

        Self { bindings }
    }
}

impl Keymap {
    fn resolve_hold(
        &self,
        mode: Mode,
        layers: &[Layer],
        size: SurfaceSize,
        x: f64,
        y: f64,
        default_hold_ms: u32,
    ) -> Option<(GestureAction, u32)> {
        for layer in layers.iter().rev() {
            let mut matches = self
                .bindings
                .iter()
                .filter(|binding| {
                    binding.mode == mode
                        && binding.layer == *layer
                        && binding.trigger.matches_hold(size, x, y)
                })
                .collect::<Vec<_>>();
            matches.sort_by_key(|binding| std::cmp::Reverse(binding.priority));

            for binding in matches {
                if binding.behavior.is_transparent() || !binding.consume {
                    continue;
                }

                return Some((
                    binding.behavior.clone().into_action(),
                    binding.trigger.hold_ms().unwrap_or(default_hold_ms),
                ));
            }
        }

        None
    }

    fn resolve_release(
        &self,
        mode: Mode,
        layers: &[Layer],
        gesture: &Gesture,
        config: &Config,
        size: SurfaceSize,
        last_tap: &mut Option<TapRecord>,
        now_ms: u64,
    ) -> GestureAction {
        let Some(kind) = recognize_gesture_kind(gesture, config, size) else {
            return GestureAction::None;
        };

        let Some(contact) = gesture.finished.first() else {
            return GestureAction::None;
        };

        if kind == GestureKind::Tap {
            let double_tap_binding = self.find_release_binding(
                mode,
                layers,
                |binding| {
                    binding.trigger.matches_double_tap_start(
                        size,
                        contact.start_x,
                        contact.start_y,
                        gesture.max_active,
                    )
                },
            );

            if let Some(binding) = double_tap_binding {
                let max_ms = binding.trigger.max_ms().unwrap_or(config.double_tap_ms);
                let is_double_tap = last_tap.is_some_and(|last| {
                    now_ms.saturating_sub(last.t_ms) <= u64::from(max_ms)
                        && (contact.start_x - last.x).hypot(contact.start_y - last.y)
                            <= config.tap_radius * 2.0
                        && binding.trigger.rect().contains_px(size, last.x, last.y)
                });

                if is_double_tap {
                    *last_tap = None;
                    return binding.behavior.clone().into_action();
                }

                *last_tap = Some(TapRecord {
                    t_ms: now_ms,
                    x: contact.start_x,
                    y: contact.start_y,
                });
                return GestureAction::None;
            }
        } else {
            *last_tap = None;
        }

        self.find_release_binding(mode, layers, |binding| {
            binding.trigger.matches_release(kind, gesture, config, size)
        })
            .map(|binding| binding.behavior.clone().into_action())
            .unwrap_or(GestureAction::None)
    }

    fn find_release_binding<F>(
        &self,
        mode: Mode,
        layers: &[Layer],
        mut predicate: F,
    ) -> Option<&Binding>
    where
        F: FnMut(&Binding) -> bool,
    {
        for layer in layers.iter().rev() {
            let mut matches = self
                .bindings
                .iter()
                .filter(|binding| binding.mode == mode && binding.layer == *layer && predicate(binding))
                .collect::<Vec<_>>();
            matches.sort_by_key(|binding| std::cmp::Reverse(binding.priority));

            for binding in matches {
                if binding.behavior.is_transparent() || !binding.consume {
                    continue;
                }
                return Some(binding);
            }
        }

        None
    }

    fn capture_rects(&self, mode: Mode, layers: &[Layer]) -> Vec<RectNorm> {
        let mut rects = Vec::new();
        let mut seen = Vec::new();

        for binding in &self.bindings {
            if binding.mode != mode
                || !layers.contains(&binding.layer)
                || !binding.consume
                || binding.behavior.is_transparent()
            {
                continue;
            }

            let target = binding.trigger.target();
            if !target.capture || seen.iter().any(|id: &String| id == &target.id) {
                continue;
            }

            seen.push(target.id.clone());
            rects.push(target.rect);
        }

        rects
    }

    fn slot_label(&self, mode: Mode, layers: &[Layer], slot_id: &str) -> Option<String> {
        self.slot_label_from_bindings(mode, layers, slot_id, true)
            .or_else(|| self.slot_label_from_bindings(mode, layers, slot_id, false))
    }

    fn slot_gesture_label(
        &self,
        mode: Mode,
        layers: &[Layer],
        slot_id: &str,
        gesture: SlotGestureKind,
    ) -> Option<String> {
        for layer in layers.iter().rev() {
            let mut matches = self
                .bindings
                .iter()
                .filter(|binding| {
                    binding.mode == mode
                        && binding.layer == *layer
                        && binding.consume
                        && binding.trigger.target_id() == slot_id
                        && binding.trigger.matches_slot_gesture(gesture)
                })
                .collect::<Vec<_>>();
            matches.sort_by_key(|binding| std::cmp::Reverse(binding.priority));

            for binding in matches {
                if binding.behavior.is_transparent() {
                    continue;
                }

                if let Some(label) = behavior_label(&binding.behavior) {
                    return Some(label);
                }
            }
        }

        None
    }

    fn slot_label_from_bindings(
        &self,
        mode: Mode,
        layers: &[Layer],
        slot_id: &str,
        tap_only: bool,
    ) -> Option<String> {
        for layer in layers.iter().rev() {
            let mut matches = self
                .bindings
                .iter()
                .filter(|binding| {
                    binding.mode == mode
                        && binding.layer == *layer
                        && binding.consume
                        && binding.trigger.target_id() == slot_id
                        && (!tap_only || binding.trigger.is_tap())
                })
                .collect::<Vec<_>>();
            matches.sort_by_key(|binding| std::cmp::Reverse(binding.priority));

            for binding in matches {
                if binding.behavior.is_transparent() {
                    continue;
                }

                if let Some(label) = behavior_label(&binding.behavior) {
                    return Some(label);
                }
            }
        }

        None
    }
}

#[derive(Clone, Debug)]
struct Binding {
    mode: Mode,
    layer: Layer,
    trigger: Trigger,
    behavior: Behavior,
    priority: i32,
    consume: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KeyChord {
    keys: Vec<u32>,
}

#[derive(Clone, Debug)]
struct Layout {
    slots: HashMap<String, Slot>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct Slot {
    id: String,
    rect: RectNorm,
    role: SlotRole,
    capture: bool,
    label: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
enum SlotRole {
    Key,
    Zone,
    GestureArea,
}

#[derive(Clone, Debug, PartialEq)]
struct SlotTarget {
    id: String,
    rect: RectNorm,
    role: SlotRole,
    capture: bool,
    label: Option<String>,
}

#[derive(Clone, Debug)]
struct SlotRegistry {
    layout: Layout,
}

impl Default for SlotRegistry {
    fn default() -> Self {
        let mut registry = Self {
            layout: Layout {
                slots: HashMap::new(),
            },
        };
        registry.insert_slot("left_bottom", left_bottom_rect(), SlotRole::Zone, true, Some("NIRI"));
        registry.insert_slot("bottom_edge", bottom_edge_rect(), SlotRole::GestureArea, true, Some("TEXT"));
        registry.insert_slot("top_left", top_left_rect(), SlotRole::Zone, true, Some("EXIT"));
        registry.insert_slot(
            "center",
            RectNorm {
                x0: 0.18,
                y0: 0.12,
                x1: 0.82,
                y1: 0.82,
            },
            SlotRole::GestureArea,
            false,
            None,
        );
        registry.insert_slot(
            "full",
            RectNorm {
                x0: 0.0,
                y0: 0.0,
                x1: 1.0,
                y1: 1.0,
            },
            SlotRole::GestureArea,
            false,
            Some("SCREEN"),
        );
        registry.insert_slot(
            "fullscreen",
            RectNorm {
                x0: 0.0,
                y0: 0.0,
                x1: 1.0,
                y1: 1.0,
            },
            SlotRole::GestureArea,
            false,
            Some("SCREEN"),
        );
        insert_default_key_slots(&mut registry);
        registry
    }
}

fn insert_default_key_slots(registry: &mut SlotRegistry) {
    let rows = [
        (0.030, 0.520, 0.088, &["q", "w", "e", "r", "t"][..]),
        (0.540, 0.520, 0.088, &["y", "u", "i", "o", "p"][..]),
        (0.052, 0.625, 0.088, &["a", "s", "d", "f", "g"][..]),
        (0.562, 0.625, 0.088, &["h", "j", "k", "l"][..]),
        (0.074, 0.730, 0.088, &["z", "x", "c", "v", "b"][..]),
        (0.606, 0.730, 0.088, &["n", "m"][..]),
    ];

    for (x0, y0, step, keys) in rows {
        for (index, key) in keys.iter().enumerate() {
            let x0 = x0 + index as f64 * step;
            registry.insert_slot(
                &format!("key_{key}"),
                RectNorm {
                    x0,
                    y0,
                    x1: x0 + 0.078,
                    y1: y0 + 0.083,
                },
                SlotRole::Key,
                true,
                Some(key),
            );
        }
    }

    for (slot, label, x0, y0, w, h) in [
        ("key_shift", "SFT", 0.040, 0.450, 0.130, 0.060),
        ("key_ctrl", "CTL", 0.200, 0.450, 0.130, 0.060),
        ("key_alt", "ALT", 0.670, 0.450, 0.130, 0.060),
        ("key_super", "SUP", 0.830, 0.450, 0.130, 0.060),
        ("key_esc", "ESC", 0.040, 0.815, 0.130, 0.075),
        ("key_spc", "SPC", 0.200, 0.815, 0.360, 0.075),
        ("key_del", "DEL", 0.590, 0.815, 0.160, 0.075),
        ("key_ret", "RET", 0.780, 0.815, 0.180, 0.075),
    ] {
        registry.insert_slot(
            slot,
            RectNorm {
                x0,
                y0,
                x1: x0 + w,
                y1: y0 + h,
            },
            SlotRole::Key,
            true,
            Some(label),
        );
    }
}

#[derive(Clone, Debug, Default)]
struct MacroRegistry {
    macros: HashMap<String, Vec<ActionStep>>,
}

impl MacroRegistry {
    fn clear(&mut self) {
        self.macros.clear();
    }

    fn insert(&mut self, name: &str, steps: Vec<ActionStep>) {
        self.macros.insert(normalize_name(name), steps);
    }

    fn get(&self, name: &str) -> Result<Vec<ActionStep>> {
        self.macros
            .get(&normalize_name(name))
            .cloned()
            .ok_or_else(|| anyhow!("unknown macro {name}"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ActionStep {
    KeyDown(u32),
    KeyUp(u32),
    TapKey(u32),
    KeySequence(Vec<KeyChord>),
    Niri(NiriAction),
    DelayMs(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NiriAction {
    FocusColumnLeft,
    FocusColumnRight,
    FocusWorkspaceUp,
    FocusWorkspaceDown,
    ToggleOverview,
}

impl NiriAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::FocusColumnLeft => "focus-column-left",
            Self::FocusColumnRight => "focus-column-right",
            Self::FocusWorkspaceUp => "focus-workspace-up",
            Self::FocusWorkspaceDown => "focus-workspace-down",
            Self::ToggleOverview => "toggle-overview",
        }
    }

    fn ipc_request_json(self) -> &'static str {
        match self {
            Self::FocusColumnLeft => r#"{"Action":{"FocusColumnLeft":{}}}"#,
            Self::FocusColumnRight => r#"{"Action":{"FocusColumnRight":{}}}"#,
            Self::FocusWorkspaceUp => r#"{"Action":{"FocusWorkspaceUp":{}}}"#,
            Self::FocusWorkspaceDown => r#"{"Action":{"FocusWorkspaceDown":{}}}"#,
            Self::ToggleOverview => r#"{"Action":{"ToggleOverview":{}}}"#,
        }
    }
}

impl std::fmt::Display for NiriAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl SlotRegistry {
    fn from_svg_file(path: &Path) -> Result<Self> {
        let source = fs::read_to_string(path)
            .with_context(|| format!("read SVG layout {}", path.display()))?;
        Self::from_svg_str(&source).with_context(|| format!("parse SVG layout {}", path.display()))
    }

    fn from_svg_str(source: &str) -> Result<Self> {
        let document = roxmltree::Document::parse(source).context("parse SVG XML")?;
        let root = document
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "svg")
            .ok_or_else(|| anyhow!("SVG layout is missing root <svg> element"))?;
        let (view_x, view_y, view_w, view_h) = svg_canvas(root)?;
        let mut registry = Self {
            layout: Layout {
                slots: HashMap::new(),
            },
        };

        for node in document
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "rect")
        {
            let Some(slot_id) = node.attribute("data-td-slot") else {
                continue;
            };

            let x = svg_number_attr(node, "x")?;
            let y = svg_number_attr(node, "y")?;
            let width = svg_number_attr(node, "width")?;
            let height = svg_number_attr(node, "height")?;
            let rect = validate_rect(
                RectNorm {
                    x0: (x - view_x) / view_w,
                    y0: (y - view_y) / view_h,
                    x1: (x + width - view_x) / view_w,
                    y1: (y + height - view_y) / view_h,
                },
                "SVG slot",
            )?;
            let role = parse_slot_role(node.attribute("data-td-role"))?;
            let capture = parse_optional_bool(node.attribute("data-td-capture"))?.unwrap_or(true);
            let label = node.attribute("data-td-label");
            let id = normalize_name(slot_id);

            if registry.layout.slots.contains_key(&id) {
                return Err(anyhow!("duplicate SVG slot {slot_id}"));
            }

            registry.insert_slot(&id, rect, role, capture, label);
        }

        if registry.layout.slots.is_empty() {
            return Err(anyhow!("SVG layout contains no rect with data-td-slot"));
        }

        Ok(registry)
    }

    fn get(&self, name: &str) -> Result<SlotTarget> {
        let key = normalize_name(name);
        self.layout
            .slots
            .get(&key)
            .map(|slot| SlotTarget {
                id: slot.id.clone(),
                rect: slot.rect,
                role: slot.role,
                capture: slot.capture,
                label: slot.label.clone(),
            })
            .ok_or_else(|| anyhow!("unknown slot {name}"))
    }

    fn slots(&self) -> impl Iterator<Item = &Slot> {
        self.layout.slots.values()
    }

    fn insert_slot(
        &mut self,
        name: &str,
        rect: RectNorm,
        role: SlotRole,
        capture: bool,
        label: Option<&str>,
    ) {
        let id = normalize_name(name);
        self.layout.slots.insert(
            id.clone(),
            Slot {
                id,
                rect,
                role,
                capture,
                label: label.map(str::to_string),
            },
        );
    }
}

fn svg_canvas(root: roxmltree::Node<'_, '_>) -> Result<(f64, f64, f64, f64)> {
    if let Some(view_box) = root.attribute("viewBox") {
        let values = view_box
            .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
            .filter(|value| !value.is_empty())
            .map(parse_svg_number)
            .collect::<Result<Vec<_>>>()?;
        if values.len() != 4 {
            return Err(anyhow!("SVG viewBox must contain four numbers"));
        }
        if values[2] <= 0.0 || values[3] <= 0.0 {
            return Err(anyhow!("SVG viewBox width/height must be positive"));
        }
        return Ok((values[0], values[1], values[2], values[3]));
    }

    let width = svg_number_attr(root, "width")?;
    let height = svg_number_attr(root, "height")?;
    if width <= 0.0 || height <= 0.0 {
        return Err(anyhow!("SVG width/height must be positive"));
    }
    Ok((0.0, 0.0, width, height))
}

fn svg_number_attr(node: roxmltree::Node<'_, '_>, name: &str) -> Result<f64> {
    parse_svg_number(
        node.attribute(name)
            .ok_or_else(|| anyhow!("SVG <{}> is missing {name}", node.tag_name().name()))?,
    )
}

fn parse_svg_number(value: &str) -> Result<f64> {
    let value = value.trim();
    if value.is_empty() || value.contains('%') {
        return Err(anyhow!("unsupported SVG numeric value {value:?}"));
    }
    let value = value.strip_suffix("px").unwrap_or(value).trim();
    value
        .parse::<f64>()
        .with_context(|| format!("parse SVG numeric value {value:?}"))
}

fn parse_slot_role(value: Option<&str>) -> Result<SlotRole> {
    match value.map(normalize_name).as_deref() {
        None | Some("") | Some("zone") => Ok(SlotRole::Zone),
        Some("key") => Ok(SlotRole::Key),
        Some("gesture") | Some("gesture_area") => Ok(SlotRole::GestureArea),
        Some(other) => Err(anyhow!("unknown SVG slot role {other}")),
    }
}

fn parse_optional_bool(value: Option<&str>) -> Result<Option<bool>> {
    match value.map(normalize_name).as_deref() {
        None | Some("") => Ok(None),
        Some("1") | Some("true") | Some("yes") | Some("on") => Ok(Some(true)),
        Some("0") | Some("false") | Some("no") | Some("off") => Ok(Some(false)),
        Some(other) => Err(anyhow!("invalid boolean value {other}")),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GestureKind {
    Tap,
    SwipeLeft,
    SwipeRight,
    SwipeUp,
    SwipeDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SwipeDirection {
    Left,
    Right,
    Up,
    Down,
}

impl SwipeDirection {
    fn as_gesture_kind(self) -> GestureKind {
        match self {
            Self::Left => GestureKind::SwipeLeft,
            Self::Right => GestureKind::SwipeRight,
            Self::Up => GestureKind::SwipeUp,
            Self::Down => GestureKind::SwipeDown,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Trigger {
    Tap {
        target: SlotTarget,
        fingers: usize,
        max_ms: Option<u32>,
    },
    DoubleTap {
        target: SlotTarget,
        fingers: usize,
        max_ms: Option<u32>,
    },
    Hold {
        target: SlotTarget,
        fingers: usize,
        min_ms: Option<u32>,
    },
    Swipe {
        target: SlotTarget,
        fingers: usize,
        direction: SwipeDirection,
        min_px: Option<f64>,
        max_ms: Option<u32>,
    },
}

impl Trigger {
    fn target(&self) -> &SlotTarget {
        match self {
            Self::Tap { target, .. }
            | Self::DoubleTap { target, .. }
            | Self::Hold { target, .. }
            | Self::Swipe { target, .. } => target,
        }
    }

    fn rect(&self) -> RectNorm {
        self.target().rect
    }

    #[allow(dead_code)]
    fn target_id(&self) -> &str {
        &self.target().id
    }

    fn is_tap(&self) -> bool {
        matches!(self, Self::Tap { .. })
    }

    fn matches_slot_gesture(&self, gesture: SlotGestureKind) -> bool {
        match (self, gesture) {
            (Self::Tap { .. }, SlotGestureKind::Tap) => true,
            (Self::Hold { .. }, SlotGestureKind::Hold) => true,
            (
                Self::Swipe {
                    direction: SwipeDirection::Up,
                    ..
                },
                SlotGestureKind::SwipeUp,
            ) => true,
            (
                Self::Swipe {
                    direction: SwipeDirection::Down,
                    ..
                },
                SlotGestureKind::SwipeDown,
            ) => true,
            (
                Self::Swipe {
                    direction: SwipeDirection::Left,
                    ..
                },
                SlotGestureKind::SwipeLeft,
            ) => true,
            (
                Self::Swipe {
                    direction: SwipeDirection::Right,
                    ..
                },
                SlotGestureKind::SwipeRight,
            ) => true,
            _ => false,
        }
    }

    fn max_ms(&self) -> Option<u32> {
        match self {
            Self::Tap { max_ms, .. }
            | Self::DoubleTap { max_ms, .. }
            | Self::Swipe { max_ms, .. } => *max_ms,
            Self::Hold { .. } => None,
        }
    }

    fn hold_ms(&self) -> Option<u32> {
        match self {
            Self::Hold { min_ms, .. } => *min_ms,
            _ => None,
        }
    }

    fn matches_hold(&self, size: SurfaceSize, x: f64, y: f64) -> bool {
        match self {
            Self::Hold {
                target, fingers, ..
            } => *fingers == 1 && target.rect.contains_px(size, x, y),
            _ => false,
        }
    }

    fn matches_double_tap_start(
        &self,
        size: SurfaceSize,
        x: f64,
        y: f64,
        fingers: usize,
    ) -> bool {
        match self {
            Self::DoubleTap {
                target,
                fingers: expected_fingers,
                ..
            } => *expected_fingers == fingers && target.rect.contains_px(size, x, y),
            _ => false,
        }
    }

    fn matches_release(
        &self,
        kind: GestureKind,
        gesture: &Gesture,
        config: &Config,
        size: SurfaceSize,
    ) -> bool {
        let Some(contact) = gesture.finished.first() else {
            return false;
        };

        match self {
            Self::Tap {
                target,
                fingers,
                max_ms,
            } => {
                kind == GestureKind::Tap
                    && gesture.max_active == *fingers
                    && target.rect.contains_px(size, contact.start_x, contact.start_y)
                    && is_tap_like(gesture, config.tap_radius, max_ms.unwrap_or(config.two_finger_tap_ms))
            }
            Self::DoubleTap { .. } | Self::Hold { .. } => false,
            Self::Swipe {
                target,
                fingers,
                direction,
                min_px,
                max_ms,
            } => {
                kind == direction.as_gesture_kind()
                    && gesture.max_active == *fingers
                    && target.rect.contains_px(size, contact.start_x, contact.start_y)
                    && min_px.is_none_or(|threshold| contact_movement(contact) >= threshold)
                    && max_ms.is_none_or(|limit| contact.last_time.saturating_sub(contact.start_time) <= limit)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Behavior {
    Niri(NiriAction),
    KeySequence(Vec<KeyChord>),
    KeyHold(u32),
    Sequence(Vec<ActionStep>),
    ModeSet(Mode),
    ModeToggle(Mode),
    ModeMomentary(Mode),
    LayerSet(Layer),
    LayerToggle(Layer),
    LayerMomentary(Layer),
    Transparent,
    NoOp,
    Exit,
}

impl Behavior {
    fn is_transparent(&self) -> bool {
        matches!(self, Self::Transparent)
    }

    fn into_action(self) -> GestureAction {
        match self {
            Self::Niri(action) => GestureAction::Niri(action),
            Self::KeySequence(sequence) => GestureAction::KeySequence(sequence),
            Self::KeyHold(key) => GestureAction::KeyHold(key),
            Self::Sequence(steps) => GestureAction::Sequence(steps),
            Self::ModeSet(mode) => GestureAction::ModeSet(mode),
            Self::ModeToggle(mode) => GestureAction::ModeToggle(mode),
            Self::ModeMomentary(mode) => GestureAction::ModeMomentary(mode),
            Self::LayerSet(layer) => GestureAction::LayerSet(layer),
            Self::LayerToggle(layer) => GestureAction::LayerToggle(layer),
            Self::LayerMomentary(layer) => GestureAction::LayerMomentary(layer),
            Self::Exit => GestureAction::Exit,
            Self::Transparent | Self::NoOp => GestureAction::None,
        }
    }
}

#[derive(Deserialize)]
struct FileConfig {
    layout: Option<LayoutFileConfig>,
    keyboard: Option<KeyboardFileConfig>,
    macros: Option<HashMap<String, MacroFileConfig>>,
    bindings: Option<Vec<BindingFileConfig>>,
}

#[derive(Deserialize)]
struct LayoutFileConfig {
    svg: Option<String>,
}

#[derive(Deserialize)]
struct KeyboardFileConfig {
    xkb_keymap: Option<String>,
    maps: Option<Vec<KeyboardMapFileConfig>>,
}

#[derive(Deserialize)]
struct KeyboardMapFileConfig {
    mode: Option<String>,
    layer: Option<String>,
    tap: Option<HashMap<String, String>>,
    hold: Option<HashMap<String, String>>,
    swipe_up: Option<HashMap<String, String>>,
    swipe_down: Option<HashMap<String, String>>,
    swipe_left: Option<HashMap<String, String>>,
    swipe_right: Option<HashMap<String, String>>,
    fingers: Option<usize>,
    max_ms: Option<u32>,
    hold_ms: Option<u32>,
    min_px: Option<f64>,
    priority: Option<i32>,
    consume: Option<bool>,
}

#[derive(Deserialize)]
struct MacroFileConfig {
    steps: Vec<ActionStepFileConfig>,
}

#[derive(Deserialize)]
struct ActionStepFileConfig {
    #[serde(rename = "type")]
    kind: String,
    key: Option<String>,
    keys: Option<String>,
    action: Option<String>,
    ms: Option<u32>,
}

#[derive(Deserialize)]
struct BindingFileConfig {
    mode: Option<String>,
    layer: Option<String>,
    trigger: TriggerFileConfig,
    behavior: BehaviorFileConfig,
    priority: Option<i32>,
    consume: Option<bool>,
}

#[derive(Deserialize)]
struct TriggerFileConfig {
    #[serde(rename = "type")]
    kind: String,
    target: String,
    direction: Option<String>,
    fingers: Option<usize>,
    min_ms: Option<u32>,
    max_ms: Option<u32>,
    min_px: Option<f64>,
}

#[derive(Deserialize)]
struct BehaviorFileConfig {
    #[serde(rename = "type")]
    kind: String,
    key: Option<String>,
    keys: Option<String>,
    action: Option<String>,
    macro_name: Option<String>,
    #[serde(rename = "macro")]
    macro_alias: Option<String>,
    steps: Option<Vec<ActionStepFileConfig>>,
    mode: Option<String>,
    layer: Option<String>,
}

impl Binding {
    fn from_file_config(
        value: BindingFileConfig,
        slots: &SlotRegistry,
        macros: &MacroRegistry,
    ) -> Result<Self> {
        let mode = parse_mode(value.mode.as_deref().unwrap_or("base"))?;
        let layer = parse_layer(value.layer.as_deref().unwrap_or("base"))?;
        let trigger = parse_trigger(value.trigger, slots)?;
        let behavior = parse_behavior(value.behavior, macros)?;

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

fn default_keyboard_maps() -> Vec<KeyboardMapFileConfig> {
    vec![keyboard_map_config(
        &[
            ("key_q", "q"),
            ("key_w", "w"),
            ("key_e", "e"),
            ("key_r", "r"),
            ("key_t", "t"),
            ("key_y", "y"),
            ("key_u", "u"),
            ("key_i", "i"),
            ("key_o", "o"),
            ("key_p", "p"),
            ("key_a", "a"),
            ("key_s", "s"),
            ("key_d", "d"),
            ("key_f", "f"),
            ("key_g", "g"),
            ("key_h", "h"),
            ("key_j", "j"),
            ("key_k", "k"),
            ("key_l", "l"),
            ("key_z", "z"),
            ("key_x", "x"),
            ("key_c", "c"),
            ("key_v", "v"),
            ("key_b", "b"),
            ("key_n", "n"),
            ("key_m", "m"),
            ("key_esc", "ESC"),
            ("key_spc", "SPC"),
            ("key_del", "DEL"),
            ("key_ret", "RET"),
        ],
        &[
            ("key_q", "1"),
            ("key_w", "2"),
            ("key_e", "3"),
            ("key_r", "4"),
            ("key_t", "5"),
            ("key_y", "6"),
            ("key_u", "7"),
            ("key_i", "8"),
            ("key_o", "9"),
            ("key_p", "0"),
            ("key_a", "!"),
            ("key_s", "@"),
            ("key_d", "#"),
            ("key_f", "$"),
            ("key_g", "%"),
            ("key_h", "^"),
            ("key_j", "&"),
            ("key_k", "<up>"),
            ("key_l", "_"),
            ("key_z", "`"),
            ("key_x", "-"),
            ("key_c", "="),
            ("key_v", "["),
            ("key_b", "]"),
            ("key_n", "/"),
            ("key_m", "?"),
            ("key_spc", "TAB"),
            ("key_del", "DELETE"),
            ("key_ret", "C-RET"),
        ],
        &[("key_j", "<down>"), ("key_k", "*"), ("key_spc", "ESC")],
        &[
            ("key_h", "<left>"),
            ("key_spc", "DEL"),
            ("key_del", "M-DEL"),
            ("key_ret", "C-<left>"),
        ],
        &[
            ("key_l", "<right>"),
            ("key_spc", "RET"),
            ("key_del", "C-<right>"),
            ("key_ret", "C-<right>"),
        ],
    )]
}

fn keyboard_map_config(
    tap: &[(&str, &str)],
    swipe_up: &[(&str, &str)],
    swipe_down: &[(&str, &str)],
    swipe_left: &[(&str, &str)],
    swipe_right: &[(&str, &str)],
) -> KeyboardMapFileConfig {
    KeyboardMapFileConfig {
        mode: Some("text".to_string()),
        layer: Some("base".to_string()),
        tap: Some(key_pairs(tap)),
        hold: Some(key_pairs(&[
            ("key_shift", "<leftshift>"),
            ("key_ctrl", "<leftctrl>"),
            ("key_alt", "<leftalt>"),
            ("key_super", "<leftmeta>"),
        ])),
        swipe_up: Some(key_pairs(swipe_up)),
        swipe_down: Some(key_pairs(swipe_down)),
        swipe_left: Some(key_pairs(swipe_left)),
        swipe_right: Some(key_pairs(swipe_right)),
        fingers: None,
        max_ms: None,
        hold_ms: None,
        min_px: None,
        priority: None,
        consume: None,
    }
}

fn key_pairs(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(slot, key)| ((*slot).to_string(), (*key).to_string()))
        .collect()
}

fn expand_keyboard_maps(maps: Vec<KeyboardMapFileConfig>, slots: &SlotRegistry) -> Result<Vec<Binding>> {
    let mut bindings = Vec::new();

    for (map_index, map) in maps.into_iter().enumerate() {
        let mode = parse_mode(map.mode.as_deref().unwrap_or("text"))?;
        let layer = parse_layer(map.layer.as_deref().unwrap_or("base"))?;
        let fingers = map.fingers.unwrap_or(1);
        let max_ms = map.max_ms;
        let hold_ms = map.hold_ms;
        let min_px = map.min_px;
        let priority = map.priority.unwrap_or(0);
        let consume = map.consume.unwrap_or(true);

        expand_keyboard_gesture_map(
            &mut bindings,
            slots,
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
        )?;
        expand_keyboard_hold_map(
            &mut bindings,
            slots,
            map_index,
            mode,
            layer,
            map.hold,
            fingers,
            hold_ms,
            priority,
            consume,
        )?;
        expand_keyboard_gesture_map(
            &mut bindings,
            slots,
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
        )?;
        expand_keyboard_gesture_map(
            &mut bindings,
            slots,
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
        )?;
        expand_keyboard_gesture_map(
            &mut bindings,
            slots,
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
        )?;
        expand_keyboard_gesture_map(
            &mut bindings,
            slots,
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
        )?;
    }

    Ok(bindings)
}

fn expand_keyboard_hold_map(
    bindings: &mut Vec<Binding>,
    slots: &SlotRegistry,
    map_index: usize,
    mode: Mode,
    layer: Layer,
    keys: Option<HashMap<String, String>>,
    fingers: usize,
    hold_ms: Option<u32>,
    priority: i32,
    consume: bool,
) -> Result<()> {
    let Some(keys) = keys else {
        return Ok(());
    };

    for (slot_id, key) in keys {
        let target = slots
            .get(&slot_id)
            .with_context(|| format!("keyboard map {map_index} hold target {slot_id}"))?;
        bindings.push(Binding {
            mode,
            layer,
            trigger: Trigger::Hold {
                target,
                fingers,
                min_ms: hold_ms,
            },
            behavior: Behavior::KeyHold(parse_single_emacs_key(&key).with_context(|| {
                format!("parse keyboard map {map_index} hold key for {slot_id} ({key})")
            })?),
            priority,
            consume,
        });
    }

    Ok(())
}

fn expand_keyboard_gesture_map<F>(
    bindings: &mut Vec<Binding>,
    slots: &SlotRegistry,
    map_index: usize,
    mode: Mode,
    layer: Layer,
    gesture_name: &str,
    keys: Option<HashMap<String, String>>,
    make_trigger: F,
    priority: i32,
    consume: bool,
) -> Result<()>
where
    F: Fn(SlotTarget) -> Trigger,
{
    let Some(keys) = keys else {
        return Ok(());
    };

    for (slot_id, key) in keys {
        let target = slots
            .get(&slot_id)
            .with_context(|| format!("keyboard map {map_index} {gesture_name} target {slot_id}"))?;
        bindings.push(Binding {
            mode,
            layer,
            trigger: make_trigger(target),
            behavior: Behavior::KeySequence(parse_emacs_key_sequence(&key).with_context(|| {
                format!("parse keyboard map {map_index} {gesture_name} key for {slot_id} ({key})")
            })?),
            priority,
            consume,
        });
    }

    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
enum CapturePolicy {
    Fullscreen,
    Zones(Vec<RectNorm>),
    #[allow(dead_code)]
    None,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RectNorm {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl RectNorm {
    fn contains_px(self, size: SurfaceSize, x: f64, y: f64) -> bool {
        let width = f64::from(size.width.max(1));
        let height = f64::from(size.height.max(1));
        x >= width * self.x0
            && x <= width * self.x1
            && y >= height * self.y0
            && y <= height * self.y1
    }

    fn to_px(self, size: SurfaceSize) -> RectPx {
        let width = f64::from(size.width.max(1));
        let height = f64::from(size.height.max(1));
        let x0 = (width * self.x0).floor().max(0.0) as i32;
        let y0 = (height * self.y0).floor().max(0.0) as i32;
        let x1 = (width * self.x1).ceil().max(0.0) as i32;
        let y1 = (height * self.y1).ceil().max(0.0) as i32;

        RectPx {
            x: x0,
            y: y0,
            w: (x1 - x0).max(0),
            h: (y1 - y0).max(0),
        }
    }
}

#[derive(Clone, Copy)]
struct RectPx {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

#[derive(Clone, Copy)]
struct SurfaceSize {
    width: u32,
    height: u32,
}

#[derive(Clone, Debug)]
struct Contact {
    id: i32,
    start_x: f64,
    start_y: f64,
    last_x: f64,
    last_y: f64,
    start_time: u32,
    last_time: u32,
}

#[derive(Debug, Default)]
struct Gesture {
    finished: Vec<Contact>,
    max_active: usize,
}

#[derive(Clone, Debug)]
struct HoldCandidate {
    id: i32,
    deadline_ms: u64,
    action: GestureAction,
}

#[derive(Clone, Copy, Debug)]
struct TapRecord {
    t_ms: u64,
    x: f64,
    y: f64,
}

#[derive(Clone, Debug)]
struct MomentaryState {
    hold_id: i32,
    return_mode: Mode,
    return_layer_stack: Vec<Layer>,
}

#[derive(Clone, Debug)]
struct HeldKeyState {
    hold_id: i32,
    key: u32,
}

#[derive(Debug)]
struct Engine {
    mode: Mode,
    layer_stack: Vec<Layer>,
    active: HashMap<i32, Contact>,
    finished: Vec<Contact>,
    max_active: usize,
    hold_candidate: Option<HoldCandidate>,
    momentary: Option<MomentaryState>,
    held_keys: Vec<HeldKeyState>,
    last_tap: Option<TapRecord>,
    last_action: Option<String>,
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            mode: Mode::Base,
            layer_stack: vec![Layer::Base],
            active: HashMap::new(),
            finished: Vec::new(),
            max_active: 0,
            hold_candidate: None,
            momentary: None,
            held_keys: Vec::new(),
            last_tap: None,
            last_action: None,
        }
    }
}

#[derive(Debug, PartialEq)]
enum EngineEffect {
    SetCapture(CapturePolicy),
    Dispatch(GestureAction),
    Redraw,
}

#[derive(Debug, PartialEq, Eq, Clone)]
enum GestureAction {
    Niri(NiriAction),
    KeySequence(Vec<KeyChord>),
    KeyHold(u32),
    KeyRelease(u32),
    Sequence(Vec<ActionStep>),
    ModeSet(Mode),
    ModeToggle(Mode),
    ModeMomentary(Mode),
    LayerSet(Layer),
    LayerToggle(Layer),
    LayerMomentary(Layer),
    Exit,
    None,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TraceEvent {
    Down {
        t: u64,
        wl_time: u32,
        id: i32,
        x: f64,
        y: f64,
    },
    Motion {
        t: u64,
        wl_time: u32,
        id: i32,
        x: f64,
        y: f64,
    },
    Up {
        t: u64,
        wl_time: u32,
        id: i32,
    },
    Cancel {
        t: u64,
    },
}

impl TraceEvent {
    #[cfg(test)]
    fn t(&self) -> u64 {
        match self {
            TraceEvent::Down { t, .. }
            | TraceEvent::Motion { t, .. }
            | TraceEvent::Up { t, .. }
            | TraceEvent::Cancel { t } => *t,
        }
    }
}

struct TraceRecorder {
    file: File,
}

impl TraceRecorder {
    fn new(path: &PathBuf) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .with_context(|| format!("open trace file {}", path.display()))?;
        Ok(Self { file })
    }

    fn record(&mut self, event: &TraceEvent) -> Result<()> {
        serde_json::to_writer(&mut self.file, event).context("write trace event")?;
        self.file.write_all(b"\n").context("write trace newline")?;
        Ok(())
    }
}

fn main() -> Result<()> {
    let config = Config::default();
    let trace = if let Some(path) = &config.record_trace_path {
        Some(TraceRecorder::new(path)?)
    } else {
        None
    };

    let conn = Connection::connect_to_env().context("connect to Wayland display")?;
    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();

    let mut app = App {
        config,
        trace,
        started_at: Some(Instant::now()),
        running: true,
        ..Default::default()
    };

    display.get_registry(&qh, ());
    event_queue
        .roundtrip(&mut app)
        .context("collect Wayland globals")?;

    app.init_overlay(&qh)?;
    eprintln!(
        "touchdeck: overlay initialized; base mode captures fullscreen; double-tap bottom edge for passthrough"
    );

    while app.running {
        event_queue
            .dispatch_pending(&mut app)
            .context("dispatch pending Wayland events")?;

        let now_ms = app.now_ms();
        let size = app.surface_size();
        let config = app.config.clone();
        let effects = app.engine.process_timers(now_ms, &config, size);
        app.apply_effects_or_stop(&qh, effects);
        app.expire_mode_hint(&qh)
            .context("expire mode hint overlay")?;

        if !app.running {
            break;
        }

        event_queue.flush().context("flush Wayland requests")?;
        let timeout = app.poll_timeout();
        let wayland_fd = event_queue.as_fd().as_raw_fd();

        let Some(guard) = event_queue.prepare_read() else {
            continue;
        };

        event_queue.flush().context("flush Wayland requests")?;
        if poll_fd(wayland_fd, timeout).context("poll Wayland fd")? {
            guard.read().context("read Wayland events")?;
        }
    }

    Ok(())
}

impl App {
    fn init_overlay(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let compositor = self
            .compositor
            .as_ref()
            .ok_or_else(|| anyhow!("Wayland compositor global is unavailable"))?;
        let layer_shell = self
            .layer_shell
            .as_ref()
            .ok_or_else(|| anyhow!("zwlr_layer_shell_v1 global is unavailable"))?;
        self.touch
            .as_ref()
            .ok_or_else(|| anyhow!("wl_touch is unavailable on this Wayland seat"))?;

        let surface = compositor.create_surface(qh, ());
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Overlay,
            String::from(NAMESPACE),
            qh,
            (),
        );

        layer_surface.set_anchor(
            zwlr_layer_surface_v1::Anchor::Top
                | zwlr_layer_surface_v1::Anchor::Bottom
                | zwlr_layer_surface_v1::Anchor::Left
                | zwlr_layer_surface_v1::Anchor::Right,
        );
        layer_surface.set_size(0, 0);
        layer_surface.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);

        surface.commit();

        self.surface = Some(surface);
        self.layer_surface = Some(layer_surface);

        self.init_virtual_keyboard(qh)?;

        Ok(())
    }

    fn init_virtual_keyboard(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let Some(manager) = self.virtual_keyboard_manager.as_ref() else {
            eprintln!("touchdeck: zwp_virtual_keyboard_manager_v1 is unavailable; key actions will be ignored");
            return Ok(());
        };
        let seat = self
            .seat
            .as_ref()
            .ok_or_else(|| anyhow!("wl_seat is unavailable for virtual keyboard"))?;

        let keyboard = manager.create_virtual_keyboard(seat, qh, ());
        let keymap_bytes = load_xkb_keymap(&self.config)?;
        let mut file = tempfile().context("create virtual keyboard keymap file")?;
        file.write_all(&keymap_bytes)
            .context("write virtual keyboard keymap")?;
        file.flush().context("flush virtual keyboard keymap")?;
        keyboard.keymap(1, file.as_fd(), keymap_bytes.len() as u32);

        self.virtual_keyboard = Some(keyboard);
        self.virtual_keyboard_keymap = Some(file);
        eprintln!("touchdeck: virtual keyboard initialized");

        Ok(())
    }

    fn surface_size(&self) -> SurfaceSize {
        SurfaceSize {
            width: self.width.max(1),
            height: self.height.max(1),
        }
    }

    fn now_ms(&self) -> u64 {
        self.started_at
            .map(|started_at| started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0)
    }

    fn poll_timeout(&self) -> Option<Duration> {
        let mut deadline = self.engine.next_timer_deadline_ms();
        if let Some(hint) = self.mode_hint {
            deadline = Some(deadline.map_or(hint.until_ms, |deadline| deadline.min(hint.until_ms)));
        }

        deadline.map(|deadline_ms| {
            let now_ms = self.now_ms();
            Duration::from_millis(deadline_ms.saturating_sub(now_ms))
        })
    }

    fn expire_mode_hint(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let expired = self
            .mode_hint
            .is_some_and(|hint| self.now_ms() >= hint.until_ms);
        if !expired {
            return Ok(());
        }

        self.mode_hint = None;
        if self.width != 0 && self.height != 0 {
            self.attach_overlay_buffer(qh, self.width, self.height)?;
        }
        Ok(())
    }

    fn attach_overlay_buffer(
        &mut self,
        qh: &QueueHandle<Self>,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let shm = self
            .shm
            .as_ref()
            .ok_or_else(|| anyhow!("wl_shm global is unavailable"))?;
        let surface = self
            .surface
            .as_ref()
            .ok_or_else(|| anyhow!("overlay surface is not initialized"))?;

        let width = width.max(1);
        let height = height.max(1);
        let stride = width
            .checked_mul(4)
            .ok_or_else(|| anyhow!("invalid buffer stride"))?;
        let size = stride
            .checked_mul(height)
            .ok_or_else(|| anyhow!("invalid buffer size"))?;

        let file = tempfile().context("create shm backing file")?;
        file.set_len(u64::from(size))
            .context("resize shm backing file")?;

        let mut mmap = unsafe { MmapMut::map_mut(&file).context("map shm backing file")? };
        self.render_overlay(&mut mmap, width, height);

        let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            qh,
            (),
        );

        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width as i32, height as i32);
        surface.commit();

        self.buffers.retain(|backing| !backing.released);
        self.buffers.push_back(BufferBacking {
            _file: file,
            _mmap: mmap,
            _pool: pool,
            buffer,
            released: false,
        });

        Ok(())
    }

    fn render_overlay(&self, mmap: &mut [u8], width: u32, height: u32) {
        mmap.fill(0);

        let size = SurfaceSize { width, height };
        if self.config.debug_alpha != 0 && !self.config.debug_draw {
            fill_rect(
                mmap,
                width,
                height,
                RectPx {
                    x: 0,
                    y: 0,
                    w: width as i32,
                    h: height as i32,
                },
                [0x00, 0x80, 0xff, self.config.debug_alpha],
            );
            self.render_mode_hint(mmap, width, height);
            return;
        }

        if !self.config.debug_draw {
            if self.engine.mode == Mode::Text {
                self.render_text_keyboard(mmap, width, height, size);
            }
            self.render_mode_hint(mmap, width, height);
            return;
        }

        match self.engine.mode {
            Mode::Base => {}
            Mode::Text => fill_rect(
                mmap,
                width,
                height,
                RectPx {
                    x: 0,
                    y: 0,
                    w: width as i32,
                    h: height as i32,
                },
                [0x10, 0x90, 0x70, 0x30],
            ),
            Mode::Passthrough => {}
            Mode::NiriMomentary => fill_rect(
                mmap,
                width,
                height,
                RectPx {
                    x: 0,
                    y: 0,
                    w: width as i32,
                    h: height as i32,
                },
                [0x90, 0x40, 0x10, 0x30],
            ),
            Mode::NiriLocked => fill_rect(
                mmap,
                width,
                height,
                RectPx {
                    x: 0,
                    y: 0,
                    w: width as i32,
                    h: height as i32,
                },
                [0x10, 0x50, 0xe0, 0x38],
            ),
        }

        for slot in self.config.slots.slots() {
            let rect = slot.rect.to_px(size);
            let color = slot_debug_color(slot);
            if slot.capture || slot.role == SlotRole::Key {
                fill_rect(mmap, width, height, rect, color);
            } else {
                draw_rect_frame(mmap, width, height, rect, color);
            }

            let label = self
                .config
                .keymap
                .slot_label(self.engine.mode, &self.engine.layer_stack, &slot.id)
                .or_else(|| slot.label.clone());
            if let Some(label) = label {
                let mut label_mark = rect;
                label_mark.h = label_mark.h.min(8);
                if slot.capture || slot.role == SlotRole::Key {
                    fill_rect(mmap, width, height, label_mark, [0xff, 0xff, 0xff, 0x70]);
                }
                draw_label_in_rect(
                    mmap,
                    width,
                    height,
                    rect,
                    &label,
                    [0xff, 0xff, 0xff, 0xd0],
                );
            }
        }

        for binding in self
            .config
            .keymap
            .bindings
            .iter()
            .filter(|binding| binding.mode == self.engine.mode && self.engine.layer_stack.contains(&binding.layer))
        {
            let target = binding.trigger.target();
            let rect = binding.trigger.rect().to_px(size);
            let color = active_binding_debug_color(target);
            if target.capture {
                fill_rect(mmap, width, height, rect, color);
            } else {
                draw_rect_frame(mmap, width, height, rect, color);
            }
        }

        if let Some(candidate) = &self.engine.hold_candidate {
            if let Some(contact) = self.engine.active.get(&candidate.id) {
                draw_circle(
                    mmap,
                    width,
                    height,
                    contact.last_x.round() as i32,
                    contact.last_y.round() as i32,
                    42,
                    [0x00, 0xc0, 0xff, 0xb0],
                );
            }
        }

        for contact in self.engine.active.values() {
            draw_circle(
                mmap,
                width,
                height,
                contact.last_x.round() as i32,
                contact.last_y.round() as i32,
                24,
                [0xff, 0xff, 0xff, 0xd0],
            );
        }

        self.render_mode_hint(mmap, width, height);
    }

    fn render_mode_hint(&self, mmap: &mut [u8], width: u32, height: u32) {
        let Some(hint) = self.mode_hint else {
            return;
        };
        if self.now_ms() >= hint.until_ms {
            return;
        }

        let max_w = (width as i32 - 32).max(80);
        let toast_w = ((width as i32 * 52) / 100).clamp(80, max_w);
        let toast_h = ((height as i32 * 45) / 1000).clamp(40, 90).min(height as i32);
        let x = (width as i32 - toast_w) / 2;
        let y = ((height as i32 * 70) / 1000)
            .max(16)
            .min((height as i32 - toast_h).max(0));
        let rect = RectPx {
            x,
            y,
            w: toast_w,
            h: toast_h,
        };
        let color = mode_hint_color(hint.mode);

        fill_rect(mmap, width, height, rect, [0x08, 0x12, 0x18, 0xd8]);
        draw_rect_frame(mmap, width, height, rect, color);
        fill_rect(
            mmap,
            width,
            height,
            RectPx {
                x: rect.x,
                y: rect.y + rect.h - 6,
                w: rect.w,
                h: 6,
            },
            color,
        );
        draw_label_in_rect_limited(
            mmap,
            width,
            height,
            rect,
            mode_hint_label(hint.mode),
            [0xff, 0xff, 0xff, 0xf0],
            8,
        );
    }

    fn render_text_keyboard(&self, mmap: &mut [u8], width: u32, height: u32, size: SurfaceSize) {
        fill_rect(
            mmap,
            width,
            height,
            RectPx {
                x: 0,
                y: 0,
                w: width as i32,
                h: height as i32,
            },
            [0x08, 0x18, 0x14, 0x20],
        );

        for slot in self.config.slots.slots() {
            if slot.role != SlotRole::Key {
                continue;
            }

            let tap_label = self.config.keymap.slot_gesture_label(
                Mode::Text,
                &self.engine.layer_stack,
                &slot.id,
                SlotGestureKind::Tap,
            );
            let hold_label = self.config.keymap.slot_gesture_label(
                Mode::Text,
                &self.engine.layer_stack,
                &slot.id,
                SlotGestureKind::Hold,
            );
            let up_label = self.config.keymap.slot_gesture_label(
                Mode::Text,
                &self.engine.layer_stack,
                &slot.id,
                SlotGestureKind::SwipeUp,
            );
            let down_label = self.config.keymap.slot_gesture_label(
                Mode::Text,
                &self.engine.layer_stack,
                &slot.id,
                SlotGestureKind::SwipeDown,
            );
            let left_label = self.config.keymap.slot_gesture_label(
                Mode::Text,
                &self.engine.layer_stack,
                &slot.id,
                SlotGestureKind::SwipeLeft,
            );
            let right_label = self.config.keymap.slot_gesture_label(
                Mode::Text,
                &self.engine.layer_stack,
                &slot.id,
                SlotGestureKind::SwipeRight,
            );

            if tap_label.is_none()
                && hold_label.is_none()
                && up_label.is_none()
                && down_label.is_none()
                && left_label.is_none()
                && right_label.is_none()
            {
                continue;
            }

            let rect = slot.rect.to_px(size);
            fill_rect(mmap, width, height, rect, [0x12, 0x34, 0x2a, 0xa8]);
            draw_rect_frame(mmap, width, height, rect, [0x80, 0xff, 0xc8, 0xb0]);
            draw_keycap_labels(
                mmap,
                width,
                height,
                rect,
                tap_label.as_deref(),
                hold_label.as_deref(),
                up_label.as_deref(),
                down_label.as_deref(),
                left_label.as_deref(),
                right_label.as_deref(),
            );
        }
    }

    fn apply_input_region(&self, qh: &QueueHandle<Self>, policy: &CapturePolicy) -> Result<()> {
        let surface = self
            .surface
            .as_ref()
            .ok_or_else(|| anyhow!("overlay surface is not initialized"))?;
        let compositor = self
            .compositor
            .as_ref()
            .ok_or_else(|| anyhow!("Wayland compositor global is unavailable"))?;
        let size = self.surface_size();

        match policy {
            CapturePolicy::Fullscreen => {
                surface.set_input_region(None);
            }
            CapturePolicy::Zones(rects) => {
                let region = compositor.create_region(qh, ());
                for rect in rects {
                    let rect = rect.to_px(size);
                    if rect.w > 0 && rect.h > 0 {
                        region.add(rect.x, rect.y, rect.w, rect.h);
                    }
                }
                surface.set_input_region(Some(&region));
                region.destroy();
            }
            CapturePolicy::None => {
                let region = compositor.create_region(qh, ());
                surface.set_input_region(Some(&region));
                region.destroy();
            }
        }

        surface.commit();
        Ok(())
    }

    fn apply_effects_or_stop(&mut self, qh: &QueueHandle<Self>, effects: Vec<EngineEffect>) {
        for effect in effects {
            let result = match effect {
                EngineEffect::SetCapture(policy) => {
                    self.capture_policy = policy.clone();
                    self.present_mode_hint_if_changed();
                    self.apply_input_region(qh, &policy)
                }
                EngineEffect::Dispatch(action) => {
                    self.dispatch_action(action);
                    Ok(())
                }
                EngineEffect::Redraw => {
                    if self.width != 0 && self.height != 0 {
                        self.attach_overlay_buffer(qh, self.width, self.height)
                    } else {
                        Ok(())
                    }
                }
            };

            if let Err(err) = result {
                eprintln!("touchdeck: failed to apply effect: {err:?}");
                self.running = false;
                break;
            }
        }
    }

    fn present_mode_hint_if_changed(&mut self) {
        let mode = self.engine.mode;
        if self.last_presented_mode == mode {
            return;
        }

        self.last_presented_mode = mode;
        if self.config.mode_hint_ms == 0 {
            self.mode_hint = None;
            return;
        }

        self.mode_hint = Some(ModeHint {
            mode,
            until_ms: self.now_ms() + u64::from(self.config.mode_hint_ms),
        });
    }

    fn dispatch_action(&mut self, action: GestureAction) {
        match action {
            GestureAction::Niri(action) => {
                self.engine.last_action = Some(action.as_str().to_string());
                eprintln!("touchdeck: niri action {action}");
                spawn_niri_action(action);
            }
            GestureAction::KeySequence(sequence) => {
                self.send_key_sequence(&sequence);
            }
            GestureAction::KeyHold(key) => {
                self.send_key_state(key, true);
            }
            GestureAction::KeyRelease(key) => {
                self.send_key_state(key, false);
            }
            GestureAction::Sequence(steps) => {
                self.run_action_steps(&steps);
            }
            GestureAction::Exit => {
                eprintln!("touchdeck: exit gesture");
                self.running = false;
            }
            GestureAction::ModeSet(_)
            | GestureAction::ModeToggle(_)
            | GestureAction::ModeMomentary(_)
            | GestureAction::LayerSet(_)
            | GestureAction::LayerToggle(_)
            | GestureAction::LayerMomentary(_) => {}
            GestureAction::None => {}
        }
    }

    fn send_key(&mut self, key: u32) {
        let Some(keyboard) = self.virtual_keyboard.as_ref() else {
            eprintln!("touchdeck: virtual keyboard unavailable; ignored key {key}");
            return;
        };

        let time = self.now_ms().min(u64::from(u32::MAX)) as u32;
        eprintln!("touchdeck: key {key}");
        keyboard.key(time, key, 1);
        keyboard.key(time.saturating_add(1), key, 0);
    }

    fn send_key_sequence(&mut self, sequence: &[KeyChord]) {
        let Some(keyboard) = self.virtual_keyboard.as_ref() else {
            eprintln!("touchdeck: virtual keyboard unavailable; ignored key sequence {sequence:?}");
            return;
        };

        let mut time = self.now_ms().min(u64::from(u32::MAX)) as u32;
        eprintln!("touchdeck: key sequence {sequence:?}");
        for chord in sequence {
            for key in &chord.keys {
                keyboard.key(time, *key, 1);
                time = time.saturating_add(1);
            }
            for key in chord.keys.iter().rev() {
                keyboard.key(time, *key, 0);
                time = time.saturating_add(1);
            }
        }
    }

    fn run_action_steps(&mut self, steps: &[ActionStep]) {
        for step in steps {
            match step {
                ActionStep::KeyDown(key) => self.send_key_state(*key, true),
                ActionStep::KeyUp(key) => self.send_key_state(*key, false),
                ActionStep::TapKey(key) => self.send_key(*key),
                ActionStep::KeySequence(sequence) => self.send_key_sequence(sequence),
                ActionStep::Niri(action) => spawn_niri_action(action.clone()),
                ActionStep::DelayMs(ms) => std::thread::sleep(Duration::from_millis(u64::from(*ms))),
            }
        }
    }

    fn send_key_state(&mut self, key: u32, pressed: bool) {
        let Some(keyboard) = self.virtual_keyboard.as_ref() else {
            eprintln!("touchdeck: virtual keyboard unavailable; ignored key state {key}");
            return;
        };

        let time = self.now_ms().min(u64::from(u32::MAX)) as u32;
        keyboard.key(time, key, if pressed { 1 } else { 0 });
    }

    fn record_trace(&mut self, event: TraceEvent) {
        if let Some(trace) = self.trace.as_mut() {
            if let Err(err) = trace.record(&event) {
                eprintln!("touchdeck: failed to record trace: {err:?}");
                self.trace = None;
            }
        }
    }

    fn touch_down(&mut self, qh: &QueueHandle<Self>, id: i32, time: u32, x: f64, y: f64) {
        let now_ms = self.now_ms();
        self.record_trace(TraceEvent::Down {
            t: now_ms,
            wl_time: time,
            id,
            x,
            y,
        });
        if self.config.log_touch {
            eprintln!("touchdeck: touch down id={id} time={time} x={x:.1} y={y:.1}");
        }

        let config = self.config.clone();
        let size = self.surface_size();
        let effects = self.engine.handle_down(now_ms, time, id, x, y, &config, size);
        self.apply_effects_or_stop(qh, effects);
    }

    fn touch_motion(&mut self, qh: &QueueHandle<Self>, id: i32, time: u32, x: f64, y: f64) {
        let now_ms = self.now_ms();
        self.record_trace(TraceEvent::Motion {
            t: now_ms,
            wl_time: time,
            id,
            x,
            y,
        });
        if self.config.log_touch {
            eprintln!("touchdeck: touch motion id={id} time={time} x={x:.1} y={y:.1}");
        }

        let config = self.config.clone();
        let effects = self.engine.handle_motion(id, time, x, y, &config);
        self.apply_effects_or_stop(qh, effects);
    }

    fn touch_up(&mut self, qh: &QueueHandle<Self>, id: i32, time: u32) {
        let now_ms = self.now_ms();
        self.record_trace(TraceEvent::Up {
            t: now_ms,
            wl_time: time,
            id,
        });
        if self.config.log_touch {
            eprintln!("touchdeck: touch up id={id} time={time}");
        }

        let config = self.config.clone();
        let size = self.surface_size();
        let effects = self.engine.handle_up(now_ms, time, id, &config, size);
        self.apply_effects_or_stop(qh, effects);
    }

    fn touch_cancel(&mut self, qh: &QueueHandle<Self>) {
        let now_ms = self.now_ms();
        self.record_trace(TraceEvent::Cancel { t: now_ms });
        if self.config.log_touch {
            eprintln!("touchdeck: touch cancel");
        }

        let config = self.config.clone();
        let effects = self.engine.handle_cancel(&config);
        self.apply_effects_or_stop(qh, effects);
    }
}

impl Engine {
    fn capture_policy(&self, config: &Config) -> CapturePolicy {
        match self.mode {
            Mode::Passthrough => {
                CapturePolicy::Zones(config.keymap.capture_rects(self.mode, &self.layer_stack))
            }
            Mode::NiriMomentary | Mode::NiriLocked => CapturePolicy::Fullscreen,
            Mode::Base | Mode::Text => CapturePolicy::Fullscreen,
        }
    }

    fn next_timer_deadline_ms(&self) -> Option<u64> {
        self.hold_candidate
            .as_ref()
            .map(|candidate| candidate.deadline_ms)
    }

    fn process_timers(
        &mut self,
        now_ms: u64,
        config: &Config,
        _size: SurfaceSize,
    ) -> Vec<EngineEffect> {
        let Some(candidate) = self.hold_candidate.clone() else {
            return Vec::new();
        };

        if now_ms < candidate.deadline_ms {
            return Vec::new();
        }

        let Some(contact) = self.active.get_mut(&candidate.id) else {
            self.hold_candidate = None;
            return Vec::new();
        };

        if contact_movement(contact) > config.tap_radius {
            self.hold_candidate = None;
            return Vec::new();
        }

        contact.start_x = contact.last_x;
        contact.start_y = contact.last_y;
        contact.start_time = contact.last_time;
        self.finished.clear();
        self.max_active = 1;
        let action = candidate.action.clone();
        self.hold_candidate = None;
        self.last_tap = None;

        let mut effects = Vec::new();
        self.perform_action(action, &mut effects, config, Some(candidate.id));
        effects
    }

    fn handle_down(
        &mut self,
        now_ms: u64,
        time: u32,
        id: i32,
        x: f64,
        y: f64,
        config: &Config,
        size: SurfaceSize,
    ) -> Vec<EngineEffect> {
        self.active.insert(
            id,
            Contact {
                id,
                start_x: x,
                start_y: y,
                last_x: x,
                last_y: y,
                start_time: time,
                last_time: time,
            },
        );
        self.max_active = self.max_active.max(self.active.len());

        if let Some((action, min_ms)) =
            config
                .keymap
                .resolve_hold(self.mode, &self.layer_stack, size, x, y, config.hold_ms)
        {
            self.hold_candidate = Some(HoldCandidate {
                id,
                deadline_ms: now_ms + u64::from(min_ms),
                action,
            });
        }

        redraw_if_debug(config)
    }

    fn handle_motion(
        &mut self,
        id: i32,
        time: u32,
        x: f64,
        y: f64,
        config: &Config,
    ) -> Vec<EngineEffect> {
        if let Some(contact) = self.active.get_mut(&id) {
            contact.last_x = x;
            contact.last_y = y;
            contact.last_time = time;

            if let Some(candidate) = &self.hold_candidate {
                if candidate.id == id && contact_movement(contact) > config.tap_radius {
                    self.hold_candidate = None;
                }
            }
        }

        redraw_if_debug(config)
    }

    fn handle_up(
        &mut self,
        now_ms: u64,
        time: u32,
        id: i32,
        config: &Config,
        size: SurfaceSize,
    ) -> Vec<EngineEffect> {
        if let Some(candidate) = &self.hold_candidate {
            if candidate.id == id {
                self.hold_candidate = None;
            }
        }

        let Some(mut contact) = self.active.remove(&id) else {
            return Vec::new();
        };
        contact.last_time = time;

        let was_held_key = self.held_keys.iter().any(|held| held.hold_id == id);
        let mut held_key_effects = self.release_held_keys_for(id);
        if was_held_key {
            held_key_effects.extend(redraw_if_debug(config));
            self.max_active = self.active.len();
            return held_key_effects;
        }

        if self.mode != Mode::NiriMomentary
            && self
                .momentary
                .as_ref()
                .is_some_and(|momentary| momentary.hold_id == id)
        {
            let mut effects = std::mem::take(&mut held_key_effects);
            self.return_from_momentary(&mut effects, config);
            self.reset_contacts();
            return effects;
        }

        match self.mode {
            Mode::Base | Mode::Text => {
                self.finished.push(contact);
                if self.active_non_hold_count() == 0 {
                    let gesture = self.take_finished_non_hold_gesture();
                    let mut effects = std::mem::take(&mut held_key_effects);
                    effects.extend(self.resolve_base_gesture(now_ms, gesture, config, size));
                    effects
                } else {
                    held_key_effects.extend(redraw_if_debug(config));
                    held_key_effects
                }
            }
            Mode::Passthrough => {
                self.finished.push(contact);
                if self.active_non_hold_count() == 0 {
                    let gesture = self.take_finished_non_hold_gesture();
                    let mut effects = std::mem::take(&mut held_key_effects);
                    effects.extend(self.resolve_passthrough_gesture(now_ms, gesture, config, size));
                    effects
                } else {
                    held_key_effects.extend(redraw_if_debug(config));
                    held_key_effects
                }
            }
            Mode::NiriLocked => {
                self.finished.push(contact);
                if self.active_non_hold_count() == 0 {
                    let gesture = self.take_finished_non_hold_gesture();
                    let mut effects = std::mem::take(&mut held_key_effects);
                    effects.extend(self.resolve_locked_gesture(now_ms, gesture, config, size));
                    effects
                } else {
                    held_key_effects.extend(redraw_if_debug(config));
                    held_key_effects
                }
            }
            Mode::NiriMomentary => {
                if self
                    .momentary
                    .as_ref()
                    .is_some_and(|momentary| momentary.hold_id == id)
                {
                    let mut effects = std::mem::take(&mut held_key_effects);
                    let gesture = Gesture {
                        finished: vec![contact],
                        max_active: 1,
                    };
                    let action = self.resolve_configured_or_niri(&gesture, config, size, now_ms);
                    self.perform_action(action, &mut effects, config, None);
                    self.return_from_momentary(&mut effects, config);
                    self.reset_contacts();
                    effects
                } else {
                    self.finished.push(contact);
                    if self.active_non_hold_count() == 0 {
                        let gesture = self.take_finished_non_hold_gesture();
                        let mut effects = std::mem::take(&mut held_key_effects);
                        effects.extend(redraw_if_debug(config));
                        let action = self.resolve_configured_or_niri(&gesture, config, size, now_ms);
                        self.perform_action(action, &mut effects, config, None);
                        effects
                    } else {
                        held_key_effects.extend(redraw_if_debug(config));
                        held_key_effects
                    }
                }
            }
        }
    }

    fn handle_cancel(&mut self, config: &Config) -> Vec<EngineEffect> {
        let mut effects = self.release_all_held_keys();
        self.set_mode(Mode::Base, &mut effects, config);
        self.reset_contacts();
        effects.push(EngineEffect::Redraw);
        effects
    }

    #[cfg(test)]
    fn handle_trace_event(
        &mut self,
        event: TraceEvent,
        config: &Config,
        size: SurfaceSize,
    ) -> Vec<EngineEffect> {
        match event {
            TraceEvent::Down {
                t,
                wl_time,
                id,
                x,
                y,
            } => self.handle_down(t, wl_time, id, x, y, config, size),
            TraceEvent::Motion {
                wl_time,
                id,
                x,
                y,
                ..
            } => self.handle_motion(id, wl_time, x, y, config),
            TraceEvent::Up { t, wl_time, id } => self.handle_up(t, wl_time, id, config, size),
            TraceEvent::Cancel { .. } => self.handle_cancel(config),
        }
    }

    fn resolve_base_gesture(
        &mut self,
        now_ms: u64,
        gesture: Gesture,
        config: &Config,
        size: SurfaceSize,
    ) -> Vec<EngineEffect> {
        let mut effects = redraw_if_debug(config);

        if is_exit_gesture(&gesture, config, size) {
            push_dispatch_effect(&mut effects, GestureAction::Exit);
            return effects;
        }

        let action = self.resolve_configured_or_niri(&gesture, config, size, now_ms);
        self.perform_action(action, &mut effects, config, None);

        effects
    }

    fn resolve_passthrough_gesture(
        &mut self,
        now_ms: u64,
        gesture: Gesture,
        config: &Config,
        size: SurfaceSize,
    ) -> Vec<EngineEffect> {
        let mut effects = redraw_if_debug(config);

        if is_exit_gesture(&gesture, config, size) {
            push_dispatch_effect(&mut effects, GestureAction::Exit);
            return effects;
        }

        let action = self.resolve_configured_or_niri(&gesture, config, size, now_ms);
        self.perform_action(action, &mut effects, config, None);

        effects
    }

    fn resolve_locked_gesture(
        &mut self,
        now_ms: u64,
        gesture: Gesture,
        config: &Config,
        size: SurfaceSize,
    ) -> Vec<EngineEffect> {
        let mut effects = redraw_if_debug(config);

        if is_exit_gesture(&gesture, config, size) {
            push_dispatch_effect(&mut effects, GestureAction::Exit);
            return effects;
        }

        let action = self.resolve_configured_or_niri(&gesture, config, size, now_ms);
        self.perform_action(action, &mut effects, config, None);
        effects
    }

    fn resolve_configured_or_niri(
        &mut self,
        gesture: &Gesture,
        config: &Config,
        size: SurfaceSize,
        now_ms: u64,
    ) -> GestureAction {
        let action = config.keymap.resolve_release(
            self.mode,
            &self.layer_stack,
            gesture,
            config,
            size,
            &mut self.last_tap,
            now_ms,
        );
        if action != GestureAction::None {
            return action;
        }

        if matches!(self.mode, Mode::NiriMomentary | Mode::NiriLocked) {
            resolve_niri_gesture(gesture, config, size)
        } else {
            GestureAction::None
        }
    }

    fn perform_action(
        &mut self,
        action: GestureAction,
        effects: &mut Vec<EngineEffect>,
        config: &Config,
        hold_id: Option<i32>,
    ) {
        match action {
            GestureAction::Niri(_)
            | GestureAction::KeySequence(_)
            | GestureAction::KeyRelease(_)
            | GestureAction::Exit => {
                effects.push(EngineEffect::Dispatch(action));
            }
            GestureAction::KeyHold(key) => {
                if let Some(hold_id) = hold_id {
                    self.held_keys.push(HeldKeyState { hold_id, key });
                    effects.push(EngineEffect::Dispatch(GestureAction::KeyHold(key)));
                } else {
                    effects.push(EngineEffect::Dispatch(GestureAction::KeySequence(vec![KeyChord {
                        keys: vec![key],
                    }])));
                }
            }
            GestureAction::Sequence(_) => {
                effects.push(EngineEffect::Dispatch(action));
            }
            GestureAction::ModeSet(mode) => {
                self.set_mode(mode, effects, config);
            }
            GestureAction::ModeToggle(mode) => {
                if self.mode == mode {
                    self.set_mode(Mode::Base, effects, config);
                } else {
                    self.set_mode(mode, effects, config);
                }
            }
            GestureAction::ModeMomentary(mode) => {
                if let Some(hold_id) = hold_id {
                    self.start_momentary(hold_id, Some(mode), None, effects, config);
                } else {
                    self.set_mode(mode, effects, config);
                }
            }
            GestureAction::LayerSet(layer) => {
                self.set_layer(layer, effects);
            }
            GestureAction::LayerToggle(layer) => {
                if self.layer_stack.contains(&layer) {
                    self.pop_layer(layer, effects);
                } else {
                    self.push_layer(layer, effects);
                }
            }
            GestureAction::LayerMomentary(layer) => {
                if let Some(hold_id) = hold_id {
                    self.start_momentary(hold_id, None, Some(layer), effects, config);
                } else {
                    self.set_layer(layer, effects);
                }
            }
            GestureAction::None => {}
        }
    }

    fn start_momentary(
        &mut self,
        hold_id: i32,
        mode: Option<Mode>,
        layer: Option<Layer>,
        effects: &mut Vec<EngineEffect>,
        config: &Config,
    ) {
        self.momentary = Some(MomentaryState {
            hold_id,
            return_mode: self.mode,
            return_layer_stack: self.layer_stack.clone(),
        });

        if let Some(mode) = mode {
            self.mode = mode;
            self.layer_stack = default_layer_stack_for_mode(mode);
            eprintln!("touchdeck: mode {}", mode_name(mode));
        }

        if let Some(layer) = layer {
            self.push_layer(layer, effects);
            eprintln!("touchdeck: layer {}", layer_name(layer));
        }

        self.last_tap = None;
        effects.push(EngineEffect::SetCapture(self.capture_policy(config)));
        effects.push(EngineEffect::Redraw);
    }

    fn return_from_momentary(&mut self, effects: &mut Vec<EngineEffect>, config: &Config) {
        let Some(momentary) = self.momentary.take() else {
            return;
        };

        self.mode = momentary.return_mode;
        self.layer_stack = momentary.return_layer_stack;
        self.hold_candidate = None;
        self.last_tap = None;
        eprintln!(
            "touchdeck: return mode {} layer {}",
            mode_name(self.mode),
            layer_name(self.current_layer())
        );
        effects.push(EngineEffect::SetCapture(self.capture_policy(config)));
        effects.push(EngineEffect::Redraw);
    }

    fn set_mode(&mut self, mode: Mode, effects: &mut Vec<EngineEffect>, config: &Config) {
        self.mode = mode;
        self.layer_stack = default_layer_stack_for_mode(mode);
        self.momentary = None;
        self.hold_candidate = None;
        self.last_tap = None;
        eprintln!("touchdeck: mode {}", mode_name(mode));
        effects.push(EngineEffect::SetCapture(self.capture_policy(config)));
        effects.push(EngineEffect::Redraw);
    }

    fn set_layer(&mut self, layer: Layer, effects: &mut Vec<EngineEffect>) {
        self.layer_stack = if layer == Layer::Base {
            vec![Layer::Base]
        } else {
            vec![Layer::Base, layer]
        };
        self.momentary = None;
        self.hold_candidate = None;
        self.last_tap = None;
        eprintln!("touchdeck: layer {}", layer_name(layer));
        effects.push(EngineEffect::Redraw);
    }

    fn push_layer(&mut self, layer: Layer, effects: &mut Vec<EngineEffect>) {
        if layer == Layer::Base {
            self.set_layer(Layer::Base, effects);
            return;
        }

        self.layer_stack.retain(|existing| *existing != layer);
        if !self.layer_stack.contains(&Layer::Base) {
            self.layer_stack.insert(0, Layer::Base);
        }
        self.layer_stack.push(layer);
        self.last_tap = None;
        eprintln!("touchdeck: push layer {}", layer_name(layer));
        effects.push(EngineEffect::Redraw);
    }

    fn pop_layer(&mut self, layer: Layer, effects: &mut Vec<EngineEffect>) {
        if layer == Layer::Base {
            self.set_layer(Layer::Base, effects);
            return;
        }

        self.layer_stack.retain(|existing| *existing != layer);
        if self.layer_stack.is_empty() {
            self.layer_stack.push(Layer::Base);
        }
        self.last_tap = None;
        eprintln!("touchdeck: pop layer {}", layer_name(layer));
        effects.push(EngineEffect::Redraw);
    }

    fn current_layer(&self) -> Layer {
        self.layer_stack.last().copied().unwrap_or(Layer::Base)
    }
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

fn default_layer_stack_for_mode(mode: Mode) -> Vec<Layer> {
    match mode {
        Mode::NiriMomentary | Mode::NiriLocked => vec![Layer::Niri],
        Mode::Base | Mode::Text | Mode::Passthrough => vec![Layer::Base],
    }
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Base => "base",
        Mode::Text => "text",
        Mode::NiriMomentary => "niri-momentary",
        Mode::NiriLocked => "niri-locked",
        Mode::Passthrough => "passthrough",
    }
}

fn mode_hint_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Base => "BASE",
        Mode::Text => "TEXT",
        Mode::NiriMomentary => "NIRI",
        Mode::NiriLocked => "NIRI-LK",
        Mode::Passthrough => "PASS",
    }
}

fn mode_hint_color(mode: Mode) -> [u8; 4] {
    match mode {
        Mode::Base => [0xff, 0xff, 0xff, 0xb0],
        Mode::Text => [0x40, 0xff, 0xb0, 0xd0],
        Mode::NiriMomentary => [0x30, 0xa0, 0xff, 0xd0],
        Mode::NiriLocked => [0xff, 0x90, 0x30, 0xd8],
        Mode::Passthrough => [0xb0, 0xb0, 0xb0, 0xc0],
    }
}

fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Base => "base",
        Layer::Niri => "niri",
    }
}

impl Engine {
    fn hold_contact_ids(&self) -> Vec<i32> {
        let mut ids = self
            .held_keys
            .iter()
            .map(|held| held.hold_id)
            .collect::<Vec<_>>();
        if let Some(momentary) = &self.momentary {
            ids.push(momentary.hold_id);
        }
        ids
    }

    fn active_non_hold_count(&self) -> usize {
        let hold_ids = self.hold_contact_ids();
        self.active
            .keys()
            .filter(|id| !hold_ids.contains(*id))
            .count()
    }

    fn take_finished_non_hold_gesture(&mut self) -> Gesture {
        let hold_ids = self.hold_contact_ids();
        let mut finished = Vec::new();
        self.finished.retain(|contact| {
            if hold_ids.contains(&contact.id) {
                true
            } else {
                finished.push(contact.clone());
                false
            }
        });
        self.max_active = self.active.len();

        Gesture {
            max_active: finished.len().max(1),
            finished,
        }
    }

    fn release_held_keys_for(&mut self, hold_id: i32) -> Vec<EngineEffect> {
        let mut effects = Vec::new();
        let mut remaining = Vec::new();
        for held in self.held_keys.drain(..) {
            if held.hold_id == hold_id {
                effects.push(EngineEffect::Dispatch(GestureAction::KeyRelease(held.key)));
            } else {
                remaining.push(held);
            }
        }
        self.held_keys = remaining;
        effects
    }

    fn release_all_held_keys(&mut self) -> Vec<EngineEffect> {
        self.held_keys
            .drain(..)
            .map(|held| EngineEffect::Dispatch(GestureAction::KeyRelease(held.key)))
            .collect()
    }

    fn reset_contacts(&mut self) {
        self.active.clear();
        self.finished.clear();
        self.max_active = 0;
        self.hold_candidate = None;
    }
}

fn poll_fd(fd: RawFd, timeout: Option<Duration>) -> Result<bool> {
    let timeout_ms = timeout
        .map(|timeout| timeout.as_millis().min(i32::MAX as u128) as i32)
        .unwrap_or(-1);
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };

    loop {
        let rc = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
        if rc > 0 {
            return Ok(true);
        }
        if rc == 0 {
            return Ok(false);
        }

        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(err.into());
    }
}

fn spawn_niri_action(action: NiriAction) {
    thread::spawn(move || {
        if let Err(err) = send_niri_action_socket(action) {
            eprintln!("touchdeck: failed to send niri action {action}: {err:?}");
        }
    });
}

fn send_niri_action_socket(action: NiriAction) -> Result<()> {
    let request = niri_action_request_json(action);
    let socket_path = env::var_os("NIRI_SOCKET")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("NIRI_SOCKET is not set"))?;
    let mut stream = UnixStream::connect(&socket_path)
        .with_context(|| format!("connect niri IPC socket {}", socket_path.display()))?;

    stream
        .write_all(request.as_bytes())
        .context("write niri IPC request")?;
    stream.write_all(b"\n").context("write niri IPC newline")?;
    stream.flush().context("flush niri IPC request")?;

    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    let bytes = reader
        .read_line(&mut reply)
        .context("read niri IPC response")?;
    if bytes == 0 {
        return Err(anyhow!("empty niri IPC response"));
    }

    let reply = reply.trim();
    let value: serde_json::Value = serde_json::from_str(reply)
        .with_context(|| format!("parse niri IPC response {reply}"))?;
    if let Some(err) = value.get("Err") {
        return Err(anyhow!("niri IPC error: {err}"));
    }

    Ok(())
}

fn niri_action_request_json(action: NiriAction) -> &'static str {
    action.ipc_request_json()
}

#[cfg(test)]
fn named_target(name: &str) -> Option<SlotTarget> {
    SlotRegistry::default().get(name).ok()
}

fn left_bottom_rect() -> RectNorm {
    RectNorm {
        x0: 0.00,
        y0: 0.82,
        x1: 0.18,
        y1: 1.00,
    }
}

fn bottom_edge_rect() -> RectNorm {
    RectNorm {
        x0: 0.18,
        y0: 0.94,
        x1: 0.94,
        y1: 1.00,
    }
}

fn parse_mode(value: &str) -> Result<Mode> {
    match normalize_name(value).as_str() {
        "base" => Ok(Mode::Base),
        "text" | "keyboard" => Ok(Mode::Text),
        "niri_momentary" | "niri" => Ok(Mode::NiriMomentary),
        "niri_locked" => Ok(Mode::NiriLocked),
        "passthrough" => Ok(Mode::Passthrough),
        _ => Err(anyhow!("unknown mode {value}")),
    }
}

fn parse_layer(value: &str) -> Result<Layer> {
    match normalize_name(value).as_str() {
        "base" => Ok(Layer::Base),
        "niri" => Ok(Layer::Niri),
        _ => Err(anyhow!("unknown layer {value}")),
    }
}

fn validate_rect(rect: RectNorm, context: &str) -> Result<RectNorm> {
    if !(rect.x0.is_finite()
        && rect.x1.is_finite()
        && rect.y0.is_finite()
        && rect.y1.is_finite()
        && rect.x0 >= 0.0
        && rect.y0 >= 0.0
        && rect.x1 <= 1.0
        && rect.y1 <= 1.0
        && rect.x0 < rect.x1
        && rect.y0 < rect.y1)
    {
        return Err(anyhow!(
            "{context} coordinates must be finite normalized ranges with x0 < x1 and y0 < y1"
        ));
    }

    Ok(rect)
}

fn resolve_config_relative(config_path: &Path, value: &str) -> PathBuf {
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

fn parse_behavior(value: BehaviorFileConfig, macros: &MacroRegistry) -> Result<Behavior> {
    match normalize_name(&value.kind).as_str() {
        "key" | "key_sequence" | "keys" => {
            let keys = value
                .key
                .or(value.keys)
                .ok_or_else(|| anyhow!("key behavior is missing key/keys"))?;
            Ok(Behavior::KeySequence(parse_emacs_key_sequence(&keys)?))
        }
        "key_hold" | "hold_key" | "modifier" => {
            let key = value
                .key
                .or(value.keys)
                .ok_or_else(|| anyhow!("key_hold behavior is missing key/keys"))?;
            Ok(Behavior::KeyHold(parse_single_emacs_key(&key)?))
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
        "niri" => Ok(Behavior::Niri(
            parse_niri_action(
                value
                    .action
                    .as_deref()
                    .ok_or_else(|| anyhow!("niri behavior is missing action"))?,
            )?,
        )),
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

fn parse_action_steps(steps: Vec<ActionStepFileConfig>) -> Result<Vec<ActionStep>> {
    steps.into_iter().map(parse_action_step).collect()
}

fn parse_action_step(value: ActionStepFileConfig) -> Result<ActionStep> {
    match normalize_name(&value.kind).as_str() {
        "key_down" => Ok(ActionStep::KeyDown(parse_single_emacs_key(
            value
                .key
                .as_deref()
                .ok_or_else(|| anyhow!("key_down step is missing key"))?,
        )?)),
        "key_up" => Ok(ActionStep::KeyUp(parse_single_emacs_key(
            value
                .key
                .as_deref()
                .ok_or_else(|| anyhow!("key_up step is missing key"))?,
        )?)),
        "tap_key" => Ok(ActionStep::TapKey(parse_single_emacs_key(
            value
                .key
                .as_deref()
                .ok_or_else(|| anyhow!("tap_key step is missing key"))?,
        )?)),
        "key_sequence" | "keys" => Ok(ActionStep::KeySequence(parse_emacs_key_sequence(
            value
                .keys
                .or(value.key)
                .as_deref()
                .ok_or_else(|| anyhow!("key_sequence step is missing key/keys"))?,
        )?)),
        "niri" => Ok(ActionStep::Niri(
            parse_niri_action(
                value
                    .action
                    .as_deref()
                    .ok_or_else(|| anyhow!("niri step is missing action"))?,
            )?,
        )),
        "delay" | "delay_ms" => Ok(ActionStep::DelayMs(
            value
                .ms
                .ok_or_else(|| anyhow!("delay step is missing ms"))?,
        )),
        other => Err(anyhow!("unknown action step type {other}")),
    }
}

fn parse_single_emacs_key(value: &str) -> Result<u32> {
    let sequence = parse_emacs_key_sequence(value)?;
    if sequence.len() != 1 || sequence[0].keys.len() != 1 {
        return Err(anyhow!("expected a single key, got {value}"));
    }
    Ok(sequence[0].keys[0])
}

fn parse_emacs_key_sequence(value: &str) -> Result<Vec<KeyChord>> {
    let sequence = value
        .split_whitespace()
        .map(parse_emacs_key_chord)
        .collect::<Result<Vec<_>>>()?;

    if sequence.is_empty() {
        Err(anyhow!("empty key sequence"))
    } else {
        Ok(sequence)
    }
}

fn parse_emacs_key_chord(token: &str) -> Result<KeyChord> {
    let mut rest = token;
    let mut last_modifier_index = None;
    let mut keys = Vec::new();

    loop {
        let Some((index, modifier_key, prefix_len)) = parse_emacs_modifier_prefix(rest) else {
            break;
        };

        if last_modifier_index.is_some_and(|last| index < last) {
            return Err(anyhow!(
                "invalid Emacs modifier order in {token}; expected A-C-H-M-S-s"
            ));
        }

        if !keys.contains(&modifier_key) {
            keys.push(modifier_key);
        }
        last_modifier_index = Some(index);
        rest = &rest[prefix_len..];
    }

    let (base_key, implicit_modifiers) = parse_emacs_base_key(rest)
        .ok_or_else(|| anyhow!("unknown Emacs key token {token}"))?;
    for modifier in implicit_modifiers {
        if !keys.contains(&modifier) {
            keys.push(modifier);
        }
    }
    keys.push(base_key);

    Ok(KeyChord { keys })
}

fn parse_emacs_modifier_prefix(value: &str) -> Option<(usize, u32, usize)> {
    let prefixes = [
        ("A-", KEY_LEFTALT),
        ("C-", KEY_LEFTCTRL),
        ("H-", KEY_LEFTMETA),
        ("M-", KEY_LEFTALT),
        ("S-", KEY_LEFTSHIFT),
        ("s-", KEY_LEFTMETA),
    ];

    prefixes
        .iter()
        .enumerate()
        .find_map(|(index, (prefix, key))| value.starts_with(prefix).then_some((index, *key, prefix.len())))
}

fn parse_emacs_base_key(value: &str) -> Option<(u32, Vec<u32>)> {
    if value.is_empty() {
        return None;
    }

    let key_name = if value.starts_with('<') && value.ends_with('>') && value.len() > 2 {
        &value[1..value.len() - 1]
    } else {
        value
    };

    if key_name.len() == 1 {
        let ch = key_name.chars().next()?;
        if ch.is_ascii_uppercase() {
            return parse_key_name(&ch.to_ascii_lowercase().to_string())
                .map(|key| (key, vec![KEY_LEFTSHIFT]));
        }
    }

    parse_key_name(key_name)
        .map(|key| (key, Vec::new()))
        .or_else(|| parse_shifted_symbol_key(key_name).map(|key| (key, vec![KEY_LEFTSHIFT])))
}

fn parse_key_name(value: &str) -> Option<u32> {
    match value.trim() {
        "-" => return Some(KEY_MINUS),
        "=" => return Some(KEY_EQUAL),
        "[" => return Some(KEY_LEFTBRACE),
        "]" => return Some(KEY_RIGHTBRACE),
        ";" => return Some(KEY_SEMICOLON),
        "'" => return Some(KEY_APOSTROPHE),
        "`" => return Some(KEY_GRAVE),
        "\\" => return Some(KEY_BACKSLASH),
        "," => return Some(KEY_COMMA),
        "." => return Some(KEY_DOT),
        "/" => return Some(KEY_SLASH),
        _ => {}
    }

    match normalize_name(value).as_str() {
        "ctrl" | "control" | "leftctrl" | "left_control" => Some(KEY_LEFTCTRL),
        "shift" | "leftshift" | "left_shift" => Some(KEY_LEFTSHIFT),
        "alt" | "leftalt" | "left_alt" => Some(KEY_LEFTALT),
        "super" | "meta" | "win" | "leftmeta" | "left_meta" => Some(KEY_LEFTMETA),
        "esc" | "escape" => Some(KEY_ESC),
        "ret" | "return" => Some(KEY_ENTER),
        "1" => Some(KEY_1),
        "2" => Some(KEY_2),
        "3" => Some(KEY_3),
        "4" => Some(KEY_4),
        "5" => Some(KEY_5),
        "6" => Some(KEY_6),
        "7" => Some(KEY_7),
        "8" => Some(KEY_8),
        "9" => Some(KEY_9),
        "0" => Some(KEY_0),
        "-" | "minus" => Some(KEY_MINUS),
        "=" | "equal" | "equals" => Some(KEY_EQUAL),
        "[" | "leftbrace" | "left_brace" | "leftbracket" | "left_bracket" => Some(KEY_LEFTBRACE),
        "]" | "rightbrace" | "right_brace" | "rightbracket" | "right_bracket" => Some(KEY_RIGHTBRACE),
        ";" | "semicolon" | "semi" => Some(KEY_SEMICOLON),
        "'" | "apostrophe" | "quote" => Some(KEY_APOSTROPHE),
        "`" | "grave" | "backtick" => Some(KEY_GRAVE),
        "\\" | "backslash" => Some(KEY_BACKSLASH),
        "," | "comma" => Some(KEY_COMMA),
        "." | "dot" | "period" => Some(KEY_DOT),
        "/" | "slash" => Some(KEY_SLASH),
        "backspace" | "bs" => Some(KEY_BACKSPACE),
        "del" => Some(KEY_BACKSPACE),
        "delete" => Some(KEY_DELETE),
        "tab" => Some(KEY_TAB),
        "enter" => Some(KEY_ENTER),
        "spc" | "space" => Some(KEY_SPACE),
        "left" | "arrow_left" => Some(KEY_LEFT),
        "right" | "arrow_right" => Some(KEY_RIGHT),
        "up" | "arrow_up" => Some(KEY_UP),
        "down" | "arrow_down" => Some(KEY_DOWN),
        "a" => Some(KEY_A),
        "b" => Some(KEY_B),
        "c" => Some(KEY_C),
        "d" => Some(KEY_D),
        "e" => Some(KEY_E),
        "f" => Some(KEY_F),
        "g" => Some(KEY_G),
        "h" => Some(KEY_H),
        "i" => Some(KEY_I),
        "j" => Some(KEY_J),
        "k" => Some(KEY_K),
        "l" => Some(KEY_L),
        "m" => Some(KEY_M),
        "n" => Some(KEY_N),
        "o" => Some(KEY_O),
        "p" => Some(KEY_P),
        "q" => Some(KEY_Q),
        "r" => Some(KEY_R),
        "s" => Some(KEY_S),
        "t" => Some(KEY_T),
        "u" => Some(KEY_U),
        "v" => Some(KEY_V),
        "w" => Some(KEY_W),
        "x" => Some(KEY_X),
        "y" => Some(KEY_Y),
        "z" => Some(KEY_Z),
        _ => None,
    }
}

fn parse_shifted_symbol_key(value: &str) -> Option<u32> {
    match value {
        "!" => Some(KEY_1),
        "@" => Some(KEY_2),
        "#" => Some(KEY_3),
        "$" => Some(KEY_4),
        "%" => Some(KEY_5),
        "^" => Some(KEY_6),
        "&" => Some(KEY_7),
        "*" => Some(KEY_8),
        "(" => Some(KEY_9),
        ")" => Some(KEY_0),
        "_" => Some(KEY_MINUS),
        "+" => Some(KEY_EQUAL),
        "{" => Some(KEY_LEFTBRACE),
        "}" => Some(KEY_RIGHTBRACE),
        ":" => Some(KEY_SEMICOLON),
        "\"" => Some(KEY_APOSTROPHE),
        "~" => Some(KEY_GRAVE),
        "|" => Some(KEY_BACKSLASH),
        "<" => Some(KEY_COMMA),
        ">" => Some(KEY_DOT),
        "?" => Some(KEY_SLASH),
        _ => None,
    }
}

fn behavior_label(behavior: &Behavior) -> Option<String> {
    match behavior {
        Behavior::Niri(action) => Some(action.as_str().to_string()),
        Behavior::KeySequence(sequence) => key_sequence_label(sequence),
        Behavior::KeyHold(key) => key_code_label(*key).map(|label| format!("{}+", label)),
        Behavior::Sequence(_) => Some("macro".to_string()),
        Behavior::ModeSet(mode) => Some(mode_name(*mode).to_string()),
        Behavior::ModeToggle(mode) => Some(format!("{}*", mode_name(*mode))),
        Behavior::ModeMomentary(mode) => Some(format!("{}+", mode_name(*mode))),
        Behavior::LayerSet(layer) => Some(layer_name(*layer).to_string()),
        Behavior::LayerToggle(layer) => Some(format!("{}*", layer_name(*layer))),
        Behavior::LayerMomentary(layer) => Some(format!("{}+", layer_name(*layer))),
        Behavior::Exit => Some("exit".to_string()),
        Behavior::Transparent | Behavior::NoOp => None,
    }
}

fn key_sequence_label(sequence: &[KeyChord]) -> Option<String> {
    let labels = sequence
        .iter()
        .map(key_chord_label)
        .collect::<Option<Vec<_>>>()?;
    Some(labels.join(" "))
}

fn key_chord_label(chord: &KeyChord) -> Option<String> {
    let base = *chord.keys.last()?;
    let mut modifiers = chord.keys[..chord.keys.len().saturating_sub(1)].to_vec();

    let base_label = if remove_modifier(&mut modifiers, KEY_LEFTSHIFT) {
        shifted_key_label(base)
            .or_else(|| uppercase_letter_label(base))
            .map(str::to_string)
            .unwrap_or_else(|| {
                modifiers.push(KEY_LEFTSHIFT);
                key_code_label(base).unwrap_or("?").to_string()
            })
    } else {
        key_code_label(base)?.to_string()
    };

    let mut label = String::new();
    if remove_modifier(&mut modifiers, KEY_LEFTCTRL) {
        label.push_str("C-");
    }
    if remove_modifier(&mut modifiers, KEY_LEFTALT) {
        label.push_str("M-");
    }
    if remove_modifier(&mut modifiers, KEY_LEFTMETA) {
        label.push_str("s-");
    }
    if remove_modifier(&mut modifiers, KEY_LEFTSHIFT) {
        label.push_str("S-");
    }
    label.push_str(&base_label);
    Some(label)
}

fn remove_modifier(modifiers: &mut Vec<u32>, key: u32) -> bool {
    if let Some(index) = modifiers.iter().position(|modifier| *modifier == key) {
        modifiers.remove(index);
        true
    } else {
        false
    }
}

fn shifted_key_label(key: u32) -> Option<&'static str> {
    match key {
        KEY_1 => Some("!"),
        KEY_2 => Some("@"),
        KEY_3 => Some("#"),
        KEY_4 => Some("$"),
        KEY_5 => Some("%"),
        KEY_6 => Some("^"),
        KEY_7 => Some("&"),
        KEY_8 => Some("*"),
        KEY_9 => Some("("),
        KEY_0 => Some(")"),
        KEY_MINUS => Some("_"),
        KEY_EQUAL => Some("+"),
        KEY_LEFTBRACE => Some("{"),
        KEY_RIGHTBRACE => Some("}"),
        KEY_SEMICOLON => Some(":"),
        KEY_APOSTROPHE => Some("\""),
        KEY_GRAVE => Some("~"),
        KEY_BACKSLASH => Some("|"),
        KEY_COMMA => Some("<"),
        KEY_DOT => Some(">"),
        KEY_SLASH => Some("?"),
        _ => None,
    }
}

fn uppercase_letter_label(key: u32) -> Option<&'static str> {
    match key {
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

fn key_code_label(key: u32) -> Option<&'static str> {
    match key {
        KEY_LEFTCTRL => Some("CTRL"),
        KEY_LEFTSHIFT => Some("SHIFT"),
        KEY_LEFTALT => Some("ALT"),
        KEY_LEFTMETA => Some("SUPER"),
        KEY_ESC => Some("ESC"),
        KEY_ENTER => Some("RET"),
        KEY_BACKSPACE => Some("DEL"),
        KEY_DELETE => Some("DELETE"),
        KEY_TAB => Some("TAB"),
        KEY_SPACE => Some("SPC"),
        KEY_LEFT => Some("LEFT"),
        KEY_RIGHT => Some("RIGHT"),
        KEY_UP => Some("UP"),
        KEY_DOWN => Some("DOWN"),
        KEY_1 => Some("1"),
        KEY_2 => Some("2"),
        KEY_3 => Some("3"),
        KEY_4 => Some("4"),
        KEY_5 => Some("5"),
        KEY_6 => Some("6"),
        KEY_7 => Some("7"),
        KEY_8 => Some("8"),
        KEY_9 => Some("9"),
        KEY_0 => Some("0"),
        KEY_MINUS => Some("-"),
        KEY_EQUAL => Some("="),
        KEY_LEFTBRACE => Some("["),
        KEY_RIGHTBRACE => Some("]"),
        KEY_SEMICOLON => Some(";"),
        KEY_APOSTROPHE => Some("'"),
        KEY_GRAVE => Some("`"),
        KEY_BACKSLASH => Some("\\"),
        KEY_COMMA => Some(","),
        KEY_DOT => Some("."),
        KEY_SLASH => Some("/"),
        KEY_A => Some("a"),
        KEY_B => Some("b"),
        KEY_C => Some("c"),
        KEY_D => Some("d"),
        KEY_E => Some("e"),
        KEY_F => Some("f"),
        KEY_G => Some("g"),
        KEY_H => Some("h"),
        KEY_I => Some("i"),
        KEY_J => Some("j"),
        KEY_K => Some("k"),
        KEY_L => Some("l"),
        KEY_M => Some("m"),
        KEY_N => Some("n"),
        KEY_O => Some("o"),
        KEY_P => Some("p"),
        KEY_Q => Some("q"),
        KEY_R => Some("r"),
        KEY_S => Some("s"),
        KEY_T => Some("t"),
        KEY_U => Some("u"),
        KEY_V => Some("v"),
        KEY_W => Some("w"),
        KEY_X => Some("x"),
        KEY_Y => Some("y"),
        KEY_Z => Some("z"),
        _ => None,
    }
}

fn normalize_name(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

fn parse_niri_action(value: &str) -> Result<NiriAction> {
    match normalize_name(value).as_str() {
        "focus_column_left" => Ok(NiriAction::FocusColumnLeft),
        "focus_column_right" => Ok(NiriAction::FocusColumnRight),
        "focus_workspace_up" => Ok(NiriAction::FocusWorkspaceUp),
        "focus_workspace_down" => Ok(NiriAction::FocusWorkspaceDown),
        "toggle_overview" => Ok(NiriAction::ToggleOverview),
        other => Err(anyhow!(
            "unsupported niri action {other}; supported actions: focus-column-left, focus-column-right, focus-workspace-up, focus-workspace-down, toggle-overview"
        )),
    }
}

fn top_left_rect() -> RectNorm {
    RectNorm {
        x0: 0.00,
        y0: 0.00,
        x1: 0.12,
        y1: 0.10,
    }
}

fn redraw_if_debug(config: &Config) -> Vec<EngineEffect> {
    if config.debug_draw {
        vec![EngineEffect::Redraw]
    } else {
        Vec::new()
    }
}

fn push_dispatch_effect(effects: &mut Vec<EngineEffect>, action: GestureAction) {
    if action != GestureAction::None {
        effects.push(EngineEffect::Dispatch(action));
    }
}

fn recognize_gesture_kind(
    gesture: &Gesture,
    config: &Config,
    size: SurfaceSize,
) -> Option<GestureKind> {
    if gesture.max_active != 1 || gesture.finished.len() != 1 {
        return None;
    }

    if is_tap_like(gesture, config.tap_radius, config.two_finger_tap_ms) {
        return Some(GestureKind::Tap);
    }

    let contact = &gesture.finished[0];
    let min_dim = f64::from(size.width.min(size.height).max(1));
    let swipe_threshold_min = config.swipe_threshold_min.min(config.swipe_threshold_max);
    let swipe_threshold_max = config.swipe_threshold_min.max(config.swipe_threshold_max);
    let swipe_threshold = (min_dim * config.swipe_threshold_ratio)
        .clamp(swipe_threshold_min, swipe_threshold_max);
    let dx = contact.last_x - contact.start_x;
    let dy = contact.last_y - contact.start_y;
    let abs_dx = dx.abs();
    let abs_dy = dy.abs();

    if abs_dx.max(abs_dy) < swipe_threshold {
        return None;
    }

    if abs_dx >= abs_dy * 1.25 {
        if dx < 0.0 {
            Some(GestureKind::SwipeLeft)
        } else {
            Some(GestureKind::SwipeRight)
        }
    } else if abs_dy >= abs_dx * 1.25 {
        if dy < 0.0 {
            Some(GestureKind::SwipeUp)
        } else {
            Some(GestureKind::SwipeDown)
        }
    } else {
        None
    }
}

fn resolve_niri_gesture(gesture: &Gesture, config: &Config, size: SurfaceSize) -> GestureAction {
    if gesture.finished.is_empty() {
        return GestureAction::None;
    }

    let min_dim = f64::from(size.width.min(size.height).max(1));
    let swipe_threshold_min = config.swipe_threshold_min.min(config.swipe_threshold_max);
    let swipe_threshold_max = config.swipe_threshold_min.max(config.swipe_threshold_max);
    let swipe_threshold = (min_dim * config.swipe_threshold_ratio)
        .clamp(swipe_threshold_min, swipe_threshold_max);

    if gesture.max_active == 2 && is_tap_like(gesture, config.tap_radius, config.two_finger_tap_ms) {
        return niri_action(config.action_two_finger_tap);
    }

    if gesture.max_active != 1 || gesture.finished.len() != 1 {
        return GestureAction::None;
    }

    let contact = &gesture.finished[0];
    let dx = contact.last_x - contact.start_x;
    let dy = contact.last_y - contact.start_y;
    let abs_dx = dx.abs();
    let abs_dy = dy.abs();

    if abs_dx.max(abs_dy) < swipe_threshold {
        return GestureAction::None;
    }

    if abs_dx >= abs_dy * 1.25 {
        if dx < 0.0 {
            niri_action(config.action_swipe_left)
        } else {
            niri_action(config.action_swipe_right)
        }
    } else if abs_dy >= abs_dx * 1.25 {
        if dy < 0.0 {
            niri_action(config.action_swipe_up)
        } else {
            niri_action(config.action_swipe_down)
        }
    } else {
        GestureAction::None
    }
}

fn is_exit_gesture(gesture: &Gesture, config: &Config, size: SurfaceSize) -> bool {
    if gesture.finished.is_empty() {
        return false;
    }

    if gesture.max_active >= 3 && is_tap_like(gesture, config.tap_radius, config.exit_tap_ms) {
        return true;
    }

    config.exit_corner_enabled
        && gesture.max_active == 1
        && gesture.finished.len() == 1
        && is_tap_like(gesture, config.tap_radius, config.exit_corner_tap_ms)
        && is_top_left_corner(&gesture.finished[0], config, size)
}

fn niri_action(action: Option<NiriAction>) -> GestureAction {
    action.map(GestureAction::Niri).unwrap_or(GestureAction::None)
}

fn is_tap_like(gesture: &Gesture, radius: f64, max_ms: u32) -> bool {
    let start = gesture
        .finished
        .iter()
        .map(|contact| contact.start_time)
        .min()
        .unwrap_or(0);
    let end = gesture
        .finished
        .iter()
        .map(|contact| contact.last_time)
        .max()
        .unwrap_or(start);

    if end.saturating_sub(start) > max_ms {
        return false;
    }

    gesture.finished.iter().all(|contact| contact_movement(contact) <= radius)
}

fn contact_movement(contact: &Contact) -> f64 {
    let dx = contact.last_x - contact.start_x;
    let dy = contact.last_y - contact.start_y;
    dx.hypot(dy)
}

fn is_top_left_corner(contact: &Contact, config: &Config, size: SurfaceSize) -> bool {
    let ratio = config.exit_corner_ratio.clamp(0.01, 0.50);
    let rect = RectNorm {
        x0: 0.0,
        y0: 0.0,
        x1: ratio,
        y1: ratio,
    };
    rect.contains_px(size, contact.start_x, contact.start_y)
}

fn slot_debug_color(slot: &Slot) -> [u8; 4] {
    match (slot.capture, slot.role) {
        (true, SlotRole::Key) => [0x10, 0xff, 0xb0, 0x38],
        (true, SlotRole::Zone) => [0x20, 0xff, 0x80, 0x50],
        (true, SlotRole::GestureArea) => [0xff, 0x90, 0x20, 0x44],
        (false, SlotRole::Key) => [0x80, 0x80, 0x80, 0x24],
        (false, SlotRole::Zone) => [0x80, 0x80, 0x80, 0x20],
        (false, SlotRole::GestureArea) => [0x60, 0x60, 0x60, 0x18],
    }
}

fn active_binding_debug_color(target: &SlotTarget) -> [u8; 4] {
    match (target.capture, target.role) {
        (true, SlotRole::Key) => [0x10, 0xff, 0xb0, 0x70],
        (true, SlotRole::Zone) => [0xe0, 0xff, 0x50, 0x70],
        (true, SlotRole::GestureArea) => [0xff, 0xb0, 0x20, 0x70],
        (false, _) => [0xff, 0xff, 0xff, 0x36],
    }
}

fn draw_rect_frame(buf: &mut [u8], width: u32, height: u32, rect: RectPx, color: [u8; 4]) {
    let thickness = 2.max((rect.w.min(rect.h) / 36).max(1));
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: thickness,
        },
        color,
    );
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x,
            y: rect.y + rect.h - thickness,
            w: rect.w,
            h: thickness,
        },
        color,
    );
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x,
            y: rect.y,
            w: thickness,
            h: rect.h,
        },
        color,
    );
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x + rect.w - thickness,
            y: rect.y,
            w: thickness,
            h: rect.h,
        },
        color,
    );
}

fn draw_keycap_labels(
    buf: &mut [u8],
    width: u32,
    height: u32,
    rect: RectPx,
    tap: Option<&str>,
    hold: Option<&str>,
    up: Option<&str>,
    down: Option<&str>,
    left: Option<&str>,
    right: Option<&str>,
) {
    let hint_color = [0xa8, 0xff, 0xd8, 0xc8];
    let hold_color = [0xff, 0xe0, 0x90, 0xc8];
    let center_color = [0xff, 0xff, 0xff, 0xf0];
    let margin = (rect.w.min(rect.h) / 12).clamp(2, 10);
    let hint_h = (rect.h / 4).max(10);
    let side_w = (rect.w / 3).max(12);

    if let Some(label) = up {
        draw_label_in_rect_limited(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + margin,
                y: rect.y + margin,
                w: rect.w - margin * 2,
                h: hint_h,
            },
            label,
            hint_color,
            3,
        );
    }

    if let Some(label) = down {
        draw_label_in_rect_limited(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + margin,
                y: rect.y + rect.h - hint_h - margin,
                w: rect.w - margin * 2,
                h: hint_h,
            },
            label,
            hint_color,
            3,
        );
    }

    if let Some(label) = left {
        draw_label_in_rect_limited(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + margin,
                y: rect.y + rect.h / 3,
                w: side_w,
                h: rect.h / 3,
            },
            label,
            hint_color,
            3,
        );
    }

    if let Some(label) = right {
        draw_label_in_rect_limited(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + rect.w - side_w - margin,
                y: rect.y + rect.h / 3,
                w: side_w,
                h: rect.h / 3,
            },
            label,
            hint_color,
            3,
        );
    }

    if tap.is_some() {
        if let Some(label) = hold {
            draw_label_in_rect_limited(
                buf,
                width,
                height,
                RectPx {
                    x: rect.x + margin,
                    y: rect.y + rect.h - hint_h - margin,
                    w: side_w,
                    h: hint_h,
                },
                label,
                hold_color,
                2,
            );
        }
    }

    let center_label = tap.or(hold);
    let center_color = if tap.is_some() {
        center_color
    } else {
        hold_color
    };

    if let Some(label) = center_label {
        draw_label_in_rect_limited(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + rect.w / 4,
                y: rect.y + rect.h / 3,
                w: rect.w / 2,
                h: rect.h / 3,
            },
            label,
            center_color,
            8,
        );
    }
}

fn draw_label_in_rect(
    buf: &mut [u8],
    width: u32,
    height: u32,
    rect: RectPx,
    label: &str,
    color: [u8; 4],
) {
    draw_label_in_rect_limited(buf, width, height, rect, label, color, 8);
}

fn draw_label_in_rect_limited(
    buf: &mut [u8],
    width: u32,
    height: u32,
    rect: RectPx,
    label: &str,
    color: [u8; 4],
    max_scale: i32,
) {
    let text = label
        .chars()
        .filter(|ch| ch.is_ascii_graphic() || *ch == ' ')
        .take(8)
        .map(|ch| ch.to_ascii_uppercase())
        .collect::<Vec<_>>();
    if text.is_empty() {
        return;
    }

    let glyph_w = 3;
    let glyph_h = 5;
    let spacing = 1;
    let total_units = text.len() as i32 * glyph_w + (text.len() as i32 - 1) * spacing;
    let scale_x = (rect.w / (total_units + 2)).max(1);
    let scale_y = (rect.h / (glyph_h + 2)).max(1);
    let scale = scale_x.min(scale_y).clamp(1, max_scale.max(1));
    let total_w = total_units * scale;
    let total_h = glyph_h * scale;
    let mut x = rect.x + (rect.w - total_w) / 2;
    let y = rect.y + (rect.h - total_h) / 2;

    for ch in text {
        draw_glyph(buf, width, height, x, y, scale, ch, color);
        x += (glyph_w + spacing) * scale;
    }
}

fn draw_glyph(
    buf: &mut [u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    scale: i32,
    ch: char,
    color: [u8; 4],
) {
    let pattern = glyph_3x5(ch);
    for (row, bits) in pattern.iter().enumerate() {
        for col in 0usize..3 {
            if *bits & (1u8 << (2 - col)) == 0 {
                continue;
            }

            fill_rect(
                buf,
                width,
                height,
                RectPx {
                    x: x + col as i32 * scale,
                    y: y + row as i32 * scale,
                    w: scale,
                    h: scale,
                },
                color,
            );
        }
    }
}

fn glyph_3x5(ch: char) -> [u8; 5] {
    match ch {
        'A' => [0b010, 0b101, 0b111, 0b101, 0b101],
        'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        'C' => [0b111, 0b100, 0b100, 0b100, 0b111],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
        'F' => [0b111, 0b100, 0b110, 0b100, 0b100],
        'G' => [0b111, 0b100, 0b101, 0b101, 0b111],
        'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        'J' => [0b001, 0b001, 0b001, 0b101, 0b111],
        'K' => [0b101, 0b101, 0b110, 0b101, 0b101],
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'N' => [0b101, 0b111, 0b111, 0b111, 0b101],
        'O' => [0b111, 0b101, 0b101, 0b101, 0b111],
        'P' => [0b111, 0b101, 0b111, 0b100, 0b100],
        'Q' => [0b111, 0b101, 0b101, 0b111, 0b001],
        'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        'S' => [0b111, 0b100, 0b111, 0b001, 0b111],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
        'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
        'W' => [0b101, 0b101, 0b111, 0b111, 0b101],
        'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        '-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        '_' => [0b000, 0b000, 0b000, 0b000, 0b111],
        '+' => [0b000, 0b010, 0b111, 0b010, 0b000],
        '=' => [0b000, 0b111, 0b000, 0b111, 0b000],
        '!' => [0b010, 0b010, 0b010, 0b000, 0b010],
        '?' => [0b111, 0b001, 0b010, 0b000, 0b010],
        '@' => [0b111, 0b101, 0b111, 0b100, 0b111],
        '#' => [0b101, 0b111, 0b101, 0b111, 0b101],
        '$' => [0b011, 0b110, 0b010, 0b011, 0b110],
        '%' => [0b101, 0b001, 0b010, 0b100, 0b101],
        '^' => [0b010, 0b101, 0b000, 0b000, 0b000],
        '&' => [0b010, 0b101, 0b010, 0b101, 0b011],
        '*' => [0b101, 0b010, 0b111, 0b010, 0b101],
        '(' => [0b001, 0b010, 0b010, 0b010, 0b001],
        ')' => [0b100, 0b010, 0b010, 0b010, 0b100],
        '[' => [0b111, 0b100, 0b100, 0b100, 0b111],
        ']' => [0b111, 0b001, 0b001, 0b001, 0b111],
        '{' => [0b011, 0b010, 0b110, 0b010, 0b011],
        '}' => [0b110, 0b010, 0b011, 0b010, 0b110],
        ';' => [0b000, 0b010, 0b000, 0b010, 0b100],
        ':' => [0b000, 0b010, 0b000, 0b010, 0b000],
        '\'' => [0b010, 0b010, 0b000, 0b000, 0b000],
        '"' => [0b101, 0b101, 0b000, 0b000, 0b000],
        '`' => [0b100, 0b010, 0b000, 0b000, 0b000],
        '~' => [0b000, 0b011, 0b110, 0b000, 0b000],
        '\\' => [0b100, 0b100, 0b010, 0b001, 0b001],
        '|' => [0b010, 0b010, 0b010, 0b010, 0b010],
        ',' => [0b000, 0b000, 0b000, 0b010, 0b100],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        '/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        '<' => [0b001, 0b010, 0b100, 0b010, 0b001],
        '>' => [0b100, 0b010, 0b001, 0b010, 0b100],
        ' ' => [0b000, 0b000, 0b000, 0b000, 0b000],
        _ => [0b111, 0b001, 0b010, 0b000, 0b010],
    }
}

fn fill_rect(buf: &mut [u8], width: u32, height: u32, rect: RectPx, color: [u8; 4]) {
    let x0 = rect.x.max(0).min(width as i32) as u32;
    let y0 = rect.y.max(0).min(height as i32) as u32;
    let x1 = (rect.x + rect.w).max(0).min(width as i32) as u32;
    let y1 = (rect.y + rect.h).max(0).min(height as i32) as u32;

    for y in y0..y1 {
        for x in x0..x1 {
            let index = ((y * width + x) * 4) as usize;
            buf[index..index + 4].copy_from_slice(&color);
        }
    }
}

fn draw_circle(
    buf: &mut [u8],
    width: u32,
    height: u32,
    cx: i32,
    cy: i32,
    radius: i32,
    color: [u8; 4],
) {
    let r2 = radius * radius;
    let x0 = (cx - radius).max(0);
    let y0 = (cy - radius).max(0);
    let x1 = (cx + radius).min(width as i32 - 1);
    let y1 = (cy + radius).min(height as i32 - 1);

    for y in y0..=y1 {
        for x in x0..=x1 {
            let dx = x - cx;
            let dy = y - cy;
            if dx * dx + dy * dy <= r2 {
                let index = (((y as u32) * width + x as u32) * 4) as usize;
                buf[index..index + 4].copy_from_slice(&color);
            }
        }
    }
}

fn env_f64(name: &str, default: f64) -> f64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(default)
}

fn env_u32(name: &str, default: u32) -> u32 {
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

fn env_bool(name: &str, default: bool) -> bool {
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

fn load_xkb_keymap(config: &Config) -> Result<Vec<u8>> {
    let mut bytes = if let Some(path) = &config.xkb_keymap_path {
        fs::read(path).with_context(|| format!("read XKB keymap {}", path.display()))?
    } else {
        DEFAULT_XKB_KEYMAP.as_bytes().to_vec()
    };

    if !bytes.ends_with(&[0]) {
        bytes.push(0);
    }

    Ok(bytes)
}

const DEFAULT_XKB_KEYMAP: &str = r#"xkb_keymap {
xkb_keycodes "evdev+aliases(qwerty)" {
    include "evdev+aliases(qwerty)"
};
xkb_types "complete" {
    include "complete"
};
xkb_compatibility "complete" {
    include "complete"
};
xkb_symbols "pc+us+inet(evdev)" {
    include "pc+us+inet(evdev)"
};
xkb_geometry "pc(pc105)" {
    include "pc(pc105)"
};
};
"#;

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind::<wl_compositor::WlCompositor, _, _>(
                        name,
                        version.min(6),
                        qh,
                        (),
                    ));
                }
                "wl_shm" => {
                    state.shm = Some(registry.bind::<wl_shm::WlShm, _, _>(
                        name,
                        version.min(1),
                        qh,
                        (),
                    ));
                }
                "wl_seat" => {
                    let seat = registry.bind::<wl_seat::WlSeat, _, _>(
                        name,
                        version.min(8),
                        qh,
                        (),
                    );
                    let touch = seat.get_touch(qh, ());
                    state.touch = Some(touch);
                    state.seat = Some(seat);
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(
                        registry.bind::<zwlr_layer_shell_v1::ZwlrLayerShellV1, _, _>(
                            name,
                            version.min(4),
                            qh,
                            (),
                        ),
                    );
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.virtual_keyboard_manager = Some(registry.bind::<
                        zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
                        _,
                        _,
                    >(
                        name,
                        version.min(1),
                        qh,
                        (),
                    ));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_compositor::WlCompositor,
        _event: wl_compositor::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_region::WlRegion, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_region::WlRegion,
        _event: wl_region::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm::WlShm, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_shm::WlShm,
        _event: wl_shm::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_shm_pool::WlShmPool,
        _event: wl_shm_pool::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for App {
    fn event(
        state: &mut Self,
        proxy: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_buffer::Event::Release) {
            for backing in &mut state.buffers {
                if backing.buffer == proxy.clone() {
                    backing.released = true;
                    break;
                }
            }
            state.buffers.retain(|backing| !backing.released);
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_seat::WlSeat,
        _event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_surface::WlSurface,
        _event: wl_surface::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        _event: zwp_virtual_keyboard_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
        _event: zwp_virtual_keyboard_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_touch::WlTouch, ()> for App {
    fn event(
        state: &mut Self,
        _proxy: &wl_touch::WlTouch,
        event: wl_touch::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_touch::Event::Down { time, id, x, y, .. } => {
                state.touch_down(qh, id, time, x, y);
            }
            wl_touch::Event::Motion { time, id, x, y } => {
                state.touch_motion(qh, id, time, x, y);
            }
            wl_touch::Event::Up { time, id, .. } => {
                state.touch_up(qh, id, time);
            }
            wl_touch::Event::Cancel => {
                state.touch_cancel(qh);
            }
            _ => {}
        }
    }
}

impl Dispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        _event: zwlr_layer_shell_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        state: &mut Self,
        layer_surface: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer_surface.ack_configure(serial);
                state.width = width;
                state.height = height;
                state.capture_policy = state.engine.capture_policy(&state.config);
                if let Err(err) = state.attach_overlay_buffer(qh, width, height) {
                    eprintln!("touchdeck: failed to attach overlay buffer: {err:?}");
                    state.running = false;
                    return;
                }
                if let Err(err) = state.apply_input_region(qh, &state.capture_policy) {
                    eprintln!("touchdeck: failed to apply input region: {err:?}");
                    state.running = false;
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.running = false;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            action_swipe_left: Some(NiriAction::FocusColumnLeft),
            action_swipe_right: Some(NiriAction::FocusColumnRight),
            action_swipe_up: Some(NiriAction::FocusWorkspaceUp),
            action_swipe_down: Some(NiriAction::FocusWorkspaceDown),
            action_two_finger_tap: Some(NiriAction::ToggleOverview),
            tap_radius: 48.0,
            two_finger_tap_ms: 350,
            exit_tap_ms: 450,
            hold_ms: 180,
            double_tap_ms: 280,
            swipe_threshold_ratio: 0.08,
            swipe_threshold_min: 64.0,
            swipe_threshold_max: 140.0,
            debug_alpha: 0,
            debug_draw: false,
            mode_hint_ms: 700,
            log_touch: false,
            record_trace_path: None,
            xkb_keymap_path: None,
            slots: SlotRegistry::default(),
            keymap: Keymap::default(),
            macros: MacroRegistry::default(),
            exit_corner_enabled: true,
            exit_corner_ratio: 0.12,
            exit_corner_tap_ms: 350,
        }
    }

    fn test_size() -> SurfaceSize {
        SurfaceSize {
            width: 1000,
            height: 2000,
        }
    }

    #[test]
    fn maps_supported_niri_actions_to_ipc_json() {
        assert_eq!(
            niri_action_request_json(parse_niri_action("focus-column-left").unwrap()),
            r#"{"Action":{"FocusColumnLeft":{}}}"#
        );
        assert_eq!(
            parse_niri_action("focus_workspace_right").unwrap_err().to_string(),
            "unsupported niri action focus_workspace_right; supported actions: focus-column-left, focus-column-right, focus-workspace-up, focus-workspace-down, toggle-overview"
        );
        assert_eq!(
            niri_action_request_json(parse_niri_action("toggle-overview").unwrap()),
            r#"{"Action":{"ToggleOverview":{}}}"#
        );
    }

    fn contact(start_x: f64, start_y: f64, last_x: f64, last_y: f64) -> Contact {
        Contact {
            id: 1,
            start_x,
            start_y,
            last_x,
            last_y,
            start_time: 0,
            last_time: 100,
        }
    }

    fn gesture(max_active: usize, finished: Vec<Contact>) -> Gesture {
        Gesture {
            finished,
            max_active,
        }
    }

    fn dispatched_actions(effects: &[EngineEffect]) -> Vec<GestureAction> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                EngineEffect::Dispatch(action) => Some(action.clone()),
                _ => None,
            })
            .collect()
    }

    fn run_trace(trace: &str, config: &Config) -> Vec<EngineEffect> {
        let mut engine = Engine::default();
        let size = test_size();
        let mut effects = Vec::new();

        for line in trace.lines().filter(|line| !line.trim().is_empty()) {
            let event: TraceEvent = serde_json::from_str(line).unwrap();
            effects.extend(engine.process_timers(event.t(), config, size));
            effects.extend(engine.handle_trace_event(event, config, size));
        }

        if let Some(deadline) = engine.next_timer_deadline_ms() {
            effects.extend(engine.process_timers(deadline, config, size));
        }

        effects
    }

    #[test]
    fn default_mode_uses_fullscreen_capture() {
        let engine = Engine::default();
        let config = test_config();
        assert_eq!(engine.capture_policy(&config), CapturePolicy::Fullscreen);
    }

    #[test]
    fn one_finger_swipe_left_maps_to_focus_column_left() {
        let config = test_config();
        let gesture = gesture(1, vec![contact(800.0, 1000.0, 600.0, 1000.0)]);

        assert_eq!(
            resolve_niri_gesture(&gesture, &config, test_size()),
            GestureAction::Niri(NiriAction::FocusColumnLeft)
        );
    }

    #[test]
    fn one_finger_swipe_up_maps_to_focus_workspace_up() {
        let config = test_config();
        let gesture = gesture(1, vec![contact(500.0, 1200.0, 500.0, 900.0)]);

        assert_eq!(
            resolve_niri_gesture(&gesture, &config, test_size()),
            GestureAction::Niri(NiriAction::FocusWorkspaceUp)
        );
    }

    #[test]
    fn two_finger_tap_maps_to_toggle_overview() {
        let config = test_config();
        let mut a = contact(400.0, 900.0, 404.0, 904.0);
        a.id = 1;
        let mut b = contact(600.0, 900.0, 604.0, 904.0);
        b.id = 2;
        let gesture = gesture(2, vec![a, b]);

        assert_eq!(
            resolve_niri_gesture(&gesture, &config, test_size()),
            GestureAction::Niri(NiriAction::ToggleOverview)
        );
    }

    #[test]
    fn top_left_tap_exits() {
        let config = test_config();
        let gesture = gesture(1, vec![contact(50.0, 50.0, 52.0, 52.0)]);

        assert!(is_exit_gesture(&gesture, &config, test_size()));
    }

    #[test]
    fn empty_action_disables_gesture() {
        let mut config = test_config();
        config.action_swipe_left = None;
        let gesture = gesture(1, vec![contact(800.0, 1000.0, 600.0, 1000.0)]);

        assert_eq!(
            resolve_niri_gesture(&gesture, &config, test_size()),
            GestureAction::None
        );
    }

    #[test]
    fn bottom_edge_swipe_up_enters_text_mode() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();

        engine.handle_down(0, 0, 1, 500.0, 1950.0, &config, size);
        engine.handle_motion(1, 80, 500.0, 1700.0, &config);
        let effects = engine.handle_up(100, 100, 1, &config, size);

        assert_eq!(engine.mode, Mode::Text);
        assert!(effects.contains(&EngineEffect::SetCapture(CapturePolicy::Fullscreen)));
    }

    #[test]
    fn default_text_keyboard_row_tap_sends_key() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        let mut effects = Vec::new();
        engine.set_mode(Mode::Text, &mut effects, &config);

        engine.handle_down(0, 0, 1, 84.0, 1180.0, &config, size);
        let effects = engine.handle_up(80, 80, 1, &config, size);

        assert!(dispatched_actions(&effects).contains(&GestureAction::KeySequence(vec![
            KeyChord { keys: vec![KEY_Q] },
        ])));
    }

    #[test]
    fn toml_binding_parses_key_action() {
        let source = r#"
[[bindings]]
mode = "base"
layer = "base"
trigger = { type = "swipe", target = "left_bottom", direction = "left" }
behavior = { type = "key", key = "DEL" }
"#;
        let file_config: FileConfig = toml::from_str(source).unwrap();
        let binding = Binding::from_file_config(
            file_config.bindings.unwrap().remove(0),
            &SlotRegistry::default(),
            &MacroRegistry::default(),
        )
        .unwrap();

        assert_eq!(binding.mode, Mode::Base);
        assert_eq!(binding.layer, Layer::Base);
        assert_eq!(
            binding.trigger,
            Trigger::Swipe {
                target: named_target("left_bottom").unwrap(),
                fingers: 1,
                direction: SwipeDirection::Left,
                min_px: None,
                max_ms: None,
            }
        );
        assert_eq!(
            binding.behavior,
            Behavior::KeySequence(vec![KeyChord {
                keys: vec![KEY_BACKSPACE],
            }])
        );
    }

    #[test]
    fn toml_keyboard_map_expands_to_text_bindings() {
        let source = r#"
[keyboard]

[[keyboard.maps]]
mode = "text"
layer = "base"

[keyboard.maps.tap]
key_a = "a"
key_c = "C-c"

[keyboard.maps.swipe_up]
key_a = "!"

[keyboard.maps.swipe_left]
key_c = "<left>"
"#;
        let file_config: FileConfig = toml::from_str(source).unwrap();
        let maps = file_config.keyboard.unwrap().maps.unwrap();
        let bindings = expand_keyboard_maps(maps, &SlotRegistry::default()).unwrap();

        assert_eq!(bindings.len(), 4);
        let key_a = bindings
            .iter()
            .find(|binding| matches!(binding.trigger, Trigger::Tap { .. }) && binding.trigger.target_id() == "key_a")
            .unwrap();
        let key_c = bindings
            .iter()
            .find(|binding| matches!(binding.trigger, Trigger::Tap { .. }) && binding.trigger.target_id() == "key_c")
            .unwrap();
        let key_a_up = bindings
            .iter()
            .find(|binding| {
                matches!(
                    binding.trigger,
                    Trigger::Swipe {
                        direction: SwipeDirection::Up,
                        ..
                    }
                ) && binding.trigger.target_id() == "key_a"
            })
            .unwrap();
        let key_c_left = bindings
            .iter()
            .find(|binding| {
                matches!(
                    binding.trigger,
                    Trigger::Swipe {
                        direction: SwipeDirection::Left,
                        ..
                    }
                ) && binding.trigger.target_id() == "key_c"
            })
            .unwrap();
        assert_eq!(key_a.mode, Mode::Text);
        assert_eq!(
            key_a.behavior,
            Behavior::KeySequence(vec![KeyChord { keys: vec![KEY_A] }])
        );
        assert_eq!(
            key_c.behavior,
            Behavior::KeySequence(vec![KeyChord {
                keys: vec![KEY_LEFTCTRL, KEY_C],
            }])
        );
        assert_eq!(
            key_a_up.behavior,
            Behavior::KeySequence(vec![KeyChord {
                keys: vec![KEY_LEFTSHIFT, KEY_1],
            }])
        );
        assert_eq!(
            key_c_left.behavior,
            Behavior::KeySequence(vec![KeyChord { keys: vec![KEY_LEFT] }])
        );
    }

    #[test]
    fn default_keyboard_label_uses_tap_binding() {
        let keymap = Keymap::default();

        assert_eq!(
            keymap.slot_label(Mode::Text, &[Layer::Base], "key_q"),
            Some("q".to_string())
        );
        assert_eq!(
            keymap.slot_label(Mode::Text, &[Layer::Base], "key_h"),
            Some("h".to_string())
        );
        assert_eq!(
            keymap.slot_gesture_label(
                Mode::Text,
                &[Layer::Base],
                "key_q",
                SlotGestureKind::SwipeUp
            ),
            Some("1".to_string())
        );
        assert_eq!(
            keymap.slot_gesture_label(
                Mode::Text,
                &[Layer::Base],
                "key_h",
                SlotGestureKind::SwipeLeft
            ),
            Some("LEFT".to_string())
        );
    }

    #[test]
    fn default_text_keyboard_swipe_up_sends_symbol_key() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        let mut effects = Vec::new();
        engine.set_mode(Mode::Text, &mut effects, &config);

        engine.handle_down(0, 0, 1, 84.0, 1180.0, &config, size);
        engine.handle_motion(1, 60, 84.0, 980.0, &config);
        let effects = engine.handle_up(80, 80, 1, &config, size);

        assert!(dispatched_actions(&effects).contains(&GestureAction::KeySequence(vec![
            KeyChord { keys: vec![KEY_1] },
        ])));
    }

    #[test]
    fn default_text_keyboard_home_row_swipe_sends_arrow_key() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        let mut effects = Vec::new();
        engine.set_mode(Mode::Text, &mut effects, &config);

        engine.handle_down(0, 0, 1, 580.0, 1400.0, &config, size);
        engine.handle_motion(1, 60, 380.0, 1400.0, &config);
        let effects = engine.handle_up(80, 80, 1, &config, size);

        assert!(dispatched_actions(&effects).contains(&GestureAction::KeySequence(vec![
            KeyChord { keys: vec![KEY_LEFT] },
        ])));
    }

    #[test]
    fn toml_binding_parses_emacs_key_sequence() {
        let source = r#"
[[bindings]]
mode = "base"
layer = "base"
trigger = { type = "tap", target = "left_bottom" }
behavior = { type = "key", key = "C-x C-s" }
"#;
        let file_config: FileConfig = toml::from_str(source).unwrap();
        let binding = Binding::from_file_config(
            file_config.bindings.unwrap().remove(0),
            &SlotRegistry::default(),
            &MacroRegistry::default(),
        )
        .unwrap();

        assert_eq!(
            binding.behavior,
            Behavior::KeySequence(vec![
                KeyChord {
                    keys: vec![KEY_LEFTCTRL, KEY_X],
                },
                KeyChord {
                    keys: vec![KEY_LEFTCTRL, KEY_S],
                },
            ])
        );
    }

    #[test]
    fn emacs_key_parser_supports_punctuation_symbols() {
        assert_eq!(
            parse_emacs_key_sequence("-").unwrap(),
            vec![KeyChord {
                keys: vec![KEY_MINUS],
            }]
        );
        assert_eq!(
            parse_emacs_key_sequence("!").unwrap(),
            vec![KeyChord {
                keys: vec![KEY_LEFTSHIFT, KEY_1],
            }]
        );
    }

    #[test]
    fn svg_layout_loader_reads_rect_slots() {
        let source = r#"
<svg viewBox="0 0 1000 2000" xmlns="http://www.w3.org/2000/svg">
  <rect data-td-slot="thumb" data-td-role="key" data-td-capture="true" data-td-label="TH" x="800" y="1600" width="200" height="400" />
</svg>
"#;
        let slots = SlotRegistry::from_svg_str(source).unwrap();
        let target = slots.get("thumb").unwrap();

        assert_eq!(target.id, "thumb");
        assert_eq!(target.role, SlotRole::Key);
        assert!(target.capture);
        assert_eq!(target.label.as_deref(), Some("TH"));
        assert_eq!(
            target.rect,
            RectNorm {
                x0: 0.80,
                x1: 1.00,
                y0: 0.80,
                y1: 1.00,
            }
        );
    }

    #[test]
    fn checked_in_example_layout_and_config_parse() {
        let slots = SlotRegistry::from_svg_str(include_str!("../layouts/phone-portrait.svg")).unwrap();
        assert!(slots.get("key_q").is_ok());
        assert!(slots.get("key_spc").is_ok());

        let config: FileConfig = toml::from_str(include_str!("../touchdeck.example.toml")).unwrap();
        assert_eq!(
            config.layout.unwrap().svg.as_deref(),
            Some("layouts/phone-portrait.svg")
        );
        let maps = config.keyboard.unwrap().maps.unwrap();
        assert_eq!(maps.len(), 1);
        assert_eq!(
            maps[0].tap.as_ref().unwrap().get("key_q").map(String::as_str),
            Some("q")
        );
        assert_eq!(
            maps[0].swipe_up.as_ref().unwrap().get("key_w").map(String::as_str),
            Some("2")
        );
        assert_eq!(
            maps[0].swipe_left.as_ref().unwrap().get("key_h").map(String::as_str),
            Some("<left>")
        );
    }

    #[test]
    fn top_layer_binding_overrides_base_layer_binding() {
        let mut config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        config.keymap.bindings = vec![
            Binding {
                mode: Mode::Base,
                layer: Layer::Base,
                trigger: Trigger::Tap {
                    target: named_target("left_bottom").unwrap(),
                    fingers: 1,
                    max_ms: None,
                },
                behavior: Behavior::KeySequence(vec![KeyChord { keys: vec![KEY_SPACE] }]),
                priority: 0,
                consume: true,
            },
            Binding {
                mode: Mode::Base,
                layer: Layer::Niri,
                trigger: Trigger::Tap {
                    target: named_target("left_bottom").unwrap(),
                    fingers: 1,
                    max_ms: None,
                },
                behavior: Behavior::KeySequence(vec![KeyChord { keys: vec![KEY_ENTER] }]),
                priority: 0,
                consume: true,
            },
        ];

        engine.perform_action(GestureAction::LayerToggle(Layer::Niri), &mut Vec::new(), &config, None);
        engine.handle_down(0, 0, 1, 100.0, 1800.0, &config, size);
        let effects = engine.handle_up(80, 80, 1, &config, size);

        assert!(dispatched_actions(&effects).contains(&GestureAction::KeySequence(vec![KeyChord { keys: vec![KEY_ENTER] }])));
        assert!(!dispatched_actions(&effects).contains(&GestureAction::KeySequence(vec![KeyChord { keys: vec![KEY_SPACE] }])));
    }

    #[test]
    fn transparent_top_layer_falls_through_to_base_layer() {
        let mut config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        config.keymap.bindings = vec![
            Binding {
                mode: Mode::Base,
                layer: Layer::Base,
                trigger: Trigger::Tap {
                    target: named_target("left_bottom").unwrap(),
                    fingers: 1,
                    max_ms: None,
                },
                behavior: Behavior::KeySequence(vec![KeyChord { keys: vec![KEY_SPACE] }]),
                priority: 0,
                consume: true,
            },
            Binding {
                mode: Mode::Base,
                layer: Layer::Niri,
                trigger: Trigger::Tap {
                    target: named_target("left_bottom").unwrap(),
                    fingers: 1,
                    max_ms: None,
                },
                behavior: Behavior::Transparent,
                priority: 100,
                consume: true,
            },
        ];

        engine.perform_action(GestureAction::LayerToggle(Layer::Niri), &mut Vec::new(), &config, None);
        engine.handle_down(0, 0, 1, 100.0, 1800.0, &config, size);
        let effects = engine.handle_up(80, 80, 1, &config, size);

        assert!(dispatched_actions(&effects).contains(&GestureAction::KeySequence(vec![KeyChord { keys: vec![KEY_SPACE] }])));
    }

    #[test]
    fn toml_macro_behavior_expands_to_sequence() {
        let source = r#"
[macros.copy]
steps = [
  { type = "key_down", key = "<leftctrl>" },
  { type = "tap_key", key = "c" },
  { type = "key_up", key = "<leftctrl>" },
]

[[bindings]]
mode = "base"
layer = "base"
trigger = { type = "tap", target = "left_bottom" }
behavior = { type = "macro", macro = "copy" }
"#;
        let file_config: FileConfig = toml::from_str(source).unwrap();
        let mut macros = MacroRegistry::default();
        for (name, macro_config) in file_config.macros.unwrap() {
            macros.insert(&name, parse_action_steps(macro_config.steps).unwrap());
        }
        let binding = Binding::from_file_config(
            file_config.bindings.unwrap().remove(0),
            &SlotRegistry::default(),
            &macros,
        )
        .unwrap();

        assert_eq!(
            binding.behavior,
            Behavior::Sequence(vec![
                ActionStep::KeyDown(KEY_LEFTCTRL),
                ActionStep::TapKey(KEY_C),
                ActionStep::KeyUp(KEY_LEFTCTRL),
            ])
        );
    }

    #[test]
    fn layer_toggle_action_switches_layer() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        let mut effects = Vec::new();

        engine.perform_action(GestureAction::LayerToggle(Layer::Niri), &mut effects, &config, None);
        assert_eq!(engine.current_layer(), Layer::Niri);
        assert_eq!(engine.layer_stack, vec![Layer::Base, Layer::Niri]);

        engine.perform_action(GestureAction::LayerToggle(Layer::Niri), &mut effects, &config, None);
        assert_eq!(engine.current_layer(), Layer::Base);
        assert_eq!(engine.layer_stack, vec![Layer::Base]);

        assert_eq!(engine.capture_policy(&config), CapturePolicy::Fullscreen);
        assert!(effects.contains(&EngineEffect::Redraw));
        assert_eq!(config.hold_ms, 180);
        assert_eq!(size.width, 1000);
    }

    #[test]
    fn layer_momentary_hold_returns_previous_layer() {
        let mut config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        config.keymap.bindings = vec![Binding {
            mode: Mode::Base,
            layer: Layer::Base,
            trigger: Trigger::Hold {
                target: named_target("left_bottom").unwrap(),
                fingers: 1,
                min_ms: None,
            },
            behavior: Behavior::LayerMomentary(Layer::Niri),
            priority: 0,
            consume: true,
        }];

        engine.handle_down(0, 0, 1, 100.0, 1800.0, &config, size);
        engine.process_timers(181, &config, size);
        assert_eq!(engine.current_layer(), Layer::Niri);
        assert_eq!(engine.layer_stack, vec![Layer::Base, Layer::Niri]);

        engine.handle_up(220, 220, 1, &config, size);
        assert_eq!(engine.current_layer(), Layer::Base);
        assert_eq!(engine.layer_stack, vec![Layer::Base]);
    }

    #[test]
    fn left_bottom_hold_enters_momentary_and_release_returns_base() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();

        engine.handle_down(0, 0, 1, 100.0, 1800.0, &config, size);
        let effects = engine.process_timers(181, &config, size);
        assert_eq!(engine.mode, Mode::NiriMomentary);
        assert!(effects.contains(&EngineEffect::SetCapture(CapturePolicy::Fullscreen)));

        let effects = engine.handle_up(220, 220, 1, &config, size);
        assert_eq!(engine.mode, Mode::Base);
        assert!(effects.contains(&EngineEffect::SetCapture(CapturePolicy::Fullscreen)));
    }

    #[test]
    fn left_bottom_double_tap_locks_and_unlocks_niri_mode() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();

        engine.handle_down(0, 0, 1, 100.0, 1800.0, &config, size);
        engine.handle_up(80, 80, 1, &config, size);
        engine.handle_down(160, 160, 1, 104.0, 1804.0, &config, size);
        let effects = engine.handle_up(220, 220, 1, &config, size);

        assert_eq!(engine.mode, Mode::NiriLocked);
        assert!(effects.contains(&EngineEffect::SetCapture(CapturePolicy::Fullscreen)));

        engine.handle_down(400, 400, 1, 100.0, 1800.0, &config, size);
        engine.handle_up(460, 460, 1, &config, size);
        engine.handle_down(540, 540, 1, 102.0, 1802.0, &config, size);
        let effects = engine.handle_up(600, 600, 1, &config, size);

        assert_eq!(engine.mode, Mode::Base);
        assert!(effects.contains(&EngineEffect::SetCapture(CapturePolicy::Fullscreen)));
    }

    #[test]
    fn bottom_edge_double_tap_enters_and_exits_passthrough() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();

        engine.handle_down(0, 0, 1, 500.0, 1950.0, &config, size);
        engine.handle_up(60, 60, 1, &config, size);
        engine.handle_down(140, 140, 1, 504.0, 1952.0, &config, size);
        let effects = engine.handle_up(200, 200, 1, &config, size);

        assert_eq!(engine.mode, Mode::Passthrough);
        assert!(matches!(effects.as_slice(), [.., EngineEffect::SetCapture(CapturePolicy::Zones(_)), EngineEffect::Redraw]));
        let CapturePolicy::Zones(rects) = engine.capture_policy(&config) else {
            panic!("passthrough should use zoned capture");
        };
        assert!(rects.contains(&named_target("left_bottom").unwrap().rect));
        assert!(rects.contains(&named_target("bottom_edge").unwrap().rect));
        assert!(rects.contains(&named_target("top_left").unwrap().rect));
        assert!(!rects.contains(&named_target("center").unwrap().rect));

        engine.handle_down(380, 380, 1, 500.0, 1950.0, &config, size);
        engine.handle_up(430, 430, 1, &config, size);
        engine.handle_down(500, 500, 1, 504.0, 1952.0, &config, size);
        let effects = engine.handle_up(550, 550, 1, &config, size);

        assert_eq!(engine.mode, Mode::Base);
        assert!(effects.contains(&EngineEffect::SetCapture(CapturePolicy::Fullscreen)));
    }

    #[test]
    fn passthrough_hold_returns_to_passthrough_after_momentary_niri() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();

        engine.handle_down(0, 0, 1, 500.0, 1950.0, &config, size);
        engine.handle_up(60, 60, 1, &config, size);
        engine.handle_down(140, 140, 1, 504.0, 1952.0, &config, size);
        engine.handle_up(200, 200, 1, &config, size);
        assert_eq!(engine.mode, Mode::Passthrough);

        engine.handle_down(300, 300, 1, 100.0, 1800.0, &config, size);
        engine.process_timers(481, &config, size);
        assert_eq!(engine.mode, Mode::NiriMomentary);

        let effects = engine.handle_up(520, 520, 1, &config, size);
        assert_eq!(engine.mode, Mode::Passthrough);
        assert!(matches!(effects.as_slice(), [.., EngineEffect::SetCapture(CapturePolicy::Zones(_)), EngineEffect::Redraw]));
    }

    #[test]
    fn replay_hold_then_same_finger_swipe_dispatches_niri_action() {
        let config = test_config();
        let trace = r#"
{"type":"down","t":0,"wl_time":0,"id":1,"x":100.0,"y":1800.0}
{"type":"motion","t":220,"wl_time":220,"id":1,"x":260.0,"y":1800.0}
{"type":"up","t":260,"wl_time":260,"id":1}
"#;

        let effects = run_trace(trace, &config);
        assert!(dispatched_actions(&effects).contains(&GestureAction::Niri(
            NiriAction::FocusColumnRight
        )));
    }

    #[test]
    fn replay_hold_plus_second_finger_swipe_dispatches_niri_action() {
        let config = test_config();
        let trace = r#"
{"type":"down","t":0,"wl_time":0,"id":1,"x":100.0,"y":1800.0}
{"type":"down","t":220,"wl_time":220,"id":2,"x":800.0,"y":1000.0}
{"type":"motion","t":240,"wl_time":240,"id":2,"x":600.0,"y":1000.0}
{"type":"up","t":260,"wl_time":260,"id":2}
{"type":"up","t":300,"wl_time":300,"id":1}
"#;

        let effects = run_trace(trace, &config);
        assert!(dispatched_actions(&effects).contains(&GestureAction::Niri(
            NiriAction::FocusColumnLeft
        )));
    }
}
