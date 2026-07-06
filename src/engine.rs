use std::collections::HashMap;

use crate::action::NiriAction;
use crate::config::{Config, KeyRoute, KeyTranslationPolicy};
use crate::geometry::{RectNorm, SurfaceSize};
use crate::gesture::{contact_movement, is_tap_like, Contact, Gesture, TapRecord};
use crate::key::KeyChord;
use crate::keymap::GestureAction;
use crate::mode::{
    default_layer_stack_for_mode, layer_name, mode_name, Layer, Mode,
};

#[cfg(test)]
use crate::TraceEvent;

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

            moved_contact = Some(contact.clone());
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
