use std::collections::HashMap;

use anyhow::{anyhow, Result};

use crate::action::{ActionStep, NiriAction};
use crate::config::{Config, KeyRoute, KeyTranslationPolicy};
use crate::geometry::{RectNorm, SurfaceSize};
use crate::gesture::{
    contact_movement, is_tap_like, recognize_gesture_kind, Contact, Gesture, GestureKind,
    SwipeDirection, TapRecord,
};
use crate::key::{key_code_label, key_sequence_label, normalize_name, KeyChord};
use crate::layout::SlotTarget;
use crate::mode::{layer_name, mode_name, Layer, Mode, SlotGestureKind};

#[derive(Clone, Debug)]
pub(crate) struct Keymap {
    pub(crate) bindings: Vec<Binding>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }
}

impl Keymap {
    pub(crate) fn resolve_hold(
        &self,
        mode: Mode,
        layers: &[Layer],
        size: SurfaceSize,
        x: f64,
        y: f64,
        default_hold_ms: u32,
        default_repeat_start_ms: u32,
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
                    binding.trigger.hold_ms().unwrap_or_else(|| {
                        if binding.behavior.is_repeat() {
                            default_repeat_start_ms
                        } else {
                            default_hold_ms
                        }
                    }),
                ));
            }
        }

        None
    }

    pub(crate) fn resolve_release(
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
            let double_tap_binding = self.find_release_binding(mode, layers, |binding| {
                binding.trigger.matches_double_tap_start(
                    size,
                    contact.start_x,
                    contact.start_y,
                    gesture.max_active,
                )
            });

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

    pub(crate) fn resolve_active_swipe(
        &self,
        mode: Mode,
        layers: &[Layer],
        contact: &Contact,
        config: &Config,
        size: SurfaceSize,
    ) -> GestureAction {
        let gesture = Gesture {
            max_active: 1,
            finished: vec![contact.clone()],
        };
        let Some(kind) = recognize_gesture_kind(&gesture, config, size) else {
            return GestureAction::None;
        };
        if !matches!(
            kind,
            GestureKind::SwipeLeft
                | GestureKind::SwipeRight
                | GestureKind::SwipeUp
                | GestureKind::SwipeDown
        ) {
            return GestureAction::None;
        }

        self.find_release_binding(mode, layers, |binding| {
            binding
                .trigger
                .matches_release(kind, &gesture, config, size)
        })
        .map(|binding| binding.behavior.clone().into_action())
        .filter(GestureAction::is_active_swipe_action)
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
                .filter(|binding| {
                    binding.mode == mode && binding.layer == *layer && predicate(binding)
                })
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

    pub(crate) fn capture_rects(&self, mode: Mode, layers: &[Layer]) -> Vec<RectNorm> {
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

    pub(crate) fn slot_label(&self, mode: Mode, layers: &[Layer], slot_id: &str) -> Option<String> {
        self.slot_label_from_bindings(mode, layers, slot_id, true)
            .or_else(|| self.slot_label_from_bindings(mode, layers, slot_id, false))
    }

    pub(crate) fn slot_gesture_label(
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

    pub(crate) fn slot_label_from_bindings(
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
pub(crate) struct Binding {
    pub(crate) mode: Mode,
    pub(crate) layer: Layer,
    pub(crate) trigger: Trigger,
    pub(crate) behavior: Behavior,
    pub(crate) priority: i32,
    pub(crate) consume: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct LastKeySequence {
    pub(crate) sequence: Vec<KeyChord>,
    pub(crate) translation: Option<KeyTranslationPolicy>,
    pub(crate) route: Option<KeyRoute>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct MacroRegistry {
    pub(crate) macros: HashMap<String, Vec<ActionStep>>,
}

impl MacroRegistry {
    pub(crate) fn clear(&mut self) {
        self.macros.clear();
    }

    pub(crate) fn insert(&mut self, name: &str, steps: Vec<ActionStep>) {
        self.macros.insert(normalize_name(name), steps);
    }

    pub(crate) fn get(&self, name: &str) -> Result<Vec<ActionStep>> {
        self.macros
            .get(&normalize_name(name))
            .cloned()
            .ok_or_else(|| anyhow!("unknown macro {name}"))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Trigger {
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
    pub(crate) fn target(&self) -> &SlotTarget {
        match self {
            Self::Tap { target, .. }
            | Self::DoubleTap { target, .. }
            | Self::Hold { target, .. }
            | Self::Swipe { target, .. } => target,
        }
    }

    pub(crate) fn rect(&self) -> RectNorm {
        self.target().rect
    }

    #[allow(dead_code)]
    pub(crate) fn target_id(&self) -> &str {
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

    fn matches_double_tap_start(&self, size: SurfaceSize, x: f64, y: f64, fingers: usize) -> bool {
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
                    && target
                        .rect
                        .contains_px(size, contact.start_x, contact.start_y)
                    && is_tap_like(
                        gesture,
                        config.tap_radius,
                        max_ms.unwrap_or(config.two_finger_tap_ms),
                    )
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
                    && target
                        .rect
                        .contains_px(size, contact.start_x, contact.start_y)
                    && min_px.is_none_or(|threshold| contact_movement(contact) >= threshold)
                    && max_ms.is_none_or(|limit| {
                        contact.last_time.saturating_sub(contact.start_time) <= limit
                    })
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Behavior {
    Niri(NiriAction),
    KeySequence(Vec<KeyChord>),
    KeyHold(u32),
    ModMorph {
        mods: u32,
        keep_mods: u32,
        normal: Box<Behavior>,
        morph: Box<Behavior>,
    },
    KeyRepeat,
    HoldRepeat {
        sequence: Vec<KeyChord>,
        start_ms: Option<u32>,
        interval_ms: Option<u32>,
        translation: Option<KeyTranslationPolicy>,
        route: Option<KeyRoute>,
    },
    KeySequenceWithOptions {
        sequence: Vec<KeyChord>,
        translation: Option<KeyTranslationPolicy>,
        route: Option<KeyRoute>,
    },
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

    fn is_repeat(&self) -> bool {
        matches!(self, Self::HoldRepeat { .. })
    }

    fn into_action(self) -> GestureAction {
        match self {
            Self::Niri(action) => GestureAction::Niri(action),
            Self::KeySequence(sequence) => GestureAction::KeySequence(sequence),
            Self::KeyHold(key) => GestureAction::KeyHold(key),
            Self::ModMorph {
                mods,
                keep_mods,
                normal,
                morph,
            } => GestureAction::ModMorph {
                mods,
                keep_mods,
                normal: Box::new(normal.into_action()),
                morph: Box::new(morph.into_action()),
            },
            Self::KeyRepeat => GestureAction::KeyRepeat,
            Self::HoldRepeat {
                sequence,
                start_ms,
                interval_ms,
                translation,
                route,
            } => GestureAction::HoldRepeat {
                sequence,
                start_ms,
                interval_ms,
                translation,
                route,
            },
            Self::KeySequenceWithOptions {
                sequence,
                translation,
                route,
            } => GestureAction::KeySequenceWithOptions {
                sequence,
                translation,
                route,
            },
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


#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) enum GestureAction {
    Niri(NiriAction),
    KeySequence(Vec<KeyChord>),
    KeySequenceWithOptions {
        sequence: Vec<KeyChord>,
        translation: Option<KeyTranslationPolicy>,
        route: Option<KeyRoute>,
    },
    KeyHold(u32),
    ModMorph {
        mods: u32,
        keep_mods: u32,
        normal: Box<GestureAction>,
        morph: Box<GestureAction>,
    },
    KeyRepeat,
    HoldRepeat {
        sequence: Vec<KeyChord>,
        start_ms: Option<u32>,
        interval_ms: Option<u32>,
        translation: Option<KeyTranslationPolicy>,
        route: Option<KeyRoute>,
    },
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

impl GestureAction {
    pub(crate) fn is_active_swipe_action(&self) -> bool {
        matches!(self, Self::KeyHold(_) | Self::HoldRepeat { .. })
    }
}


fn behavior_label(behavior: &Behavior) -> Option<String> {
    match behavior {
        Behavior::Niri(action) => Some(action.as_str().to_string()),
        Behavior::KeySequence(sequence) => key_sequence_label(sequence),
        Behavior::KeySequenceWithOptions {
            sequence,
            translation,
            route,
        } => key_sequence_label(sequence).map(|label| {
            let mut label = label;
            if let Some(translation) = translation {
                label.push('/');
                label.push_str(translation.as_str());
            }
            if let Some(route) = route {
                label.push('@');
                label.push_str(route.as_str());
            }
            label
        }),
        Behavior::KeyHold(key) => key_code_label(*key).map(|label| format!("{}+", label)),
        Behavior::ModMorph { .. } => Some("morph".to_string()),
        Behavior::KeyRepeat => Some("repeat".to_string()),
        Behavior::HoldRepeat { sequence, .. } => {
            key_sequence_label(sequence).map(|label| format!("{}...", label))
        }
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

