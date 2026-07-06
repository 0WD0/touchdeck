use std::thread;

use anyhow::{anyhow, Result};

use crate::key::{normalize_name, KeyChord};
use touchdeck::niri;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ActionStep {
    KeyDown(u32),
    KeyUp(u32),
    TapKey(u32),
    KeySequence(Vec<KeyChord>),
    Niri(NiriAction),
    DelayMs(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NiriAction {
    FocusColumnLeft,
    FocusColumnRight,
    FocusWorkspaceUp,
    FocusWorkspaceDown,
    ToggleOverview,
}

impl NiriAction {
    pub(crate) fn as_str(self) -> &'static str {
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


pub(crate) fn spawn_niri_action(action: NiriAction) {
    thread::spawn(move || {
        if let Err(err) = send_niri_action_socket(action) {
            eprintln!("touchdeck: failed to send niri action {action}: {err:?}");
        }
    });
}

pub(crate) fn send_niri_action_socket(action: NiriAction) -> Result<()> {
    let request = niri_action_request_json(action);
    let _ = niri::send_ipc_request_json(request)?;
    Ok(())
}

fn niri_action_request_json(action: NiriAction) -> &'static str {
    action.ipc_request_json()
}


pub(crate) fn parse_niri_action(value: &str) -> Result<NiriAction> {
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

