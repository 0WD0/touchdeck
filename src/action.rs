use anyhow::{anyhow, Result};
use clap::Parser;
use niri_ipc::{Action as IpcAction, Request as IpcRequest};

use crate::key::{normalize_name, KeyChord};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ActionStep {
    KeyDown(u32),
    KeyUp(u32),
    TapKey(u32),
    KeySequence(Vec<KeyChord>),
    Niri(NiriCommand),
    DelayMs(u32),
}

#[derive(Clone, Debug)]
pub(crate) struct NiriCommand {
    label: String,
    action: IpcAction,
}

impl NiriCommand {
    pub(crate) fn new(label: impl Into<String>, action: IpcAction) -> Self {
        Self {
            label: label.into(),
            action,
        }
    }

    pub(crate) fn spawn(command: Vec<String>) -> Self {
        Self::new(
            format!("spawn {}", command.join(" ")),
            IpcAction::Spawn { command },
        )
    }

    pub(crate) fn spawn_sh(command: String) -> Self {
        Self::new(
            format!("spawn-sh {command}"),
            IpcAction::SpawnSh { command },
        )
    }

    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    pub(crate) fn request_json(&self) -> String {
        ipc_request_json(self.action.clone())
    }
}

impl std::fmt::Display for NiriCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

impl PartialEq for NiriCommand {
    fn eq(&self, other: &Self) -> bool {
        self.request_json() == other.request_json()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NiriResizeEdge {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl NiriResizeEdge {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomRight => "bottom-right",
        }
    }
}

pub(crate) fn parse_niri_resize_edge(value: &str) -> Result<NiriResizeEdge> {
    match normalize_name(value).as_str() {
        "left" => Ok(NiriResizeEdge::Left),
        "right" => Ok(NiriResizeEdge::Right),
        "top" | "up" => Ok(NiriResizeEdge::Top),
        "bottom" | "down" => Ok(NiriResizeEdge::Bottom),
        "top_left" | "left_top" => Ok(NiriResizeEdge::TopLeft),
        "top_right" | "right_top" => Ok(NiriResizeEdge::TopRight),
        "bottom_left" | "left_bottom" => Ok(NiriResizeEdge::BottomLeft),
        "bottom_right" | "right_bottom" => Ok(NiriResizeEdge::BottomRight),
        _ => Err(anyhow!("unknown niri resize edge {value}")),
    }
}

pub(crate) fn parse_niri_action(value: &str) -> Result<NiriCommand> {
    let label = value.trim();
    if label.is_empty() {
        return Err(anyhow!("niri action is empty"));
    }

    let tokens = label.split_whitespace().collect::<Vec<_>>();
    let action = IpcAction::try_parse_from(std::iter::once("touchdeck-niri").chain(tokens))
        .map_err(|err| anyhow!("invalid niri action {label:?}: {err}"))?;

    Ok(NiriCommand::new(label, action))
}

pub(crate) fn niri_action_request_json(action: &NiriCommand) -> String {
    action.request_json()
}

pub(crate) fn niri_interactive_move_begin_request_json(output: &str, x: f64, y: f64) -> String {
    ipc_request_json(IpcAction::InteractiveMoveBegin {
        id: None,
        output: output.to_string(),
        x,
        y,
    })
}

pub(crate) fn niri_interactive_move_update_request_json(
    output: &str,
    x: f64,
    y: f64,
    dx: f64,
    dy: f64,
) -> String {
    ipc_request_json(IpcAction::InteractiveMoveUpdate {
        output: output.to_string(),
        x,
        y,
        dx,
        dy,
    })
}

pub(crate) fn niri_interactive_move_end_request_json() -> String {
    ipc_request_json(IpcAction::InteractiveMoveEnd {})
}

pub(crate) fn niri_interactive_resize_begin_request_json(edge: NiriResizeEdge) -> String {
    ipc_request_json(IpcAction::InteractiveResizeBegin {
        id: None,
        edges: edge.as_str().to_string(),
    })
}

pub(crate) fn niri_interactive_resize_update_request_json(dx: f64, dy: f64) -> String {
    ipc_request_json(IpcAction::InteractiveResizeUpdate { dx, dy })
}

pub(crate) fn niri_interactive_resize_end_request_json() -> String {
    ipc_request_json(IpcAction::InteractiveResizeEnd {})
}

fn ipc_request_json(action: IpcAction) -> String {
    serde_json::to_string(&IpcRequest::Action(action)).expect("serialize niri IPC request")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn assert_json_eq(actual: String, expected: &str) {
        let actual: Value = serde_json::from_str(&actual).unwrap();
        let expected: Value = serde_json::from_str(expected).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn parses_supported_niri_actions_to_ipc_json() {
        assert_json_eq(
            niri_action_request_json(&parse_niri_action("focus-column-left").unwrap()),
            r#"{"Action":{"FocusColumnLeft":{}}}"#,
        );
        assert_json_eq(
            niri_action_request_json(&parse_niri_action("toggle-overview").unwrap()),
            r#"{"Action":{"ToggleOverview":{}}}"#,
        );
    }

    #[test]
    fn parses_parameterized_niri_actions_to_ipc_json() {
        assert_json_eq(
            niri_action_request_json(&parse_niri_action("set-window-width +10%").unwrap()),
            r#"{"Action":{"SetWindowWidth":{"change":{"AdjustProportion":10.0},"id":null}}}"#,
        );
        assert_json_eq(
            niri_action_request_json(
                &parse_niri_action("move-floating-window -x +40 -y -10").unwrap(),
            ),
            r#"{"Action":{"MoveFloatingWindow":{"id":null,"x":{"AdjustFixed":40.0},"y":{"AdjustFixed":-10.0}}}}"#,
        );
        assert_json_eq(
            niri_action_request_json(
                &parse_niri_action("move-window-to-workspace-up --focus false").unwrap(),
            ),
            r#"{"Action":{"MoveWindowToWorkspaceUp":{"focus":false}}}"#,
        );
        assert_json_eq(
            niri_action_request_json(&parse_niri_action("move-column-to-workspace 5").unwrap()),
            r#"{"Action":{"MoveColumnToWorkspace":{"focus":true,"reference":{"Index":5}}}}"#,
        );
        assert_json_eq(
            NiriCommand::spawn(vec!["foot".to_string()]).request_json(),
            r#"{"Action":{"Spawn":{"command":["foot"]}}}"#,
        );
        assert_json_eq(
            NiriCommand::spawn_sh("noctalia msg settings-toggle".to_string()).request_json(),
            r#"{"Action":{"SpawnSh":{"command":"noctalia msg settings-toggle"}}}"#,
        );
    }
}
