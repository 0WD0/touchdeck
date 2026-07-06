use touchdeck::protocol::ImeStatus;

use super::fcitx_dbus::{FcitxDbusOutput, FCITX_CAPABILITY_CLIENT_SIDE_INPUT_PANEL};
use super::rime_engine::RimeOutput;
use super::{status_is_empty, ImeApp};

impl ImeApp {
    pub(super) fn apply_rime_output(&mut self, output: RimeOutput) {
        let preedit = output.status.preedit.clone();
        let commit = output.commit;
        let status = output.status.clone();
        self.status = output.status;
        self.status.active = self.active;

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
