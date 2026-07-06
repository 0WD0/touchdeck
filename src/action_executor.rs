use std::collections::HashMap;
use std::fs::File;
use std::sync::mpsc::Sender;
use std::time::Duration;

use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1;

use crate::action::ActionStep;
use crate::config::{Config, KeyRoute, KeyTranslationPolicy};
use crate::key::{modifier_mask_for_key, KeyChord, XKB_MOD_SUPER};
use crate::keymap::{GestureAction, LastKeySequence};
use crate::niri_backend::spawn_niri_action;
use touchdeck::ime::TouchDeckEvent;

#[derive(Default)]
pub(crate) struct ActionExecutor {
    virtual_keyboard: Option<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1>,
    _virtual_keyboard_keymap: Option<File>,
    modifier_mask: u32,
    held_modifier_mask: u32,
    modifier_mask_stack: Vec<u32>,
    last_key_sequence: Option<LastKeySequence>,
    active_actions: HashMap<i32, PressedAction>,
    ime_event_tx: Option<Sender<TouchDeckEvent>>,
}

#[derive(Debug)]
enum PressedAction {
    None,
    Key(u32),
    ModMorph {
        masked_mods: u32,
        pressed: Box<PressedAction>,
    },
}

#[derive(Default)]
pub(crate) struct ExecutorOutcome {
    pub(crate) exit: bool,
    pub(crate) last_action: Option<String>,
}

impl ExecutorOutcome {
    fn merge(&mut self, other: Self) {
        self.exit |= other.exit;
        if other.last_action.is_some() {
            self.last_action = other.last_action;
        }
    }
}

struct ExecutionContext<'a> {
    now_ms: u64,
    config: &'a Config,
}

impl ActionExecutor {
    pub(crate) fn set_ime_event_sender(&mut self, sender: Sender<TouchDeckEvent>) {
        self.ime_event_tx = Some(sender);
    }

    pub(crate) fn set_virtual_keyboard(
        &mut self,
        keyboard: zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
        keymap_file: File,
    ) {
        self.modifier_mask = 0;
        self.held_modifier_mask = 0;
        self.modifier_mask_stack.clear();
        self.active_actions.clear();
        self.last_key_sequence = None;
        self.virtual_keyboard = Some(keyboard);
        self._virtual_keyboard_keymap = Some(keymap_file);
    }

    pub(crate) fn clear_virtual_keyboard(&mut self) {
        self.virtual_keyboard = None;
        self._virtual_keyboard_keymap = None;
        self.modifier_mask = 0;
        self.held_modifier_mask = 0;
        self.modifier_mask_stack.clear();
        self.active_actions.clear();
        self.last_key_sequence = None;
    }

    pub(crate) fn dispatch_action(
        &mut self,
        action: GestureAction,
        now_ms: u64,
        config: &Config,
    ) -> ExecutorOutcome {
        let mut ctx = ExecutionContext { now_ms, config };
        self.dispatch_action_inner(action, &mut ctx)
    }

    pub(crate) fn press_action(
        &mut self,
        hold_id: i32,
        action: GestureAction,
        now_ms: u64,
        config: &Config,
    ) -> ExecutorOutcome {
        let mut ctx = ExecutionContext { now_ms, config };
        let (pressed, outcome) = self.press_action_inner(action, &mut ctx);
        self.active_actions.insert(hold_id, pressed);
        outcome
    }

    pub(crate) fn release_action(
        &mut self,
        hold_id: i32,
        now_ms: u64,
        config: &Config,
    ) -> ExecutorOutcome {
        let Some(pressed) = self.active_actions.remove(&hold_id) else {
            return ExecutorOutcome::default();
        };
        let mut ctx = ExecutionContext { now_ms, config };
        self.release_pressed_action(pressed, &mut ctx)
    }

    fn dispatch_action_inner(
        &mut self,
        action: GestureAction,
        ctx: &mut ExecutionContext<'_>,
    ) -> ExecutorOutcome {
        match action {
            GestureAction::Niri(action) => {
                eprintln!("touchdeck: niri action {action}");
                spawn_niri_action(action);
                ExecutorOutcome {
                    exit: false,
                    last_action: Some(action.as_str().to_string()),
                }
            }
            GestureAction::KeySequence(sequence) => {
                self.send_key_sequence(&sequence, None, None, ctx);
                ExecutorOutcome::default()
            }
            GestureAction::KeySequenceWithOptions {
                sequence,
                translation,
                route,
            } => {
                self.send_key_sequence(&sequence, translation, route, ctx);
                ExecutorOutcome::default()
            }
            GestureAction::ModMorph {
                mods,
                keep_mods,
                normal,
                morph,
            } => {
                let (pressed, mut outcome) = self.press_action_inner(
                    GestureAction::ModMorph {
                        mods,
                        keep_mods,
                        normal,
                        morph,
                    },
                    ctx,
                );
                outcome.merge(self.release_pressed_action(pressed, ctx));
                outcome
            }
            GestureAction::KeyRepeat => {
                self.repeat_last_key_sequence(ctx);
                ExecutorOutcome::default()
            }
            GestureAction::HoldRepeat {
                sequence,
                translation,
                route,
                ..
            } => {
                self.send_key_sequence(&sequence, translation, route, ctx);
                ExecutorOutcome::default()
            }
            GestureAction::KeyHold(key) => {
                self.send_key_state(key, true, ctx);
                ExecutorOutcome::default()
            }
            GestureAction::Sequence(steps) => self.run_action_steps(&steps, ctx),
            GestureAction::Exit => {
                eprintln!("touchdeck: exit gesture");
                ExecutorOutcome {
                    exit: true,
                    last_action: None,
                }
            }
            GestureAction::ModeSet(_)
            | GestureAction::ModeToggle(_)
            | GestureAction::ModeMomentary(_)
            | GestureAction::LayerSet(_)
            | GestureAction::LayerToggle(_)
            | GestureAction::LayerMomentary(_)
            | GestureAction::None => ExecutorOutcome::default(),
        }
    }

    fn press_action_inner(
        &mut self,
        action: GestureAction,
        ctx: &mut ExecutionContext<'_>,
    ) -> (PressedAction, ExecutorOutcome) {
        match action {
            GestureAction::KeyHold(key) => {
                self.send_key_state(key, true, ctx);
                if let Some(mask) = modifier_mask_for_key(key) {
                    self.held_modifier_mask |= mask;
                }
                (PressedAction::Key(key), ExecutorOutcome::default())
            }
            GestureAction::ModMorph {
                mods,
                keep_mods,
                normal,
                morph,
            } => {
                if self.held_modifier_mask & mods == 0 {
                    self.press_action_inner(*normal, ctx)
                } else {
                    let masked_mods = mods & !keep_mods;
                    self.push_modifier_mask(masked_mods, ctx);
                    let (pressed, outcome) = self.press_action_inner(*morph, ctx);
                    (
                        PressedAction::ModMorph {
                            masked_mods,
                            pressed: Box::new(pressed),
                        },
                        outcome,
                    )
                }
            }
            action => (PressedAction::None, self.dispatch_action_inner(action, ctx)),
        }
    }

    fn release_pressed_action(
        &mut self,
        pressed: PressedAction,
        ctx: &mut ExecutionContext<'_>,
    ) -> ExecutorOutcome {
        match pressed {
            PressedAction::None => ExecutorOutcome::default(),
            PressedAction::Key(key) => {
                self.send_key_state(key, false, ctx);
                if let Some(mask) = modifier_mask_for_key(key) {
                    self.held_modifier_mask &= !mask;
                    self.restore_held_modifiers(ctx);
                }
                ExecutorOutcome::default()
            }
            PressedAction::ModMorph {
                masked_mods,
                pressed,
            } => {
                let mut outcome = self.release_pressed_action(*pressed, ctx);
                self.pop_modifier_mask(masked_mods, ctx);
                outcome.merge(ExecutorOutcome::default());
                outcome
            }
        }
    }

    fn send_key(&mut self, key: u32, ctx: &mut ExecutionContext<'_>) {
        let time = ctx.now_ms.min(u64::from(u32::MAX)) as u32;
        let release_time = time.saturating_add(self.key_tap_gap_ms(key, ctx.config));
        eprintln!("touchdeck: key {key}");
        self.emit_key_output(time, key, true, None, None, ctx);
        self.emit_key_output(release_time, key, false, None, None, ctx);
        self.restore_held_modifiers(ctx);
    }

    fn send_key_sequence(
        &mut self,
        sequence: &[KeyChord],
        translation: Option<KeyTranslationPolicy>,
        route: Option<KeyRoute>,
        ctx: &mut ExecutionContext<'_>,
    ) {
        let mut time = ctx.now_ms.min(u64::from(u32::MAX)) as u32;
        eprintln!("touchdeck: key sequence {sequence:?}");
        if !sequence.is_empty() {
            self.last_key_sequence = Some(LastKeySequence {
                sequence: sequence.to_vec(),
                translation,
                route,
            });
        }
        for chord in sequence {
            for key in &chord.keys {
                self.emit_key_output(time, *key, true, translation, route, ctx);
                time = time.saturating_add(1);
            }
            if chord.keys.len() == 1 {
                time = time.saturating_add(self.key_tap_gap_ms(chord.keys[0], ctx.config));
            }
            for key in chord.keys.iter().rev() {
                self.emit_key_output(time, *key, false, translation, route, ctx);
                time = time.saturating_add(1);
            }
        }
        self.restore_held_modifiers(ctx);
    }

    fn repeat_last_key_sequence(&mut self, ctx: &mut ExecutionContext<'_>) {
        let Some(last) = self.last_key_sequence.clone() else {
            eprintln!("touchdeck: key_repeat ignored; no previous key sequence");
            return;
        };
        eprintln!("touchdeck: repeat last key sequence {:?}", last.sequence);
        self.send_key_sequence(&last.sequence, last.translation, last.route, ctx);
    }

    fn run_action_steps(
        &mut self,
        steps: &[ActionStep],
        ctx: &mut ExecutionContext<'_>,
    ) -> ExecutorOutcome {
        let mut outcome = ExecutorOutcome::default();
        for step in steps {
            match step {
                ActionStep::KeyDown(key) => self.send_key_state(*key, true, ctx),
                ActionStep::KeyUp(key) => self.send_key_state(*key, false, ctx),
                ActionStep::TapKey(key) => self.send_key(*key, ctx),
                ActionStep::KeySequence(sequence) => {
                    self.send_key_sequence(sequence, None, None, ctx)
                }
                ActionStep::Niri(action) => {
                    eprintln!("touchdeck: niri action {action}");
                    spawn_niri_action(*action);
                    outcome.last_action = Some(action.as_str().to_string());
                }
                ActionStep::DelayMs(ms) => {
                    std::thread::sleep(Duration::from_millis(u64::from(*ms)));
                    ctx.now_ms = ctx.now_ms.saturating_add(u64::from(*ms));
                }
            }
        }
        outcome
    }

    fn send_key_state(&mut self, key: u32, pressed: bool, ctx: &mut ExecutionContext<'_>) {
        let time = ctx.now_ms.min(u64::from(u32::MAX)) as u32;
        self.emit_key_output(time, key, pressed, None, None, ctx);
    }

    fn emit_key_output(
        &mut self,
        time: u32,
        key: u32,
        pressed: bool,
        translation: Option<KeyTranslationPolicy>,
        route: Option<KeyRoute>,
        ctx: &mut ExecutionContext<'_>,
    ) {
        if ctx.config.text_output.backend.uses_virtual_keyboard() && self.virtual_keyboard.is_none()
        {
            eprintln!("touchdeck: virtual keyboard unavailable; ignored key state {key}");
        }

        if ctx.config.text_output.backend.uses_virtual_keyboard() {
            if let Some(keyboard) = &self.virtual_keyboard {
                keyboard.key(time, key, if pressed { 1 } else { 0 });
            }
        }

        let Some(mask) = modifier_mask_for_key(key) else {
            self.emit_ime_key_state(time, key, pressed, translation, route, ctx);
            return;
        };

        if pressed {
            self.set_modifier_mask(self.modifier_mask | mask, ctx.config);
        } else {
            self.set_modifier_mask(self.modifier_mask & !mask, ctx.config);
        }
        self.emit_ime_key_state(time, key, pressed, translation, route, ctx);
    }

    fn set_modifier_mask(&mut self, modifier_mask: u32, config: &Config) {
        self.modifier_mask = modifier_mask;
        if config.text_output.backend.uses_virtual_keyboard() {
            if let Some(keyboard) = &self.virtual_keyboard {
                keyboard.modifiers(self.modifier_mask, 0, 0, 0);
            }
        }
    }

    fn push_modifier_mask(&mut self, masked_mods: u32, ctx: &mut ExecutionContext<'_>) {
        if masked_mods == 0 {
            return;
        }
        self.modifier_mask_stack.push(masked_mods);
        self.restore_held_modifiers(ctx);
    }

    fn pop_modifier_mask(&mut self, masked_mods: u32, ctx: &mut ExecutionContext<'_>) {
        if masked_mods == 0 {
            return;
        }
        if let Some(index) = self
            .modifier_mask_stack
            .iter()
            .rposition(|value| *value == masked_mods)
        {
            self.modifier_mask_stack.remove(index);
        }
        self.restore_held_modifiers(ctx);
    }

    fn restore_held_modifiers(&mut self, ctx: &mut ExecutionContext<'_>) {
        let masked_mods = self
            .modifier_mask_stack
            .iter()
            .copied()
            .fold(0, |mask, value| mask | value);
        self.set_modifier_mask(self.held_modifier_mask & !masked_mods, ctx.config);
    }

    fn emit_ime_key_state(
        &mut self,
        time: u32,
        key: u32,
        pressed: bool,
        translation: Option<KeyTranslationPolicy>,
        route: Option<KeyRoute>,
        ctx: &mut ExecutionContext<'_>,
    ) {
        if !ctx.config.text_output.backend.uses_ime() {
            return;
        }

        let Some(sender) = &self.ime_event_tx else {
            eprintln!("touchdeck: embedded touchdeck-ime is unavailable; ignored IME key {key}");
            return;
        };

        let effective_route = route.or_else(|| self.default_ime_route_for_key(key));
        let event = TouchDeckEvent {
            protocol: "touchdeck-ime-v1".to_string(),
            kind: "key".to_string(),
            source: "touchdeck".to_string(),
            time,
            key,
            state: if pressed { "pressed" } else { "released" }.to_string(),
            modifiers: self.modifier_mask,
            translation: translation.map(|value| value.as_str().to_string()),
            route: effective_route.map(|value| value.as_str().to_string()),
        };

        if sender.send(event).is_err() {
            eprintln!("touchdeck: embedded touchdeck-ime event channel is closed");
        }
    }

    fn key_tap_gap_ms(&self, key: u32, config: &Config) -> u32 {
        if modifier_mask_for_key(key).is_some() {
            config.modifier_tap_ms.max(1)
        } else {
            1
        }
    }

    fn default_ime_route_for_key(&self, key: u32) -> Option<KeyRoute> {
        if modifier_mask_for_key(key) == Some(XKB_MOD_SUPER)
            || self.modifier_mask & XKB_MOD_SUPER != 0
        {
            Some(KeyRoute::AppKey)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::NiriAction;
    use crate::config::{InputConfig, TextOutputBackend, TextOutputConfig, TouchInputBackend};
    use crate::key::*;
    use crate::keymap::MacroRegistry;
    use crate::layout::SlotRegistry;

    fn test_config() -> Config {
        Config {
            input: InputConfig {
                touch_backend: TouchInputBackend::Wayland,
                evdev_touch_device: None,
                evdev_device_name_contains: None,
                sunshine_output: None,
                sunshine_router_socket: std::path::PathBuf::from("/tmp/touchdeck-test.sock"),
                evdev_grab: true,
            },
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
            },
            slots: SlotRegistry::default(),
            keymap: crate::keymap::Keymap::default(),
            macros: MacroRegistry::default(),
            exit_corner_enabled: true,
            exit_corner_ratio: 0.12,
            exit_corner_tap_ms: 350,
        }
    }

    fn press_for_test(
        executor: &mut ActionExecutor,
        action: GestureAction,
        config: &Config,
    ) -> PressedAction {
        let mut ctx = ExecutionContext { now_ms: 0, config };
        let (pressed, _) = executor.press_action_inner(action, &mut ctx);
        pressed
    }

    fn dispatch_for_test(executor: &mut ActionExecutor, action: GestureAction, config: &Config) {
        let mut ctx = ExecutionContext { now_ms: 0, config };
        executor.dispatch_action_inner(action, &mut ctx);
    }

    fn release_for_test(executor: &mut ActionExecutor, pressed: PressedAction, config: &Config) {
        let mut ctx = ExecutionContext { now_ms: 0, config };
        executor.release_pressed_action(pressed, &mut ctx);
    }

    #[test]
    fn super_modifier_defaults_to_app_key_route_for_ime_backend() {
        let mut config = test_config();
        config.text_output.backend = TextOutputBackend::Ime;
        let mut executor = ActionExecutor::default();
        let (tx, rx) = std::sync::mpsc::channel();
        executor.set_ime_event_sender(tx);

        dispatch_for_test(
            &mut executor,
            GestureAction::KeySequence(vec![KeyChord {
                keys: vec![KEY_LEFTMETA],
            }]),
            &config,
        );

        let pressed = rx.recv().unwrap();
        let released = rx.recv().unwrap();
        assert_eq!(pressed.key, KEY_LEFTMETA);
        assert_eq!(pressed.route.as_deref(), Some("app-key"));
        assert_eq!(released.key, KEY_LEFTMETA);
        assert_eq!(released.route.as_deref(), Some("app-key"));
    }

    #[test]
    fn super_chords_default_to_app_key_route_for_ime_backend() {
        let mut config = test_config();
        config.text_output.backend = TextOutputBackend::Ime;
        let mut executor = ActionExecutor::default();
        let (tx, rx) = std::sync::mpsc::channel();
        executor.set_ime_event_sender(tx);

        dispatch_for_test(
            &mut executor,
            GestureAction::KeySequence(vec![KeyChord {
                keys: vec![KEY_LEFTMETA, KEY_A],
            }]),
            &config,
        );

        let events = (0..4).map(|_| rx.recv().unwrap()).collect::<Vec<_>>();
        assert_eq!(events[0].key, KEY_LEFTMETA);
        assert_eq!(events[0].route.as_deref(), Some("app-key"));
        assert_eq!(events[1].key, KEY_A);
        assert_eq!(events[1].route.as_deref(), Some("app-key"));
        assert_eq!(events[2].key, KEY_A);
        assert_eq!(events[2].route.as_deref(), Some("app-key"));
        assert_eq!(events[3].key, KEY_LEFTMETA);
        assert_eq!(events[3].route.as_deref(), Some("app-key"));
    }

    #[test]
    fn shift_modifier_stays_on_default_ime_key_route() {
        let mut config = test_config();
        config.text_output.backend = TextOutputBackend::Ime;
        let mut executor = ActionExecutor::default();
        let (tx, rx) = std::sync::mpsc::channel();
        executor.set_ime_event_sender(tx);

        dispatch_for_test(
            &mut executor,
            GestureAction::KeySequence(vec![KeyChord {
                keys: vec![KEY_LEFTSHIFT],
            }]),
            &config,
        );

        let pressed = rx.recv().unwrap();
        let released = rx.recv().unwrap();
        assert_eq!(pressed.key, KEY_LEFTSHIFT);
        assert_eq!(pressed.route, None);
        assert_eq!(released.key, KEY_LEFTSHIFT);
        assert_eq!(released.route, None);
    }

    #[test]
    fn mod_morph_hold_keeps_selected_binding_until_release() {
        let config = test_config();
        let mut executor = ActionExecutor::default();

        let shift = press_for_test(
            &mut executor,
            GestureAction::KeyHold(KEY_LEFTSHIFT),
            &config,
        );
        assert_eq!(executor.held_modifier_mask & XKB_MOD_SHIFT, XKB_MOD_SHIFT);
        assert_eq!(executor.modifier_mask & XKB_MOD_SHIFT, XKB_MOD_SHIFT);

        let morph = press_for_test(
            &mut executor,
            GestureAction::ModMorph {
                mods: XKB_MOD_SHIFT,
                keep_mods: 0,
                normal: Box::new(GestureAction::KeyHold(KEY_A)),
                morph: Box::new(GestureAction::KeyHold(KEY_B)),
            },
            &config,
        );

        let PressedAction::ModMorph {
            masked_mods,
            pressed,
        } = &morph
        else {
            panic!("expected active mod-morph state");
        };
        assert_eq!(*masked_mods, XKB_MOD_SHIFT);
        assert!(matches!(pressed.as_ref(), PressedAction::Key(KEY_B)));
        assert_eq!(executor.modifier_mask & XKB_MOD_SHIFT, 0);

        release_for_test(&mut executor, shift, &config);
        assert_eq!(executor.held_modifier_mask & XKB_MOD_SHIFT, 0);

        release_for_test(&mut executor, morph, &config);
        assert_eq!(executor.modifier_mask & XKB_MOD_SHIFT, 0);
        assert!(executor.modifier_mask_stack.is_empty());
    }

    #[test]
    fn one_shot_mod_morph_restores_held_modifiers_after_shifted_morph() {
        let config = test_config();
        let mut executor = ActionExecutor::default();

        let shift = press_for_test(
            &mut executor,
            GestureAction::KeyHold(KEY_LEFTSHIFT),
            &config,
        );
        dispatch_for_test(
            &mut executor,
            GestureAction::ModMorph {
                mods: XKB_MOD_SHIFT,
                keep_mods: 0,
                normal: Box::new(GestureAction::KeySequence(vec![KeyChord {
                    keys: vec![KEY_SLASH],
                }])),
                morph: Box::new(GestureAction::KeySequence(vec![KeyChord {
                    keys: vec![KEY_LEFTSHIFT, KEY_SLASH],
                }])),
            },
            &config,
        );

        assert_eq!(executor.held_modifier_mask & XKB_MOD_SHIFT, XKB_MOD_SHIFT);
        assert_eq!(executor.modifier_mask & XKB_MOD_SHIFT, XKB_MOD_SHIFT);
        assert!(executor.modifier_mask_stack.is_empty());

        release_for_test(&mut executor, shift, &config);
        assert_eq!(executor.modifier_mask & XKB_MOD_SHIFT, 0);
    }
}
