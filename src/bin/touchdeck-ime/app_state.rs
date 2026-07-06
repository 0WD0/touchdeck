use touchdeck::protocol::ImeStatus;

use super::config::KeyTranslationPolicy;
use super::fcitx_dbus::{FcitxDbusOutput, FCITX_CAPABILITY_CLIENT_SIDE_INPUT_PANEL};
use super::key::KeyState;
use super::{status_is_empty, ImeApp};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ImeSource {
    FcitxDbus,
    Physical,
    Touchdeck,
    Xim,
}

impl ImeSource {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::FcitxDbus => "fcitx-dbus",
            Self::Physical => "physical",
            Self::Touchdeck => "touchdeck",
            Self::Xim => "xim",
        }
    }
}

pub(super) struct ImeEffects {
    pub(super) handled: bool,
    pub(super) preedit: String,
    pub(super) commit: Option<String>,
    pub(super) status: ImeStatus,
}

impl ImeApp {
    pub(super) fn process_rime_key(
        &mut self,
        context: &str,
        keysym: u32,
        state: KeyState,
        modifiers: u32,
        translation: Option<KeyTranslationPolicy>,
    ) -> Option<ImeEffects> {
        let Some(rime) = self.rime.as_mut() else {
            eprintln!("touchdeck-ime: rime engine unavailable for {context}");
            return None;
        };

        let output = match rime.process_key(keysym, state, modifiers, translation) {
            Ok(output) => output,
            Err(err) => {
                eprintln!("touchdeck-ime: rime error for {context}: {err:?}");
                return None;
            }
        };

        let preedit = output.status.preedit.clone();
        Some(ImeEffects {
            handled: output.handled,
            preedit,
            commit: output.commit,
            status: output.status,
        })
    }

    pub(super) fn apply_local_effects(&mut self, source: ImeSource, mut effects: ImeEffects) {
        let preedit = effects.preedit;
        let commit = effects.commit;
        let status = effects.status.clone();
        effects.status.active = self.active;
        effects.status.source = source.as_str().to_string();
        self.status = effects.status;
        self.status.active = self.active;

        log_ime_ownership(|| {
            eprintln!(
                "touchdeck-ime: ownership apply_local source={} active={} fcitx_focus={} preedit={:?} candidates={} commit={}",
                source.as_str(),
                self.active,
                self.fcitx_focus.is_some(),
                preedit,
                status.candidates.len(),
                commit.is_some()
            );
        });

        if self.fcitx_focus.is_some() {
            self.emit_fcitx_output(preedit, commit, status);
            return;
        }

        if preedit != self.preedit {
            self.set_preedit(preedit);
        }

        if let Some(text) = commit {
            self.commit_text(text);
        }
    }

    pub(super) fn apply_response_effects(
        &mut self,
        source: ImeSource,
        mut effects: ImeEffects,
    ) -> ImeEffects {
        effects.status.active = true;
        effects.status.source = source.as_str().to_string();
        self.status = effects.status.clone();
        self.preedit = effects.preedit.clone();
        log_ime_ownership(|| {
            eprintln!(
                "touchdeck-ime: ownership apply_response source={} active={} fcitx_focus={} preedit={:?} candidates={} commit={}",
                source.as_str(),
                effects.status.active,
                self.fcitx_focus.is_some(),
                effects.preedit,
                effects.status.candidates.len(),
                effects.commit.is_some()
            );
        });
        self.broadcast_status(source.as_str());
        effects
    }

    pub(super) fn fcitx_uses_client_side_input_panel(&self) -> bool {
        self.fcitx_focus.is_some()
            && (self.fcitx_capability & FCITX_CAPABILITY_CLIENT_SIDE_INPUT_PANEL) != 0
    }

    pub(super) fn emit_fcitx_output(
        &mut self,
        preedit: String,
        commit: Option<String>,
        status: ImeStatus,
    ) {
        let target = match self.fcitx_focus.clone() {
            Some(target) => target,
            None => return,
        };

        log_ime_ownership(|| {
            eprintln!(
                "touchdeck-ime: ownership emit_fcitx_output target={} display={} preedit={:?} candidates={} commit={}",
                target.path.as_str(),
                target.display,
                preedit,
                status.candidates.len(),
                commit.is_some()
            );
        });

        let preedit_changed = preedit != self.preedit;
        self.preedit = preedit.clone();

        if preedit_changed || commit.is_some() {
            if let Some(tx) = &self.fcitx_output_tx {
                let cursor_rect = self
                    .fcitx_cursor_rect
                    .as_ref()
                    .filter(|rect| rect.target.matches(&target))
                    .cloned();
                eprintln!(
                    "touchdeck-ime: fcitx dbus output path={} commit={:?} preedit={:?} cursor_rect={:?}",
                    target.path.as_str(),
                    commit,
                    preedit,
                    cursor_rect
                );
                let _ = tx.send(FcitxDbusOutput {
                    target,
                    preedit: Some(preedit),
                    commit,
                    status,
                    cursor_rect,
                });
            }
        }
    }

    pub(super) fn rime_state_is_empty(&self) -> bool {
        self.preedit.is_empty() && status_is_empty(&self.status)
    }

    pub(super) fn set_preedit(&mut self, text: String) {
        self.preedit = text;

        let Some(input_method) = &self.input_method else {
            return;
        };

        let cursor = self.preedit.len().min(i32::MAX as usize) as i32;
        input_method.set_preedit_string(self.preedit.clone(), cursor, cursor);
        input_method.commit(self.serial);
    }

    pub(super) fn clear_preedit(&mut self) {
        self.set_preedit(String::new());
        self.status.preedit.clear();
        self.status.commit_preview.clear();
        self.status.candidates.clear();
        self.status.highlighted_candidate_index = None;
    }

    pub(super) fn commit_text(&mut self, text: String) {
        let Some(input_method) = &self.input_method else {
            return;
        };

        if !self.preedit.is_empty() {
            input_method.set_preedit_string(String::new(), 0, 0);
            self.preedit.clear();
        }

        eprintln!("touchdeck-ime: commit {text:?}");
        input_method.commit_string(text);
        input_method.commit(self.serial);
    }
}

fn log_ime_ownership(log: impl FnOnce()) {
    if std::env::var_os("TOUCHDECK_LOG_IME_OWNERSHIP").is_some() {
        log();
    }
}
