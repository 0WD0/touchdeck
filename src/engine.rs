use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::action::NiriAction;
use crate::config::{Config, KeyRoute, KeyTranslationPolicy};
use crate::geometry::{RectNorm, SurfaceSize};
use crate::gesture::{contact_movement, is_tap_like, Contact, Gesture, TapRecord};
use crate::key::KeyChord;
use crate::keymap::GestureAction;
use crate::mode::{
    default_layer_stack_for_mode, layer_name, mode_name, Layer, Mode,
};


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
    last_tap: Option<TapRecord>,
    pub(crate) last_action: Option<String>,
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
            held_actions: Vec::new(),
            repeaters: Vec::new(),
            last_tap: None,
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
    Redraw,
}


impl Engine {
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
        let hold_deadline = self
            .hold_candidate
            .as_ref()
            .map(|candidate| candidate.deadline_ms);
        self.repeaters.iter().map(|repeater| repeater.next_ms).fold(
            hold_deadline,
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
        let mut effects = Vec::new();

        if let Some(candidate) = self.hold_candidate.clone() {
            if now_ms >= candidate.deadline_ms {
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

                match action {
                    GestureAction::HoldRepeat {
                        sequence,
                        start_ms,
                        interval_ms,
                        translation,
                        route,
                    } => {
                        self.start_hold_repeat(
                            candidate.id,
                            now_ms,
                            sequence,
                            start_ms,
                            interval_ms,
                            translation,
                            route,
                            config,
                            &mut effects,
                        );
                    }
                    action => {
                        self.perform_action(action, &mut effects, config, Some(candidate.id));
                    }
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

        if let Some((action, min_ms)) = config.keymap.resolve_hold(
            self.mode,
            &self.layer_stack,
            size,
            x,
            y,
            config.hold_ms,
            config.repeat_start_ms,
        ) {
            self.hold_candidate = Some(HoldCandidate {
                id,
                deadline_ms: now_ms + u64::from(min_ms),
                action,
            });
        }

        redraw_if_debug(config)
    }

    pub(crate) fn handle_motion(
        &mut self,
        now_ms: u64,
        id: i32,
        time: u32,
        x: f64,
        y: f64,
        config: &Config,
        size: SurfaceSize,
    ) -> Vec<EngineEffect> {
        let mut action = GestureAction::None;
        let mut moved_contact = None;

        if let Some(contact) = self.active.get_mut(&id) {
            contact.last_x = x;
            contact.last_y = y;
            contact.last_time = time;

            if let Some(candidate) = &self.hold_candidate {
                if candidate.id == id && contact_movement(contact) > config.tap_radius {
                    self.hold_candidate = None;
                }
            }

            moved_contact = Some(*contact);
        }

        if let Some(contact) = moved_contact {
            if !self.hold_contact_ids().contains(&id) && self.active_non_hold_count() == 1 {
                action = config.keymap.resolve_active_swipe(
                    self.mode,
                    &self.layer_stack,
                    &contact,
                    config,
                    size,
                );
            }
        }

        let mut effects = Vec::new();
        if action != GestureAction::None {
            if self
                .hold_candidate
                .as_ref()
                .is_some_and(|candidate| candidate.id == id)
            {
                self.hold_candidate = None;
            }
            self.last_tap = None;
            self.start_active_action(id, now_ms, action, config, &mut effects);
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
        let mut held_action_effects = self.release_held_actions_for(id);
        self.stop_repeaters_for(id);
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
            } => self.handle_down(t, wl_time, id, x, y, config, size),
            TraceEvent::Motion {
                t,
                wl_time,
                id,
                x,
                y,
            } => self.handle_motion(t, id, wl_time, x, y, config, size),
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

    pub(crate) fn perform_action(
        &mut self,
        action: GestureAction,
        effects: &mut Vec<EngineEffect>,
        config: &Config,
        hold_id: Option<i32>,
    ) {
        match action {
            GestureAction::Niri(_)
            | GestureAction::KeySequence(_)
            | GestureAction::KeySequenceWithOptions { .. }
            | GestureAction::ModMorph { .. }
            | GestureAction::KeyRepeat
            | GestureAction::HoldRepeat { .. }
            | GestureAction::Exit => self.perform_dispatch_action(action, effects, hold_id),
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
            GestureAction::None => {}
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
                hold_id,
                now_ms,
                sequence,
                start_ms,
                interval_ms,
                translation,
                route,
                config,
                effects,
            ),
            action => self.perform_action(action, effects, config, Some(hold_id)),
        }
    }

    fn start_hold_repeat(
        &mut self,
        hold_id: i32,
        now_ms: u64,
        sequence: Vec<KeyChord>,
        start_ms: Option<u32>,
        interval_ms: Option<u32>,
        translation: Option<KeyTranslationPolicy>,
        route: Option<KeyRoute>,
        config: &Config,
        effects: &mut Vec<EngineEffect>,
    ) {
        let start_ms = start_ms.unwrap_or(config.repeat_start_ms);
        let interval_ms = interval_ms.unwrap_or(config.repeat_interval_ms).max(1);
        if translation.is_some() || route.is_some() {
            effects.push(EngineEffect::Dispatch(
                GestureAction::KeySequenceWithOptions {
                    sequence: sequence.clone(),
                    translation,
                    route,
                },
            ));
        } else {
            effects.push(EngineEffect::Dispatch(GestureAction::KeySequence(
                sequence.clone(),
            )));
        }

        self.repeaters
            .retain(|repeater| repeater.hold_id != hold_id);
        self.repeaters.push(RepeatState {
            hold_id,
            next_ms: now_ms + u64::from(start_ms),
            interval_ms,
            sequence,
            translation,
            route,
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
        self.repeaters.clear();
        self.last_tap = None;
        eprintln!(
            "touchdeck: return mode {} layer {}",
            mode_name(self.mode),
            layer_name(self.current_layer())
        );
        effects.push(EngineEffect::SetCapture(self.capture_policy(config)));
        effects.push(EngineEffect::Redraw);
    }

    pub(crate) fn set_mode(&mut self, mode: Mode, effects: &mut Vec<EngineEffect>, config: &Config) {
        self.mode = mode;
        self.layer_stack = default_layer_stack_for_mode(mode);
        self.momentary = None;
        self.hold_candidate = None;
        self.repeaters.clear();
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

    pub(crate) fn current_layer(&self) -> Layer {
        self.layer_stack.last().copied().unwrap_or(Layer::Base)
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

pub(crate) fn resolve_niri_gesture(gesture: &Gesture, config: &Config, size: SurfaceSize) -> GestureAction {
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

fn niri_action(action: Option<NiriAction>) -> GestureAction {
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
    use crate::config::{default_ime_socket_path, expand_keyboard_maps, parse_action_steps, BehaviorRegistry, Config, FileConfig, TextOutputBackend, TextOutputConfig};
    use crate::key::*;
    use crate::keymap::{Behavior, Binding, Keymap, MacroRegistry, Trigger};
    use crate::layout::{SlotRegistry, SlotTarget};
    use crate::mode::{Layer, Mode};

    fn test_config() -> Config {
        let mut config = Config {
            action_swipe_left: Some(NiriAction::FocusWorkspaceDown),
            action_swipe_right: Some(NiriAction::FocusWorkspaceUp),
            action_swipe_up: Some(NiriAction::FocusColumnRight),
            action_swipe_down: Some(NiriAction::FocusColumnLeft),
            action_two_finger_tap: Some(NiriAction::ToggleOverview),
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
                ime_socket: default_ime_socket_path(),
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
                config
                    .keymap
                    .bindings
                    .push(
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
                config
                    .keymap
                    .bindings
                    .extend(
                        expand_keyboard_maps(
                            maps,
                            &config.slots,
                            &config.macros,
                            &behavior_registry,
                        )
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
            GestureAction::Niri(NiriAction::FocusColumnLeft)
        );
    }

    #[test]
    fn one_finger_swipe_up_maps_to_focus_column_right() {
        let config = test_config();
        let gesture = gesture(1, vec![contact(500.0, 1200.0, 500.0, 900.0)]);

        assert_eq!(
            resolve_niri_gesture(&gesture, &config, test_size()),
            GestureAction::Niri(NiriAction::FocusColumnRight)
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
        let (x, y) = test_slot_center("bottom_edge");

        engine.handle_down(0, 0, 1, x, y, &config, size);
        engine.handle_motion(80, 1, 80, x, y - 300.0, &config, size);
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

        engine.handle_down(0, 0, 1, x, y, &config, size);
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

        engine.handle_down(0, 0, 1, x, y, &config, size);
        engine.handle_motion(60, 1, 60, x, y - 220.0, &config, size);
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

        engine.handle_down(0, 0, 1, x, y, &config, size);
        let mut effects = engine.handle_motion(60, 1, 60, x - 220.0, y, &config, size);
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
                layer: Layer::Base,
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
                layer: Layer::Niri,
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
            GestureAction::LayerToggle(Layer::Niri),
            &mut Vec::new(),
            &config,
            None,
        );
        engine.handle_down(0, 0, 1, x, y, &config, size);
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
                layer: Layer::Base,
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
                layer: Layer::Niri,
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
            GestureAction::LayerToggle(Layer::Niri),
            &mut Vec::new(),
            &config,
            None,
        );
        engine.handle_down(0, 0, 1, x, y, &config, size);
        let effects = engine.handle_up(80, 80, 1, &config, size);

        assert!(
            dispatched_actions(&effects).contains(&GestureAction::KeySequence(vec![KeyChord {
                keys: vec![KEY_SPACE]
            }]))
        );
    }

    #[test]
    fn layer_toggle_action_switches_layer() {
        let config = test_config();
        let size = test_size();
        let mut engine = Engine::default();
        let mut effects = Vec::new();

        engine.perform_action(
            GestureAction::LayerToggle(Layer::Niri),
            &mut effects,
            &config,
            None,
        );
        assert_eq!(engine.current_layer(), Layer::Niri);
        assert_eq!(engine.layer_stack, vec![Layer::Base, Layer::Niri]);

        engine.perform_action(
            GestureAction::LayerToggle(Layer::Niri),
            &mut effects,
            &config,
            None,
        );
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
        let (x, y) = test_slot_center("left_bottom");
        config.keymap.bindings = vec![Binding {
            mode: Mode::Base,
            layer: Layer::Base,
            trigger: Trigger::Hold {
                target: test_target("left_bottom"),
                fingers: 1,
                min_ms: None,
            },
            behavior: Behavior::LayerMomentary(Layer::Niri),
            priority: 0,
            consume: true,
        }];

        engine.handle_down(0, 0, 1, x, y, &config, size);
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
        let (x, y) = test_slot_center("left_bottom");

        engine.handle_down(0, 0, 1, x, y, &config, size);
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

        engine.handle_down(0, 0, 1, x, y, &config, size);
        engine.handle_up(80, 80, 1, &config, size);
        engine.handle_down(160, 160, 1, x + 4.0, y + 4.0, &config, size);
        let effects = engine.handle_up(220, 220, 1, &config, size);

        assert_eq!(engine.mode, Mode::NiriLocked);
        assert!(effects.contains(&EngineEffect::SetCapture(CapturePolicy::Fullscreen)));

        engine.handle_down(400, 400, 1, x, y, &config, size);
        engine.handle_up(460, 460, 1, &config, size);
        engine.handle_down(540, 540, 1, x + 2.0, y + 2.0, &config, size);
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

        engine.handle_down(0, 0, 1, x, y, &config, size);
        engine.handle_up(60, 60, 1, &config, size);
        engine.handle_down(140, 140, 1, x + 4.0, y + 2.0, &config, size);
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

        engine.handle_down(380, 380, 1, x, y, &config, size);
        engine.handle_up(430, 430, 1, &config, size);
        engine.handle_down(500, 500, 1, x + 4.0, y + 2.0, &config, size);
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

        engine.handle_down(0, 0, 1, bottom_x, bottom_y, &config, size);
        engine.handle_up(60, 60, 1, &config, size);
        engine.handle_down(140, 140, 1, bottom_x + 4.0, bottom_y + 2.0, &config, size);
        engine.handle_up(200, 200, 1, &config, size);
        assert_eq!(engine.mode, Mode::Passthrough);

        engine.handle_down(300, 300, 1, left_x, left_y, &config, size);
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
        assert!(dispatched_actions(&effects)
            .contains(&GestureAction::Niri(NiriAction::FocusColumnRight)));
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
        assert!(dispatched_actions(&effects)
            .contains(&GestureAction::Niri(NiriAction::FocusColumnLeft)));
    }
}
