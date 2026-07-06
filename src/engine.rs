use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::action::{NiriCommand, NiriResizeEdge};
use crate::config::{Config, KeyRoute, KeyTranslationPolicy};
use crate::geometry::{RectNorm, SurfaceSize};
use crate::gesture::{contact_movement, is_tap_like, Contact, Gesture, TapRecord};
use crate::key::KeyChord;
use crate::keymap::{
    gesture_centroid, ActiveSwipeQuery, DragStartQuery, GestureAction, HoldQuery, HoldTapFlavor,
    KeymapContext, ReleaseQuery,
};
use crate::mode::{default_layer_stack_for_mode, layer_name, mode_name, Layer, Mode};

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum TraceEvent {
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
    pub(crate) fn t(&self) -> u64 {
        match self {
            TraceEvent::Down { t, .. }
            | TraceEvent::Motion { t, .. }
            | TraceEvent::Up { t, .. }
            | TraceEvent::Cancel { t } => *t,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CapturePolicy {
    Fullscreen,
    Zones(Vec<RectNorm>),
    #[allow(dead_code)]
    None,
}

#[derive(Clone, Debug)]
pub(crate) struct HoldCandidate {
    pub(crate) id: i32,
    deadline_ms: u64,
    action: GestureAction,
    flavor: Option<HoldTapFlavor>,
    interrupted: bool,
}

#[derive(Clone, Debug)]
struct MomentaryState {
    hold_id: i32,
    return_mode: Mode,
    return_layer_stack: Vec<Layer>,
}

#[derive(Clone, Debug)]
struct HeldActionState {
    hold_id: i32,
}

#[derive(Clone, Debug)]
struct RepeatState {
    hold_id: i32,
    next_ms: u64,
    interval_ms: u32,
    sequence: Vec<KeyChord>,
    translation: Option<KeyTranslationPolicy>,
    route: Option<KeyRoute>,
}

#[derive(Clone, Debug)]
enum ContinuousDragOperation {
    NiriMove,
    NiriResize { edge: NiriResizeEdge },
}

#[derive(Clone, Debug)]
struct ContinuousDragState {
    operation: ContinuousDragOperation,
    contact_ids: Vec<i32>,
    start_x: f64,
    start_y: f64,
    last_x: f64,
    last_y: f64,
}

#[derive(Clone, Debug)]
struct ArmedDragState {
    edge: NiriResizeEdge,
    fingers: usize,
    min_px: f64,
    expires_at_ms: u64,
}

#[derive(Clone, Debug)]
struct StickyLayerState {
    layer: Layer,
    expires_at_ms: u64,
}

#[derive(Debug)]
pub(crate) struct Engine {
    pub(crate) mode: Mode,
    pub(crate) layer_stack: Vec<Layer>,
    pub(crate) active: HashMap<i32, Contact>,
    finished: Vec<Contact>,
    max_active: usize,
    pub(crate) hold_candidate: Option<HoldCandidate>,
    momentary: Option<MomentaryState>,
    held_actions: Vec<HeldActionState>,
    repeaters: Vec<RepeatState>,
    continuous_drag: Option<ContinuousDragState>,
    armed_drag: Option<ArmedDragState>,
    sticky_layer: Option<StickyLayerState>,
    last_tap: Option<TapRecord>,
    now_ms: u64,
    pub(crate) last_action: Option<String>,
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            mode: Mode::Base,
            layer_stack: vec![Layer::base()],
            active: HashMap::new(),
            finished: Vec::new(),
            max_active: 0,
            hold_candidate: None,
            momentary: None,
            held_actions: Vec::new(),
            repeaters: Vec::new(),
            continuous_drag: None,
            armed_drag: None,
            sticky_layer: None,
            last_tap: None,
            now_ms: 0,
            last_action: None,
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum EngineEffect {
    SetCapture(CapturePolicy),
    Dispatch(GestureAction),
    Press { hold_id: i32, action: GestureAction },
    Release { hold_id: i32 },
    InteractiveMoveBegin { x: f64, y: f64 },
    InteractiveMoveUpdate { x: f64, y: f64, dx: f64, dy: f64 },
    InteractiveMoveEnd,
    InteractiveResizeBegin { edge: NiriResizeEdge },
    InteractiveResizeUpdate { dx: f64, dy: f64 },
    InteractiveResizeEnd,
    Redraw,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TouchSample {
    pub(crate) now_ms: u64,
    pub(crate) time: u32,
    pub(crate) id: i32,
    pub(crate) x: f64,
    pub(crate) y: f64,
}

#[derive(Debug)]
struct HoldRepeatStart {
    hold_id: i32,
    now_ms: u64,
    sequence: Vec<KeyChord>,
    start_ms: Option<u32>,
    interval_ms: Option<u32>,
    translation: Option<KeyTranslationPolicy>,
    route: Option<KeyRoute>,
}

impl Engine {
    fn keymap_context(&self, size: SurfaceSize) -> KeymapContext<'_> {
        KeymapContext {
            mode: self.mode,
            layers: &self.layer_stack,
            size,
        }
    }

    pub(crate) fn capture_policy(&self, config: &Config) -> CapturePolicy {
        match self.mode {
            Mode::Passthrough => {
                CapturePolicy::Zones(config.keymap.capture_rects(self.mode, &self.layer_stack))
            }
            Mode::NiriMomentary | Mode::NiriLocked => CapturePolicy::Fullscreen,
            Mode::Base | Mode::Text => CapturePolicy::Fullscreen,
        }
    }

    pub(crate) fn next_timer_deadline_ms(&self) -> Option<u64> {
        let mut deadline = self
            .hold_candidate
            .as_ref()
            .map(|candidate| candidate.deadline_ms);
        if let Some(armed) = &self.armed_drag {
            deadline = Some(deadline.map_or(armed.expires_at_ms, |deadline| {
                deadline.min(armed.expires_at_ms)
            }));
        }
        if let Some(sticky) = &self.sticky_layer {
            deadline = Some(deadline.map_or(sticky.expires_at_ms, |deadline| {
                deadline.min(sticky.expires_at_ms)
            }));
        }
        self.repeaters.iter().map(|repeater| repeater.next_ms).fold(
            deadline,
            |deadline, repeat_deadline| {
                Some(deadline.map_or(repeat_deadline, |deadline| deadline.min(repeat_deadline)))
            },
        )
    }

    pub(crate) fn process_timers(
        &mut self,
        now_ms: u64,
        config: &Config,
        _size: SurfaceSize,
    ) -> Vec<EngineEffect> {
        self.now_ms = now_ms;
        let mut effects = Vec::new();

        if self
            .armed_drag
            .as_ref()
            .is_some_and(|armed| now_ms >= armed.expires_at_ms)
        {
            self.armed_drag = None;
            effects.push(EngineEffect::Redraw);
        }

        if self
            .sticky_layer
            .as_ref()
            .is_some_and(|sticky| now_ms >= sticky.expires_at_ms)
        {
            self.clear_sticky_layer(&mut effects);
        }

        if let Some(candidate) = self.hold_candidate.clone() {
            if now_ms >= candidate.deadline_ms {
                if candidate.flavor == Some(HoldTapFlavor::TapUnlessInterrupted)
                    && !candidate.interrupted
                {
                    if let Some(candidate) = &mut self.hold_candidate {
                        candidate.deadline_ms = u64::MAX;
                    }
                } else {
                    self.activate_hold_candidate(now_ms, config, &mut effects);
                }
            }
        }

        let active_ids = self.active.keys().copied().collect::<Vec<_>>();
        for repeater in &mut self.repeaters {
            if now_ms < repeater.next_ms || !active_ids.contains(&repeater.hold_id) {
                continue;
            }
            if let Some(translation) = repeater.translation {
                effects.push(EngineEffect::Dispatch(
                    GestureAction::KeySequenceWithOptions {
                        sequence: repeater.sequence.clone(),
                        translation: Some(translation),
                        route: repeater.route,
                    },
                ));
            } else if let Some(route) = repeater.route {
                effects.push(EngineEffect::Dispatch(
                    GestureAction::KeySequenceWithOptions {
                        sequence: repeater.sequence.clone(),
                        translation: None,
                        route: Some(route),
                    },
                ));
            } else {
                effects.push(EngineEffect::Dispatch(GestureAction::KeySequence(
                    repeater.sequence.clone(),
                )));
            }
            repeater.next_ms = now_ms + u64::from(repeater.interval_ms.max(1));
        }
        self.repeaters
            .retain(|repeater| active_ids.contains(&repeater.hold_id));

        effects
    }

    pub(crate) fn handle_down(
        &mut self,
        sample: TouchSample,
        config: &Config,
        size: SurfaceSize,
    ) -> Vec<EngineEffect> {
        self.now_ms = sample.now_ms;
        self.expire_armed_drag(sample.now_ms);

        self.active.insert(
            sample.id,
            Contact {
                id: sample.id,
                start_x: sample.x,
                start_y: sample.y,
                last_x: sample.x,
                last_y: sample.y,
                start_time: sample.time,
                last_time: sample.time,
            },
        );
        self.max_active = self.max_active.max(self.active.len());

        let mut effects = Vec::new();
        self.interrupt_hold_candidate_on_down(sample.id, sample.now_ms, config, &mut effects);

        if let Some(hold) = config.keymap.resolve_hold(HoldQuery {
            context: self.keymap_context(size),
            x: sample.x,
            y: sample.y,
            default_hold_ms: config.hold_ms,
            default_repeat_start_ms: config.repeat_start_ms,
        }) {
            self.hold_candidate = Some(HoldCandidate {
                id: sample.id,
                deadline_ms: sample.now_ms + u64::from(hold.min_ms),
                action: hold.action,
                flavor: hold.flavor,
                interrupted: false,
            });
        }

        effects.extend(redraw_if_debug(config));
        effects
    }

    pub(crate) fn handle_motion(
        &mut self,
        sample: TouchSample,
        config: &Config,
        size: SurfaceSize,
    ) -> Vec<EngineEffect> {
        self.now_ms = sample.now_ms;
        self.expire_armed_drag(sample.now_ms);

        let mut action = GestureAction::None;
        let mut moved_contact = None;

        if let Some(contact) = self.active.get_mut(&sample.id) {
            contact.last_x = sample.x;
            contact.last_y = sample.y;
            contact.last_time = sample.time;

            if let Some(candidate) = &self.hold_candidate {
                if candidate.id == sample.id && contact_movement(contact) > config.tap_radius {
                    self.hold_candidate = None;
                }
            }

            moved_contact = Some(*contact);
        }

        let mut effects = Vec::new();
        if self.continuous_drag.is_some() {
            self.update_continuous_drag(&mut effects);
            effects.extend(redraw_if_debug(config));
            return effects;
        }

        if let Some(armed) = self.armed_drag.clone() {
            let gesture = self.active_non_hold_gesture();
            if self.armed_drag_matches(&gesture, &armed) {
                self.armed_drag = None;
                self.start_continuous_drag(
                    ContinuousDragOperation::NiriResize { edge: armed.edge },
                    gesture,
                    &mut effects,
                );
                effects.extend(redraw_if_debug(config));
                return effects;
            }
        }

        if matches!(self.mode, Mode::NiriMomentary | Mode::NiriLocked) {
            let gesture = self.active_non_hold_gesture();
            let action = config.keymap.resolve_drag_start(DragStartQuery {
                context: self.keymap_context(size),
                gesture: &gesture,
                config,
            });
            if action == GestureAction::NiriInteractiveMove {
                self.start_continuous_drag(
                    ContinuousDragOperation::NiriMove,
                    gesture,
                    &mut effects,
                );
                effects.extend(redraw_if_debug(config));
                return effects;
            }
        }

        if let Some(contact) = moved_contact {
            if !self.hold_contact_ids().contains(&sample.id) && self.active_non_hold_count() == 1 {
                action = config.keymap.resolve_active_swipe(ActiveSwipeQuery {
                    context: self.keymap_context(size),
                    contact: &contact,
                    config,
                });
            }
        }

        if action != GestureAction::None {
            if self
                .hold_candidate
                .as_ref()
                .is_some_and(|candidate| candidate.id == sample.id)
            {
                self.hold_candidate = None;
            }
            self.last_tap = None;
            self.start_active_action(sample.id, sample.now_ms, action, config, &mut effects);
        }

        effects.extend(redraw_if_debug(config));
        effects
    }

    pub(crate) fn handle_up(
        &mut self,
        now_ms: u64,
        time: u32,
        id: i32,
        config: &Config,
        size: SurfaceSize,
    ) -> Vec<EngineEffect> {
        self.now_ms = now_ms;
        let mut interrupt_effects = Vec::new();
        self.interrupt_hold_candidate_on_up(id, now_ms, config, &mut interrupt_effects);

        if let Some(candidate) = &self.hold_candidate {
            if candidate.id == id {
                self.hold_candidate = None;
            }
        }

        let Some(mut contact) = self.active.remove(&id) else {
            return Vec::new();
        };
        contact.last_time = time;

        let was_held_action = self.held_actions.iter().any(|held| held.hold_id == id);
        let was_repeating = self.repeaters.iter().any(|repeater| repeater.hold_id == id);
        let mut held_action_effects = interrupt_effects;
        held_action_effects.extend(self.release_held_actions_for(id));
        self.stop_repeaters_for(id);
        if self.drag_contains(id) {
            let mut effects = std::mem::take(&mut held_action_effects);
            self.finish_continuous_drag(&mut effects);
            effects.extend(redraw_if_debug(config));
            return effects;
        }
        if was_held_action || was_repeating {
            held_action_effects.extend(redraw_if_debug(config));
            self.max_active = self.active.len();
            return held_action_effects;
        }

        if self.mode != Mode::NiriMomentary
            && self
                .momentary
                .as_ref()
                .is_some_and(|momentary| momentary.hold_id == id)
        {
            let mut effects = std::mem::take(&mut held_action_effects);
            self.return_from_momentary(&mut effects, config);
            self.reset_contacts();
            return effects;
        }

        match self.mode {
            Mode::Base | Mode::Text => {
                self.finished.push(contact);
                if self.active_non_hold_count() == 0 {
                    let gesture = self.take_finished_non_hold_gesture();
                    let mut effects = std::mem::take(&mut held_action_effects);
                    effects.extend(self.resolve_base_gesture(now_ms, gesture, config, size));
                    effects
                } else {
                    held_action_effects.extend(redraw_if_debug(config));
                    held_action_effects
                }
            }
            Mode::Passthrough => {
                self.finished.push(contact);
                if self.active_non_hold_count() == 0 {
                    let gesture = self.take_finished_non_hold_gesture();
                    let mut effects = std::mem::take(&mut held_action_effects);
                    effects.extend(self.resolve_passthrough_gesture(now_ms, gesture, config, size));
                    effects
                } else {
                    held_action_effects.extend(redraw_if_debug(config));
                    held_action_effects
                }
            }
            Mode::NiriLocked => {
                self.finished.push(contact);
                if self.active_non_hold_count() == 0 {
                    let gesture = self.take_finished_non_hold_gesture();
                    let mut effects = std::mem::take(&mut held_action_effects);
                    effects.extend(self.resolve_locked_gesture(now_ms, gesture, config, size));
                    effects
                } else {
                    held_action_effects.extend(redraw_if_debug(config));
                    held_action_effects
                }
            }
            Mode::NiriMomentary => {
                if self
                    .momentary
                    .as_ref()
                    .is_some_and(|momentary| momentary.hold_id == id)
                {
                    let mut effects = std::mem::take(&mut held_action_effects);
                    self.finish_continuous_drag(&mut effects);
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
                        let mut effects = std::mem::take(&mut held_action_effects);
                        effects.extend(redraw_if_debug(config));
                        let action =
                            self.resolve_configured_or_niri(&gesture, config, size, now_ms);
                        self.perform_action(action, &mut effects, config, None);
                        effects
                    } else {
                        held_action_effects.extend(redraw_if_debug(config));
                        held_action_effects
                    }
                }
            }
        }
    }

    pub(crate) fn handle_cancel(&mut self, config: &Config) -> Vec<EngineEffect> {
        let mut effects = self.release_all_held_actions();
        self.finish_continuous_drag(&mut effects);
        self.armed_drag = None;
        self.clear_sticky_layer(&mut effects);
        self.set_mode(Mode::Base, &mut effects, config);
        self.reset_contacts();
        effects.push(EngineEffect::Redraw);
        effects
    }

    #[cfg(test)]
    pub(crate) fn handle_trace_event(
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
            } => self.handle_down(
                TouchSample {
                    now_ms: t,
                    time: wl_time,
                    id,
                    x,
                    y,
                },
                config,
                size,
            ),
            TraceEvent::Motion {
                t,
                wl_time,
                id,
                x,
                y,
            } => self.handle_motion(
                TouchSample {
                    now_ms: t,
                    time: wl_time,
                    id,
                    x,
                    y,
                },
                config,
                size,
            ),
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
        let context = KeymapContext {
            mode: self.mode,
            layers: &self.layer_stack,
            size,
        };
        let action = config.keymap.resolve_release(ReleaseQuery {
            context,
            gesture,
            config,
            last_tap: &mut self.last_tap,
            now_ms,
        });
        if action != GestureAction::None {
            return action;
        }

        if matches!(self.mode, Mode::NiriMomentary | Mode::NiriLocked) {
            resolve_niri_gesture(gesture, config, size)
        } else {
            GestureAction::None
        }
    }

    pub(crate) fn perform_action(
        &mut self,
        action: GestureAction,
        effects: &mut Vec<EngineEffect>,
        config: &Config,
        hold_id: Option<i32>,
    ) {
        let consume_sticky_layer = self.should_consume_sticky_layer(&action, hold_id);

        match action {
            GestureAction::Niri(_)
            | GestureAction::NiriInteractiveMove
            | GestureAction::KeySequence(_)
            | GestureAction::KeySequenceWithOptions { .. }
            | GestureAction::ModMorph { .. }
            | GestureAction::KeyRepeat
            | GestureAction::HoldRepeat { .. }
            | GestureAction::Exit => self.perform_dispatch_action(action, effects, hold_id),
            GestureAction::NiriInteractiveResize {
                edge,
                fingers,
                min_px,
                timeout_ms,
            } => {
                self.remember_held_action_if_needed(hold_id);
                self.arm_resize(edge, fingers, min_px, timeout_ms, config);
                effects.push(EngineEffect::Redraw);
            }
            GestureAction::KeyHold(key) => {
                if let Some(hold_id) = hold_id {
                    self.remember_held_action(hold_id);
                    effects.push(EngineEffect::Press {
                        hold_id,
                        action: GestureAction::KeyHold(key),
                    });
                } else {
                    effects.push(EngineEffect::Dispatch(GestureAction::KeySequence(vec![
                        KeyChord { keys: vec![key] },
                    ])));
                }
            }
            GestureAction::Sequence(_) => {
                self.perform_dispatch_action(action, effects, hold_id);
            }
            GestureAction::ModeSet(mode) => {
                self.remember_held_action_if_needed(hold_id);
                self.set_mode(mode, effects, config);
            }
            GestureAction::ModeToggle(mode) => {
                self.remember_held_action_if_needed(hold_id);
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
                self.remember_held_action_if_needed(hold_id);
                self.set_layer(layer, effects);
            }
            GestureAction::LayerToggle(layer) => {
                self.remember_held_action_if_needed(hold_id);
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
            GestureAction::LayerSticky { layer, timeout_ms } => {
                self.remember_held_action_if_needed(hold_id);
                self.activate_sticky_layer(layer, timeout_ms, effects);
            }
            GestureAction::None => {}
        }

        if consume_sticky_layer {
            self.clear_sticky_layer(effects);
        }
    }

    fn start_active_action(
        &mut self,
        hold_id: i32,
        now_ms: u64,
        action: GestureAction,
        config: &Config,
        effects: &mut Vec<EngineEffect>,
    ) {
        match action {
            GestureAction::HoldRepeat {
                sequence,
                start_ms,
                interval_ms,
                translation,
                route,
            } => self.start_hold_repeat(
                HoldRepeatStart {
                    hold_id,
                    now_ms,
                    sequence,
                    start_ms,
                    interval_ms,
                    translation,
                    route,
                },
                config,
                effects,
            ),
            action => self.perform_action(action, effects, config, Some(hold_id)),
        }
    }

    fn interrupt_hold_candidate_on_down(
        &mut self,
        id: i32,
        now_ms: u64,
        config: &Config,
        effects: &mut Vec<EngineEffect>,
    ) {
        let Some(candidate) = &mut self.hold_candidate else {
            return;
        };
        if candidate.id == id {
            return;
        }

        match candidate.flavor {
            Some(HoldTapFlavor::HoldPreferred | HoldTapFlavor::TapUnlessInterrupted) => {
                candidate.interrupted = true;
                self.activate_hold_candidate(now_ms, config, effects);
            }
            Some(HoldTapFlavor::Balanced) => {
                candidate.interrupted = true;
            }
            Some(HoldTapFlavor::TapPreferred) | None => {}
        }
    }

    fn interrupt_hold_candidate_on_up(
        &mut self,
        id: i32,
        now_ms: u64,
        config: &Config,
        effects: &mut Vec<EngineEffect>,
    ) {
        let should_activate = self.hold_candidate.as_ref().is_some_and(|candidate| {
            candidate.id != id
                && candidate.flavor == Some(HoldTapFlavor::Balanced)
                && candidate.interrupted
        });

        if should_activate {
            self.activate_hold_candidate(now_ms, config, effects);
        }
    }

    fn activate_hold_candidate(
        &mut self,
        now_ms: u64,
        config: &Config,
        effects: &mut Vec<EngineEffect>,
    ) -> bool {
        let Some(candidate) = self.hold_candidate.take() else {
            return false;
        };

        let Some(contact) = self.active.get_mut(&candidate.id) else {
            return false;
        };

        if contact_movement(contact) > config.tap_radius {
            return false;
        }

        contact.start_x = contact.last_x;
        contact.start_y = contact.last_y;
        contact.start_time = contact.last_time;
        self.finished.clear();
        self.last_tap = None;

        match candidate.action {
            GestureAction::HoldRepeat {
                sequence,
                start_ms,
                interval_ms,
                translation,
                route,
            } => {
                self.start_hold_repeat(
                    HoldRepeatStart {
                        hold_id: candidate.id,
                        now_ms,
                        sequence,
                        start_ms,
                        interval_ms,
                        translation,
                        route,
                    },
                    config,
                    effects,
                );
            }
            action => {
                self.perform_action(action, effects, config, Some(candidate.id));
            }
        }

        self.max_active = self.active_non_hold_count().max(1);
        true
    }

    fn start_hold_repeat(
        &mut self,
        repeat: HoldRepeatStart,
        config: &Config,
        effects: &mut Vec<EngineEffect>,
    ) {
        let start_ms = repeat.start_ms.unwrap_or(config.repeat_start_ms);
        let interval_ms = repeat
            .interval_ms
            .unwrap_or(config.repeat_interval_ms)
            .max(1);
        if repeat.translation.is_some() || repeat.route.is_some() {
            effects.push(EngineEffect::Dispatch(
                GestureAction::KeySequenceWithOptions {
                    sequence: repeat.sequence.clone(),
                    translation: repeat.translation,
                    route: repeat.route,
                },
            ));
        } else {
            effects.push(EngineEffect::Dispatch(GestureAction::KeySequence(
                repeat.sequence.clone(),
            )));
        }

        self.repeaters
            .retain(|repeater| repeater.hold_id != repeat.hold_id);
        self.repeaters.push(RepeatState {
            hold_id: repeat.hold_id,
            next_ms: repeat.now_ms + u64::from(start_ms),
            interval_ms,
            sequence: repeat.sequence,
            translation: repeat.translation,
            route: repeat.route,
        });
    }

    fn perform_dispatch_action(
        &mut self,
        action: GestureAction,
        effects: &mut Vec<EngineEffect>,
        hold_id: Option<i32>,
    ) {
        if let Some(hold_id) = hold_id {
            self.remember_held_action(hold_id);
            effects.push(EngineEffect::Press { hold_id, action });
        } else {
            effects.push(EngineEffect::Dispatch(action));
        }
    }

    fn remember_held_action_if_needed(&mut self, hold_id: Option<i32>) {
        if let Some(hold_id) = hold_id {
            self.remember_held_action(hold_id);
        }
    }

    fn remember_held_action(&mut self, hold_id: i32) {
        if !self.held_actions.iter().any(|held| held.hold_id == hold_id) {
            self.held_actions.push(HeldActionState { hold_id });
        }
    }

    fn should_consume_sticky_layer(&self, action: &GestureAction, hold_id: Option<i32>) -> bool {
        if hold_id.is_some() || self.sticky_layer.is_none() {
            return false;
        }

        !matches!(
            action,
            GestureAction::LayerSticky { .. }
                | GestureAction::LayerSet(_)
                | GestureAction::LayerToggle(_)
                | GestureAction::LayerMomentary(_)
                | GestureAction::ModeSet(_)
                | GestureAction::ModeToggle(_)
                | GestureAction::ModeMomentary(_)
                | GestureAction::None
        )
    }

    fn activate_sticky_layer(
        &mut self,
        layer: Layer,
        timeout_ms: Option<u32>,
        effects: &mut Vec<EngineEffect>,
    ) {
        let timeout_ms = u64::from(timeout_ms.unwrap_or(1000));
        self.clear_sticky_layer(effects);
        self.push_layer(layer.clone(), effects);
        self.sticky_layer = Some(StickyLayerState {
            layer,
            expires_at_ms: self.now_ms + timeout_ms,
        });
    }

    fn clear_sticky_layer(&mut self, effects: &mut Vec<EngineEffect>) {
        let Some(sticky) = self.sticky_layer.take() else {
            return;
        };

        self.layer_stack
            .retain(|existing| existing != &sticky.layer);
        if self.layer_stack.is_empty() {
            self.layer_stack.push(Layer::base());
        }
        effects.push(EngineEffect::Redraw);
    }

    fn start_momentary(
        &mut self,
        hold_id: i32,
        mode: Option<Mode>,
        layer: Option<Layer>,
        effects: &mut Vec<EngineEffect>,
        config: &Config,
    ) {
        self.clear_sticky_layer(effects);
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
            eprintln!("touchdeck: layer {}", layer_name(&layer));
            self.push_layer(layer, effects);
        }

        self.last_tap = None;
        self.armed_drag = None;
        effects.push(EngineEffect::SetCapture(self.capture_policy(config)));
        effects.push(EngineEffect::Redraw);
    }

    fn return_from_momentary(&mut self, effects: &mut Vec<EngineEffect>, config: &Config) {
        let Some(momentary) = self.momentary.take() else {
            return;
        };

        self.finish_continuous_drag(effects);
        self.armed_drag = None;
        self.clear_sticky_layer(effects);
        self.mode = momentary.return_mode;
        self.layer_stack = momentary.return_layer_stack;
        self.hold_candidate = None;
        self.repeaters.clear();
        self.last_tap = None;
        eprintln!(
            "touchdeck: return mode {} layer {}",
            mode_name(self.mode),
            layer_name(&self.current_layer())
        );
        effects.push(EngineEffect::SetCapture(self.capture_policy(config)));
        effects.push(EngineEffect::Redraw);
    }

    pub(crate) fn set_mode(
        &mut self,
        mode: Mode,
        effects: &mut Vec<EngineEffect>,
        config: &Config,
    ) {
        self.clear_sticky_layer(effects);
        self.mode = mode;
        self.layer_stack = default_layer_stack_for_mode(mode);
        self.momentary = None;
        self.hold_candidate = None;
        self.repeaters.clear();
        self.armed_drag = None;
        self.last_tap = None;
        eprintln!("touchdeck: mode {}", mode_name(mode));
        effects.push(EngineEffect::SetCapture(self.capture_policy(config)));
        effects.push(EngineEffect::Redraw);
    }

    fn set_layer(&mut self, layer: Layer, effects: &mut Vec<EngineEffect>) {
        self.clear_sticky_layer(effects);
        let label = layer_name(&layer).to_string();
        self.layer_stack = if layer == Layer::base() {
            vec![Layer::base()]
        } else {
            vec![Layer::base(), layer]
        };
        self.momentary = None;
        self.hold_candidate = None;
        self.armed_drag = None;
        self.last_tap = None;
        eprintln!("touchdeck: layer {label}");
        effects.push(EngineEffect::Redraw);
    }

    fn push_layer(&mut self, layer: Layer, effects: &mut Vec<EngineEffect>) {
        if layer == Layer::base() {
            self.set_layer(Layer::base(), effects);
            return;
        }

        let label = layer_name(&layer).to_string();
        self.layer_stack.retain(|existing| existing != &layer);
        if !self.layer_stack.contains(&Layer::base()) {
            self.layer_stack.insert(0, Layer::base());
        }
        self.layer_stack.push(layer);
        self.last_tap = None;
        eprintln!("touchdeck: push layer {label}");
        effects.push(EngineEffect::Redraw);
    }

    fn pop_layer(&mut self, layer: Layer, effects: &mut Vec<EngineEffect>) {
        if layer == Layer::base() {
            self.set_layer(Layer::base(), effects);
            return;
        }

        let label = layer_name(&layer).to_string();
        self.layer_stack.retain(|existing| existing != &layer);
        if self.layer_stack.is_empty() {
            self.layer_stack.push(Layer::base());
        }
        self.last_tap = None;
        eprintln!("touchdeck: pop layer {label}");
        effects.push(EngineEffect::Redraw);
    }

    pub(crate) fn current_layer(&self) -> Layer {
        self.layer_stack.last().cloned().unwrap_or_else(Layer::base)
    }
}

impl Engine {
    fn hold_contact_ids(&self) -> Vec<i32> {
        let mut ids = self
            .held_actions
            .iter()
            .map(|held| held.hold_id)
            .collect::<Vec<_>>();
        ids.extend(self.repeaters.iter().map(|repeater| repeater.hold_id));
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

    fn active_non_hold_gesture(&self) -> Gesture {
        let hold_ids = self.hold_contact_ids();
        let finished = self
            .active
            .values()
            .filter(|contact| !hold_ids.contains(&contact.id))
            .copied()
            .collect::<Vec<_>>();

        Gesture {
            max_active: finished.len(),
            finished,
        }
    }

    fn take_finished_non_hold_gesture(&mut self) -> Gesture {
        let hold_ids = self.hold_contact_ids();
        let mut finished = Vec::new();
        self.finished.retain(|contact| {
            if hold_ids.contains(&contact.id) {
                true
            } else {
                finished.push(*contact);
                false
            }
        });
        self.max_active = self.active.len();

        Gesture {
            max_active: finished.len().max(1),
            finished,
        }
    }

    fn drag_contains(&self, id: i32) -> bool {
        self.continuous_drag
            .as_ref()
            .is_some_and(|drag| drag.contact_ids.contains(&id))
    }

    fn arm_resize(
        &mut self,
        edge: NiriResizeEdge,
        fingers: usize,
        min_px: Option<f64>,
        timeout_ms: Option<u32>,
        config: &Config,
    ) {
        let fingers = fingers.max(1);
        let min_px = min_px.unwrap_or(config.tap_radius.max(8.0));
        let timeout_ms = u64::from(timeout_ms.unwrap_or(1000));
        self.armed_drag = Some(ArmedDragState {
            edge,
            fingers,
            min_px,
            expires_at_ms: self.now_ms + timeout_ms,
        });
        self.last_action = Some(format!("resize:{}", edge.as_str()));
    }

    fn expire_armed_drag(&mut self, now_ms: u64) {
        if self
            .armed_drag
            .as_ref()
            .is_some_and(|armed| now_ms > armed.expires_at_ms)
        {
            self.armed_drag = None;
        }
    }

    fn armed_drag_matches(&self, gesture: &Gesture, armed: &ArmedDragState) -> bool {
        if gesture.max_active != armed.fingers || gesture.finished.is_empty() {
            return false;
        }

        let Some((start_x, start_y, last_x, last_y)) = gesture_centroid(gesture) else {
            return false;
        };

        (last_x - start_x).hypot(last_y - start_y) >= armed.min_px
    }

    fn start_continuous_drag(
        &mut self,
        operation: ContinuousDragOperation,
        gesture: Gesture,
        effects: &mut Vec<EngineEffect>,
    ) {
        let Some((start_x, start_y, last_x, last_y)) = gesture_centroid(&gesture) else {
            return;
        };

        let contact_ids = gesture
            .finished
            .iter()
            .map(|contact| contact.id)
            .collect::<Vec<_>>();
        self.continuous_drag = Some(ContinuousDragState {
            operation: operation.clone(),
            contact_ids,
            start_x,
            start_y,
            last_x,
            last_y,
        });
        self.last_tap = None;

        match operation {
            ContinuousDragOperation::NiriMove => {
                effects.push(EngineEffect::InteractiveMoveBegin {
                    x: start_x,
                    y: start_y,
                });
            }
            ContinuousDragOperation::NiriResize { edge } => {
                effects.push(EngineEffect::InteractiveResizeBegin { edge });
            }
        }

        let dx = last_x - start_x;
        let dy = last_y - start_y;
        if dx != 0.0 || dy != 0.0 {
            match operation {
                ContinuousDragOperation::NiriMove => {
                    effects.push(EngineEffect::InteractiveMoveUpdate {
                        x: last_x,
                        y: last_y,
                        dx,
                        dy,
                    });
                }
                ContinuousDragOperation::NiriResize { .. } => {
                    effects.push(EngineEffect::InteractiveResizeUpdate { dx, dy });
                }
            }
        }
    }

    fn update_continuous_drag(&mut self, effects: &mut Vec<EngineEffect>) {
        let Some(drag) = &self.continuous_drag else {
            return;
        };
        let operation = drag.operation.clone();
        let contact_ids = drag.contact_ids.clone();
        let start_x = drag.start_x;
        let start_y = drag.start_y;
        let last_x = drag.last_x;
        let last_y = drag.last_y;

        let contacts = contact_ids
            .iter()
            .filter_map(|id| self.active.get(id).copied())
            .collect::<Vec<_>>();
        if contacts.len() != contact_ids.len() {
            self.finish_continuous_drag(effects);
            return;
        }

        let gesture = Gesture {
            max_active: contacts.len(),
            finished: contacts,
        };
        let Some((_start_x, _start_y, x, y)) = gesture_centroid(&gesture) else {
            return;
        };
        let dx = x - last_x;
        let dy = y - last_y;

        if let Some(drag) = &mut self.continuous_drag {
            drag.last_x = x;
            drag.last_y = y;
        }

        if dx != 0.0 || dy != 0.0 {
            match operation {
                ContinuousDragOperation::NiriMove => {
                    effects.push(EngineEffect::InteractiveMoveUpdate { x, y, dx, dy });
                }
                ContinuousDragOperation::NiriResize { .. } => {
                    effects.push(EngineEffect::InteractiveResizeUpdate {
                        dx: x - start_x,
                        dy: y - start_y,
                    });
                }
            }
        }
    }

    fn finish_continuous_drag(&mut self, effects: &mut Vec<EngineEffect>) {
        if let Some(drag) = self.continuous_drag.take() {
            match drag.operation {
                ContinuousDragOperation::NiriMove => effects.push(EngineEffect::InteractiveMoveEnd),
                ContinuousDragOperation::NiriResize { .. } => {
                    effects.push(EngineEffect::InteractiveResizeEnd)
                }
            }
        }
    }

    fn release_held_actions_for(&mut self, hold_id: i32) -> Vec<EngineEffect> {
        let mut effects = Vec::new();
        let mut remaining = Vec::new();
        for held in self.held_actions.drain(..) {
            if held.hold_id == hold_id {
                effects.push(EngineEffect::Release { hold_id });
            } else {
                remaining.push(held);
            }
        }
        self.held_actions = remaining;
        effects
    }

    fn release_all_held_actions(&mut self) -> Vec<EngineEffect> {
        self.held_actions
            .drain(..)
            .map(|held| EngineEffect::Release {
                hold_id: held.hold_id,
            })
            .collect()
    }

    fn stop_repeaters_for(&mut self, hold_id: i32) {
        self.repeaters
            .retain(|repeater| repeater.hold_id != hold_id);
    }

    fn reset_contacts(&mut self) {
        self.active.clear();
        self.finished.clear();
        self.max_active = 0;
        self.hold_candidate = None;
        self.repeaters.clear();
        self.continuous_drag = None;
        self.armed_drag = None;
        self.sticky_layer = None;
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

pub(crate) fn resolve_niri_gesture(
    gesture: &Gesture,
    config: &Config,
    size: SurfaceSize,
) -> GestureAction {
    if gesture.finished.is_empty() {
        return GestureAction::None;
    }

    let min_dim = f64::from(size.width.min(size.height).max(1));
    let swipe_threshold_min = config.swipe_threshold_min.min(config.swipe_threshold_max);
    let swipe_threshold_max = config.swipe_threshold_min.max(config.swipe_threshold_max);
    let swipe_threshold =
        (min_dim * config.swipe_threshold_ratio).clamp(swipe_threshold_min, swipe_threshold_max);

    if gesture.max_active == 2 && is_tap_like(gesture, config.tap_radius, config.two_finger_tap_ms)
    {
        return niri_action(config.action_two_finger_tap.clone());
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
            niri_action(config.action_swipe_left.clone())
        } else {
            niri_action(config.action_swipe_right.clone())
        }
    } else if abs_dy >= abs_dx * 1.25 {
        if dy < 0.0 {
            niri_action(config.action_swipe_up.clone())
        } else {
            niri_action(config.action_swipe_down.clone())
        }
    } else {
        GestureAction::None
    }
}

pub(crate) fn is_exit_gesture(gesture: &Gesture, config: &Config, size: SurfaceSize) -> bool {
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

fn niri_action(action: Option<NiriCommand>) -> GestureAction {
    action
        .map(GestureAction::Niri)
        .unwrap_or(GestureAction::None)
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

#[cfg(test)]
mod tests {

    use super::*;
    use crate::config::{
        expand_keyboard_maps, parse_action_steps, BehaviorRegistry, Config, FileConfig,
        InputConfig, TextOutputBackend, TextOutputConfig, TouchInputBackend,
    };
    use crate::key::*;
    use crate::keymap::{Behavior, Binding, HoldTapFlavor, Keymap, MacroRegistry, Trigger};
    use crate::layout::{SlotRegistry, SlotTarget};
    use crate::mode::{Layer, Mode};

    fn niri(action: &str) -> NiriCommand {
        crate::action::parse_niri_action(action).unwrap()
    }

    fn test_config() -> Config {
        let mut config = Config {
            input: InputConfig {
                touch_backend: TouchInputBackend::Wayland,
                evdev_touch_device: None,
                evdev_device_name_contains: None,
                sunshine_output: None,
                sunshine_router_socket: std::path::PathBuf::from("/tmp/touchdeck-test.sock"),
                evdev_grab: true,
            },
            action_swipe_left: Some(niri("focus-workspace-down")),
            action_swipe_right: Some(niri("focus-workspace-up")),
            action_swipe_up: Some(niri("focus-column-right")),
            action_swipe_down: Some(niri("focus-column-left")),
            action_two_finger_tap: Some(niri("toggle-overview")),
            tap_radius: 48.0,
            two_finger_tap_ms: 350,
            exit_tap_ms: 450,
            hold_ms: 180,
            repeat_start_ms: 360,
            repeat_interval_ms: 45,
            double_tap_ms: 280,
            swipe_threshold_ratio: 0.08,
            swipe_threshold_min: 64.0,
            swipe_threshold_max: 140.0,
            debug_alpha: 0,
            debug_draw: false,
            mode_hint_ms: 700,
            modifier_tap_ms: 40,
            log_touch: false,
            record_trace_path: None,
            xkb_keymap_path: None,
            text_output: TextOutputConfig {
                backend: TextOutputBackend::VirtualKeyboard,
            },
            slots: test_slots(),
            keymap: Keymap::default(),
            macros: MacroRegistry::default(),
            exit_corner_enabled: true,
            exit_corner_ratio: 0.12,
            exit_corner_tap_ms: 350,
        };
        apply_example_keymap(&mut config);
        config
    }

    fn test_size() -> SurfaceSize {
        SurfaceSize {
            width: 1000,
            height: 2400,
        }
    }

    fn test_slots() -> SlotRegistry {
        SlotRegistry::from_svg_str(include_str!("../layouts/phone-portrait.svg")).unwrap()
    }

    fn test_target(name: &str) -> SlotTarget {
        test_slots().get(name).unwrap()
    }

    fn test_slot_center(name: &str) -> (f64, f64) {
        let rect = test_target(name).rect.to_px(test_size());
        (
            f64::from(rect.x) + f64::from(rect.w) / 2.0,
            f64::from(rect.y) + f64::from(rect.h) / 2.0,
        )
    }

    fn apply_example_keymap(config: &mut Config) {
        let mut file_config: FileConfig =
            toml::from_str(include_str!("../touchdeck.example.toml")).unwrap();

        if let Some(macros) = file_config.macros.take() {
            config.macros.clear();
            for (name, macro_config) in macros {
                config
                    .macros
                    .insert(&name, parse_action_steps(macro_config.steps).unwrap());
            }
        }

        let mut behavior_registry = BehaviorRegistry::default();
        if let Some(behaviors) = file_config.behaviors.take() {
            behavior_registry.extend(behaviors);
        }
        if let Some(keyboard) = &file_config.keyboard {
            if let Some(behaviors) = &keyboard.behaviors {
                behavior_registry.extend(behaviors.clone());
            }
        }

        config.keymap.bindings.clear();
        if let Some(bindings) = file_config.bindings.take() {
            for binding in bindings {
                config.keymap.bindings.push(
                    Binding::from_file_config(
                        binding,
                        &config.slots,
                        &config.macros,
                        &behavior_registry,
                    )
                    .unwrap(),
                );
            }
        }

        if let Some(keyboard) = file_config.keyboard {
            if let Some(maps) = keyboard.layers {
                config.keymap.bindings.extend(
                    expand_keyboard_maps(maps, &config.slots, &config.macros, &behavior_registry)
                        .unwrap(),
                );
            }
        }
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

    fn sample(now_ms: u64, time: u32, id: i32, x: f64, y: f64) -> TouchSample {
        TouchSample {
            now_ms,
            time,
            id,
            x,
            y,
        }
    }

    fn handle_down(
        engine: &mut Engine,
        sample: TouchSample,
        config: &Config,
        size: SurfaceSize,
    ) -> Vec<EngineEffect> {
        engine.handle_down(sample, config, size)
    }

    fn handle_motion(
        engine: &mut Engine,
        sample: TouchSample,
        config: &Config,
        size: SurfaceSize,
    ) -> Vec<EngineEffect> {
        engine.handle_motion(sample, config, size)
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
    fn one_finger_swipe_down_maps_to_focus_column_left() {
        let config = test_config();
        let gesture = gesture(1, vec![contact(500.0, 900.0, 500.0, 1200.0)]);

        assert_eq!(
            resolve_niri_gesture(&gesture, &config, test_size()),
            GestureAction::Niri(niri("focus-column-left"))
        );
    }

    #[test]
    fn one_finger_swipe_up_maps_to_focus_column_right() {
        let config = test_config();
        let gesture = gesture(1, vec![contact(500.0, 1200.0, 500.0, 900.0)]);

        assert_eq!(
            resolve_niri_gesture(&gesture, &config, test_size()),
            GestureAction::Niri(niri("focus-column-right"))
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
            GestureAction::Niri(niri("toggle-overview"))
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
        let (x, y) = test_slot_center("bottom_edge");

        handle_down(&mut engine, sample(0, 0, 1, x, y), &config, size);
        handle_motion(&mut engine, sample(80, 80, 1, x, y - 300.0), &config, size);
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
        let (x, y) = test_slot_center("key_q");

        handle_down(&mut engine, sample(0, 0, 1, x, y), &config, size);
        let effects = engine.handle_up(80, 80, 1, &config, size);

        assert!(
            dispatched_actions(&effects).contains(&GestureAction::KeySequence(vec![KeyChord {
                keys: vec![KEY_Q]
            },]))
        );
    }

    #[test]
    fn default_text_keyboard_swipe_up_sends_symbol_key() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        let mut effects = Vec::new();
        engine.set_mode(Mode::Text, &mut effects, &config);
        let (x, y) = test_slot_center("key_n1");

        handle_down(&mut engine, sample(0, 0, 1, x, y), &config, size);
        handle_motion(&mut engine, sample(60, 60, 1, x, y - 220.0), &config, size);
        let effects = engine.handle_up(80, 80, 1, &config, size);

        assert!(
            dispatched_actions(&effects).contains(&GestureAction::KeySequence(vec![KeyChord {
                keys: vec![KEY_LEFTSHIFT, KEY_1]
            },]))
        );
    }

    #[test]
    fn default_text_keyboard_home_row_swipe_sends_arrow_key() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        let mut effects = Vec::new();
        engine.set_mode(Mode::Text, &mut effects, &config);
        let (x, y) = test_slot_center("key_h");

        handle_down(&mut engine, sample(0, 0, 1, x, y), &config, size);
        let mut effects =
            handle_motion(&mut engine, sample(60, 60, 1, x - 220.0, y), &config, size);
        effects.extend(engine.handle_up(80, 80, 1, &config, size));

        assert!(
            dispatched_actions(&effects).contains(&GestureAction::KeySequence(vec![KeyChord {
                keys: vec![KEY_LEFT]
            },]))
        );
    }

    #[test]
    fn top_layer_binding_overrides_base_layer_binding() {
        let mut config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        let (x, y) = test_slot_center("left_bottom");
        config.keymap.bindings = vec![
            Binding {
                mode: Mode::Base,
                layer: Layer::base(),
                trigger: Trigger::Tap {
                    target: test_target("left_bottom"),
                    fingers: 1,
                    max_ms: None,
                },
                behavior: Behavior::KeySequence(vec![KeyChord {
                    keys: vec![KEY_SPACE],
                }]),
                priority: 0,
                consume: true,
            },
            Binding {
                mode: Mode::Base,
                layer: Layer::niri(),
                trigger: Trigger::Tap {
                    target: test_target("left_bottom"),
                    fingers: 1,
                    max_ms: None,
                },
                behavior: Behavior::KeySequence(vec![KeyChord {
                    keys: vec![KEY_ENTER],
                }]),
                priority: 0,
                consume: true,
            },
        ];

        engine.perform_action(
            GestureAction::LayerToggle(Layer::niri()),
            &mut Vec::new(),
            &config,
            None,
        );
        handle_down(&mut engine, sample(0, 0, 1, x, y), &config, size);
        let effects = engine.handle_up(80, 80, 1, &config, size);

        assert!(
            dispatched_actions(&effects).contains(&GestureAction::KeySequence(vec![KeyChord {
                keys: vec![KEY_ENTER]
            }]))
        );
        assert!(
            !dispatched_actions(&effects).contains(&GestureAction::KeySequence(vec![KeyChord {
                keys: vec![KEY_SPACE]
            }]))
        );
    }

    #[test]
    fn transparent_top_layer_falls_through_to_base_layer() {
        let mut config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        let (x, y) = test_slot_center("left_bottom");
        config.keymap.bindings = vec![
            Binding {
                mode: Mode::Base,
                layer: Layer::base(),
                trigger: Trigger::Tap {
                    target: test_target("left_bottom"),
                    fingers: 1,
                    max_ms: None,
                },
                behavior: Behavior::KeySequence(vec![KeyChord {
                    keys: vec![KEY_SPACE],
                }]),
                priority: 0,
                consume: true,
            },
            Binding {
                mode: Mode::Base,
                layer: Layer::niri(),
                trigger: Trigger::Tap {
                    target: test_target("left_bottom"),
                    fingers: 1,
                    max_ms: None,
                },
                behavior: Behavior::Transparent,
                priority: 100,
                consume: true,
            },
        ];

        engine.perform_action(
            GestureAction::LayerToggle(Layer::niri()),
            &mut Vec::new(),
            &config,
            None,
        );
        handle_down(&mut engine, sample(0, 0, 1, x, y), &config, size);
        let effects = engine.handle_up(80, 80, 1, &config, size);

        assert!(
            dispatched_actions(&effects).contains(&GestureAction::KeySequence(vec![KeyChord {
                keys: vec![KEY_SPACE]
            }]))
        );
    }

    #[test]
    fn hold_tap_hold_preferred_activates_hold_on_second_touch_down() {
        let mut config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        let mut effects = Vec::new();
        engine.set_mode(Mode::Text, &mut effects, &config);
        config.keymap.bindings = vec![
            Binding {
                mode: Mode::Text,
                layer: Layer::base(),
                trigger: Trigger::Tap {
                    target: test_target("thumb_super"),
                    fingers: 1,
                    max_ms: None,
                },
                behavior: Behavior::HoldTap {
                    hold: Box::new(Behavior::LayerMomentary(Layer::new("niri_bind"))),
                    tap: Box::new(Behavior::KeySequence(vec![KeyChord { keys: vec![KEY_A] }])),
                    flavor: HoldTapFlavor::HoldPreferred,
                    tapping_term_ms: Some(220),
                },
                priority: 0,
                consume: true,
            },
            Binding {
                mode: Mode::Text,
                layer: Layer::new("niri_bind"),
                trigger: Trigger::Tap {
                    target: test_target("key_q"),
                    fingers: 1,
                    max_ms: None,
                },
                behavior: Behavior::Niri(niri("toggle-overview")),
                priority: 0,
                consume: true,
            },
        ];
        let (thumb_x, thumb_y) = test_slot_center("thumb_super");
        let (q_x, q_y) = test_slot_center("key_q");

        handle_down(
            &mut engine,
            sample(0, 0, 1, thumb_x, thumb_y),
            &config,
            size,
        );
        handle_down(&mut engine, sample(80, 80, 2, q_x, q_y), &config, size);
        assert!(engine.layer_stack.contains(&Layer::new("niri_bind")));

        let effects = engine.handle_up(120, 120, 2, &config, size);
        assert!(
            dispatched_actions(&effects).contains(&GestureAction::Niri(niri("toggle-overview")))
        );
    }

    #[test]
    fn hold_tap_tap_unless_interrupted_stays_tap_after_tapping_term() {
        let mut config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        let mut effects = Vec::new();
        engine.set_mode(Mode::Text, &mut effects, &config);
        config.keymap.bindings = vec![Binding {
            mode: Mode::Text,
            layer: Layer::base(),
            trigger: Trigger::Tap {
                target: test_target("thumb_super"),
                fingers: 1,
                max_ms: None,
            },
            behavior: Behavior::HoldTap {
                hold: Box::new(Behavior::LayerMomentary(Layer::new("niri_bind"))),
                tap: Box::new(Behavior::KeySequence(vec![KeyChord { keys: vec![KEY_A] }])),
                flavor: HoldTapFlavor::TapUnlessInterrupted,
                tapping_term_ms: Some(120),
            },
            priority: 0,
            consume: true,
        }];
        let (thumb_x, thumb_y) = test_slot_center("thumb_super");

        handle_down(
            &mut engine,
            sample(0, 0, 1, thumb_x, thumb_y),
            &config,
            size,
        );
        engine.process_timers(160, &config, size);
        let effects = engine.handle_up(220, 220, 1, &config, size);

        assert!(
            dispatched_actions(&effects).contains(&GestureAction::KeySequence(vec![KeyChord {
                keys: vec![KEY_A]
            }]))
        );
        assert!(!engine.layer_stack.contains(&Layer::new("niri_bind")));
    }

    #[test]
    fn layer_toggle_action_switches_layer() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        let mut effects = Vec::new();

        engine.perform_action(
            GestureAction::LayerToggle(Layer::niri()),
            &mut effects,
            &config,
            None,
        );
        assert_eq!(engine.current_layer(), Layer::niri());
        assert_eq!(engine.layer_stack, vec![Layer::base(), Layer::niri()]);

        engine.perform_action(
            GestureAction::LayerToggle(Layer::niri()),
            &mut effects,
            &config,
            None,
        );
        assert_eq!(engine.current_layer(), Layer::base());
        assert_eq!(engine.layer_stack, vec![Layer::base()]);

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
        let (x, y) = test_slot_center("left_bottom");
        config.keymap.bindings = vec![Binding {
            mode: Mode::Base,
            layer: Layer::base(),
            trigger: Trigger::Hold {
                target: test_target("left_bottom"),
                fingers: 1,
                min_ms: None,
            },
            behavior: Behavior::LayerMomentary(Layer::niri()),
            priority: 0,
            consume: true,
        }];

        handle_down(&mut engine, sample(0, 0, 1, x, y), &config, size);
        engine.process_timers(181, &config, size);
        assert_eq!(engine.current_layer(), Layer::niri());
        assert_eq!(engine.layer_stack, vec![Layer::base(), Layer::niri()]);

        engine.handle_up(220, 220, 1, &config, size);
        assert_eq!(engine.current_layer(), Layer::base());
        assert_eq!(engine.layer_stack, vec![Layer::base()]);
    }

    #[test]
    fn left_bottom_hold_enters_momentary_and_release_returns_base() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        let (x, y) = test_slot_center("left_bottom");

        handle_down(&mut engine, sample(0, 0, 1, x, y), &config, size);
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
        let (x, y) = test_slot_center("left_bottom");

        handle_down(&mut engine, sample(0, 0, 1, x, y), &config, size);
        engine.handle_up(80, 80, 1, &config, size);
        handle_down(
            &mut engine,
            sample(160, 160, 1, x + 4.0, y + 4.0),
            &config,
            size,
        );
        let effects = engine.handle_up(220, 220, 1, &config, size);

        assert_eq!(engine.mode, Mode::NiriLocked);
        assert!(effects.contains(&EngineEffect::SetCapture(CapturePolicy::Fullscreen)));

        handle_down(&mut engine, sample(400, 400, 1, x, y), &config, size);
        engine.handle_up(460, 460, 1, &config, size);
        handle_down(
            &mut engine,
            sample(540, 540, 1, x + 2.0, y + 2.0),
            &config,
            size,
        );
        let effects = engine.handle_up(600, 600, 1, &config, size);

        assert_eq!(engine.mode, Mode::Base);
        assert!(effects.contains(&EngineEffect::SetCapture(CapturePolicy::Fullscreen)));
    }

    #[test]
    fn bottom_edge_double_tap_enters_and_exits_passthrough() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        let (x, y) = test_slot_center("bottom_edge");

        handle_down(&mut engine, sample(0, 0, 1, x, y), &config, size);
        engine.handle_up(60, 60, 1, &config, size);
        handle_down(
            &mut engine,
            sample(140, 140, 1, x + 4.0, y + 2.0),
            &config,
            size,
        );
        let effects = engine.handle_up(200, 200, 1, &config, size);

        assert_eq!(engine.mode, Mode::Passthrough);
        assert!(matches!(
            effects.as_slice(),
            [
                ..,
                EngineEffect::SetCapture(CapturePolicy::Zones(_)),
                EngineEffect::Redraw
            ]
        ));
        let CapturePolicy::Zones(rects) = engine.capture_policy(&config) else {
            panic!("passthrough should use zoned capture");
        };
        assert!(rects.contains(&test_target("left_bottom").rect));
        assert!(rects.contains(&test_target("bottom_edge").rect));
        assert!(rects.contains(&test_target("top_left").rect));
        assert!(!rects.contains(&test_target("center").rect));

        handle_down(&mut engine, sample(380, 380, 1, x, y), &config, size);
        engine.handle_up(430, 430, 1, &config, size);
        handle_down(
            &mut engine,
            sample(500, 500, 1, x + 4.0, y + 2.0),
            &config,
            size,
        );
        let effects = engine.handle_up(550, 550, 1, &config, size);

        assert_eq!(engine.mode, Mode::Base);
        assert!(effects.contains(&EngineEffect::SetCapture(CapturePolicy::Fullscreen)));
    }

    #[test]
    fn passthrough_hold_returns_to_passthrough_after_momentary_niri() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        let (bottom_x, bottom_y) = test_slot_center("bottom_edge");
        let (left_x, left_y) = test_slot_center("left_bottom");

        handle_down(
            &mut engine,
            sample(0, 0, 1, bottom_x, bottom_y),
            &config,
            size,
        );
        engine.handle_up(60, 60, 1, &config, size);
        handle_down(
            &mut engine,
            sample(140, 140, 1, bottom_x + 4.0, bottom_y + 2.0),
            &config,
            size,
        );
        engine.handle_up(200, 200, 1, &config, size);
        assert_eq!(engine.mode, Mode::Passthrough);

        handle_down(
            &mut engine,
            sample(300, 300, 1, left_x, left_y),
            &config,
            size,
        );
        engine.process_timers(481, &config, size);
        assert_eq!(engine.mode, Mode::NiriMomentary);

        let effects = engine.handle_up(520, 520, 1, &config, size);
        assert_eq!(engine.mode, Mode::Passthrough);
        assert!(matches!(
            effects.as_slice(),
            [
                ..,
                EngineEffect::SetCapture(CapturePolicy::Zones(_)),
                EngineEffect::Redraw
            ]
        ));
    }

    #[test]
    fn replay_hold_then_same_finger_swipe_dispatches_niri_action() {
        let config = test_config();
        let trace = r#"
    {"type":"down","t":0,"wl_time":0,"id":1,"x":90.0,"y":2184.0}
    {"type":"motion","t":220,"wl_time":220,"id":1,"x":90.0,"y":1880.0}
    {"type":"up","t":260,"wl_time":260,"id":1}
    "#;

        let effects = run_trace(trace, &config);
        assert!(
            dispatched_actions(&effects).contains(&GestureAction::Niri(niri("focus-column-right")))
        );
    }

    #[test]
    fn replay_hold_plus_second_finger_swipe_dispatches_niri_action() {
        let config = test_config();
        let trace = r#"
    {"type":"down","t":0,"wl_time":0,"id":1,"x":90.0,"y":2184.0}
    {"type":"down","t":220,"wl_time":220,"id":2,"x":800.0,"y":900.0}
    {"type":"motion","t":240,"wl_time":240,"id":2,"x":800.0,"y":1200.0}
    {"type":"up","t":260,"wl_time":260,"id":2}
    {"type":"up","t":300,"wl_time":300,"id":1}
    "#;

        let effects = run_trace(trace, &config);
        assert!(
            dispatched_actions(&effects).contains(&GestureAction::Niri(niri("focus-column-left")))
        );
    }
}
