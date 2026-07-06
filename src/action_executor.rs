use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use touchdeck::protocol::ImeStatus;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1;

use crate::action::ActionStep;
use crate::config::{Config, KeyRoute, KeyTranslationPolicy};
use crate::key::{modifier_mask_for_key, KeyChord};
use crate::keymap::{GestureAction, LastKeySequence};
use crate::niri_backend::spawn_niri_action;

#[derive(Default)]
pub(crate) struct ActionExecutor {
    virtual_keyboard: Option<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1>,
    _virtual_keyboard_keymap: Option<File>,
    modifier_mask: u32,
    held_modifier_mask: u32,
    modifier_mask_stack: Vec<u32>,
    last_key_sequence: Option<LastKeySequence>,
    active_actions: HashMap<i32, PressedAction>,
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
    ime_status: &'a mut ImeStatus,
    ime_status_dirty: &'a mut bool,
}

impl ActionExecutor {
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
        ime_status: &mut ImeStatus,
        ime_status_dirty: &mut bool,
    ) -> ExecutorOutcome {
        let mut ctx = ExecutionContext {
            now_ms,
            config,
            ime_status,
            ime_status_dirty,
        };
        self.dispatch_action_inner(action, &mut ctx)
    }

    pub(crate) fn press_action(
        &mut self,
        hold_id: i32,
        action: GestureAction,
        now_ms: u64,
        config: &Config,
        ime_status: &mut ImeStatus,
        ime_status_dirty: &mut bool,
    ) -> ExecutorOutcome {
        let mut ctx = ExecutionContext {
            now_ms,
            config,
            ime_status,
            ime_status_dirty,
        };
        let (pressed, outcome) = self.press_action_inner(action, &mut ctx);
        self.active_actions.insert(hold_id, pressed);
        outcome
    }

    pub(crate) fn release_action(
        &mut self,
        hold_id: i32,
        now_ms: u64,
        config: &Config,
        ime_status: &mut ImeStatus,
        ime_status_dirty: &mut bool,
    ) -> ExecutorOutcome {
        let Some(pressed) = self.active_actions.remove(&hold_id) else {
            return ExecutorOutcome::default();
        };
        let mut ctx = ExecutionContext {
            now_ms,
            config,
            ime_status,
            ime_status_dirty,
        };
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

        let message = serde_json::json!({
            "protocol": "touchdeck-ime-v1",
            "type": "key",
            "source": "touchdeck",
            "time": time,
            "key": key,
            "state": if pressed { "pressed" } else { "released" },
            "modifiers": self.modifier_mask,
            "translation": translation.map(KeyTranslationPolicy::as_str),
            "route": route.map(KeyRoute::as_str),
        });

        let mut stream = match UnixStream::connect(&ctx.config.text_output.ime_socket) {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!(
                    "touchdeck: failed to connect touchdeck-ime socket {}: {err}",
                    ctx.config.text_output.ime_socket.display()
                );
                return;
            }
        };

        if let Err(err) = serde_json::to_writer(&mut stream, &message)
            .and_then(|()| stream.write_all(b"\n").map_err(serde_json::Error::io))
        {
            eprintln!("touchdeck: failed to write touchdeck-ime event: {err}");
            return;
        }

        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {}
            Ok(_) => match serde_json::from_str::<ImeStatus>(line.trim()) {
                Ok(status) if status.protocol == "touchdeck-ime-v1" && status.kind == "status" => {
                    if status != *ctx.ime_status {
                        *ctx.ime_status = status;
                        *ctx.ime_status_dirty = true;
                    }
                }
                Ok(status) => {
                    eprintln!("touchdeck: ignored unsupported touchdeck-ime status {status:?}");
                }
                Err(err) => {
                    eprintln!("touchdeck: failed to parse touchdeck-ime status: {err}");
                }
            },
            Err(err) => {
                eprintln!("touchdeck: failed to read touchdeck-ime status: {err}");
            }
        }
    }

    fn key_tap_gap_ms(&self, key: u32, config: &Config) -> u32 {
        if modifier_mask_for_key(key).is_some() {
            config.modifier_tap_ms.max(1)
        } else {
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::NiriAction;
    use crate::config::{default_ime_socket_path, TextOutputBackend, TextOutputConfig};
    use crate::key::*;
    use crate::keymap::MacroRegistry;
    use crate::layout::SlotRegistry;

    fn test_config() -> Config {
        Config {
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
        let mut ime_status = ImeStatus::default();
        let mut ime_dirty = false;
        let mut ctx = ExecutionContext {
            now_ms: 0,
            config,
            ime_status: &mut ime_status,
            ime_status_dirty: &mut ime_dirty,
        };
        let (pressed, _) = executor.press_action_inner(action, &mut ctx);
        pressed
    }

    fn dispatch_for_test(executor: &mut ActionExecutor, action: GestureAction, config: &Config) {
        let mut ime_status = ImeStatus::default();
        let mut ime_dirty = false;
        let mut ctx = ExecutionContext {
            now_ms: 0,
            config,
            ime_status: &mut ime_status,
            ime_status_dirty: &mut ime_dirty,
        };
        executor.dispatch_action_inner(action, &mut ctx);
    }

    fn release_for_test(executor: &mut ActionExecutor, pressed: PressedAction, config: &Config) {
        let mut ime_status = ImeStatus::default();
        let mut ime_dirty = false;
        let mut ctx = ExecutionContext {
            now_ms: 0,
            config,
            ime_status: &mut ime_status,
            ime_status_dirty: &mut ime_dirty,
        };
        executor.release_pressed_action(pressed, &mut ctx);
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
