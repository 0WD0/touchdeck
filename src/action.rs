use anyhow::{anyhow, Result};
use niri_ipc::{
    Action as IpcAction, ColumnDisplay as IpcColumnDisplay, PositionChange as IpcPositionChange,
    Request as IpcRequest, SizeChange as IpcSizeChange,
    WorkspaceReferenceArg as IpcWorkspaceReference,
};

use crate::key::{normalize_name, KeyChord};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ActionStep {
    KeyDown(u32),
    KeyUp(u32),
    TapKey(u32),
    KeySequence(Vec<KeyChord>),
    Niri(NiriAction),
    Spawn(Vec<String>),
    SpawnSh(String),
    DelayMs(u32),
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum NiriAction {
    CloseWindow,
    FullscreenWindow,
    ToggleWindowedFullscreen,
    FocusColumnLeft,
    FocusColumnRight,
    FocusColumnFirst,
    FocusColumnLast,
    FocusColumnRightOrFirst,
    FocusColumnLeftOrLast,
    FocusColumn(usize),
    FocusWindowDown,
    FocusWindowUp,
    FocusWindowTop,
    FocusWindowBottom,
    FocusWindowDownOrTop,
    FocusWindowUpOrBottom,
    FocusWindowOrWorkspaceDown,
    FocusWindowOrWorkspaceUp,
    FocusWindowOrMonitorUp,
    FocusWindowOrMonitorDown,
    FocusColumnOrMonitorLeft,
    FocusColumnOrMonitorRight,
    MoveColumnLeft,
    MoveColumnRight,
    MoveColumnToFirst,
    MoveColumnToLast,
    MoveColumnLeftOrToMonitorLeft,
    MoveColumnRightOrToMonitorRight,
    MoveColumnToIndex(usize),
    MoveWindowDown,
    MoveWindowUp,
    MoveWindowDownOrToWorkspaceDown,
    MoveWindowUpOrToWorkspaceUp,
    MoveWindowToWorkspaceDown {
        focus: bool,
    },
    MoveWindowToWorkspaceUp {
        focus: bool,
    },
    MoveColumnToWorkspaceDown {
        focus: bool,
    },
    MoveColumnToWorkspaceUp {
        focus: bool,
    },
    MoveColumnToWorkspace {
        reference: WorkspaceReference,
        focus: bool,
    },
    ConsumeOrExpelWindowLeft,
    ConsumeOrExpelWindowRight,
    ConsumeWindowIntoColumn,
    ExpelWindowFromColumn,
    SwapWindowLeft,
    SwapWindowRight,
    ToggleColumnTabbedDisplay,
    SetColumnDisplay(ColumnDisplay),
    CenterColumn,
    CenterWindow,
    CenterVisibleColumns,
    FocusWorkspaceDown,
    FocusWorkspaceUp,
    FocusWorkspace(WorkspaceReference),
    FocusWorkspacePrevious,
    MoveWorkspaceDown,
    MoveWorkspaceUp,
    SwitchPresetColumnWidth,
    SwitchPresetColumnWidthBack,
    SwitchPresetWindowWidth,
    SwitchPresetWindowWidthBack,
    SwitchPresetWindowHeight,
    SwitchPresetWindowHeightBack,
    MaximizeColumn,
    MaximizeWindowToEdges,
    SetColumnWidth(SizeChange),
    SetWindowWidth(SizeChange),
    SetWindowHeight(SizeChange),
    ResetWindowHeight,
    ExpandColumnToAvailableWidth,
    ToggleWindowFloating,
    MoveWindowToFloating,
    MoveWindowToTiling,
    FocusFloating,
    FocusTiling,
    SwitchFocusBetweenFloatingAndTiling,
    MoveFloatingWindow {
        x: PositionChange,
        y: PositionChange,
    },
    ShowHotkeyOverlay,
    ToggleOverview,
    OpenOverview,
    CloseOverview,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SizeChange {
    SetFixed(i32),
    SetProportion(f64),
    AdjustFixed(i32),
    AdjustProportion(f64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PositionChange {
    SetFixed(f64),
    SetProportion(f64),
    AdjustFixed(f64),
    AdjustProportion(f64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceReference {
    Index(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColumnDisplay {
    Normal,
    Tabbed,
}

impl NiriAction {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CloseWindow => "close-window",
            Self::FullscreenWindow => "fullscreen-window",
            Self::ToggleWindowedFullscreen => "toggle-windowed-fullscreen",
            Self::FocusColumnLeft => "focus-column-left",
            Self::FocusColumnRight => "focus-column-right",
            Self::FocusColumnFirst => "focus-column-first",
            Self::FocusColumnLast => "focus-column-last",
            Self::FocusColumnRightOrFirst => "focus-column-right-or-first",
            Self::FocusColumnLeftOrLast => "focus-column-left-or-last",
            Self::FocusColumn(_) => "focus-column",
            Self::FocusWindowDown => "focus-window-down",
            Self::FocusWindowUp => "focus-window-up",
            Self::FocusWindowTop => "focus-window-top",
            Self::FocusWindowBottom => "focus-window-bottom",
            Self::FocusWindowDownOrTop => "focus-window-down-or-top",
            Self::FocusWindowUpOrBottom => "focus-window-up-or-bottom",
            Self::FocusWindowOrWorkspaceDown => "focus-window-or-workspace-down",
            Self::FocusWindowOrWorkspaceUp => "focus-window-or-workspace-up",
            Self::FocusWindowOrMonitorUp => "focus-window-or-monitor-up",
            Self::FocusWindowOrMonitorDown => "focus-window-or-monitor-down",
            Self::FocusColumnOrMonitorLeft => "focus-column-or-monitor-left",
            Self::FocusColumnOrMonitorRight => "focus-column-or-monitor-right",
            Self::MoveColumnLeft => "move-column-left",
            Self::MoveColumnRight => "move-column-right",
            Self::MoveColumnToFirst => "move-column-to-first",
            Self::MoveColumnToLast => "move-column-to-last",
            Self::MoveColumnLeftOrToMonitorLeft => "move-column-left-or-to-monitor-left",
            Self::MoveColumnRightOrToMonitorRight => "move-column-right-or-to-monitor-right",
            Self::MoveColumnToIndex(_) => "move-column-to-index",
            Self::MoveWindowDown => "move-window-down",
            Self::MoveWindowUp => "move-window-up",
            Self::MoveWindowDownOrToWorkspaceDown => "move-window-down-or-to-workspace-down",
            Self::MoveWindowUpOrToWorkspaceUp => "move-window-up-or-to-workspace-up",
            Self::MoveWindowToWorkspaceDown { .. } => "move-window-to-workspace-down",
            Self::MoveWindowToWorkspaceUp { .. } => "move-window-to-workspace-up",
            Self::MoveColumnToWorkspaceDown { .. } => "move-column-to-workspace-down",
            Self::MoveColumnToWorkspaceUp { .. } => "move-column-to-workspace-up",
            Self::MoveColumnToWorkspace { .. } => "move-column-to-workspace",
            Self::ConsumeOrExpelWindowLeft => "consume-or-expel-window-left",
            Self::ConsumeOrExpelWindowRight => "consume-or-expel-window-right",
            Self::ConsumeWindowIntoColumn => "consume-window-into-column",
            Self::ExpelWindowFromColumn => "expel-window-from-column",
            Self::SwapWindowLeft => "swap-window-left",
            Self::SwapWindowRight => "swap-window-right",
            Self::ToggleColumnTabbedDisplay => "toggle-column-tabbed-display",
            Self::SetColumnDisplay(_) => "set-column-display",
            Self::CenterColumn => "center-column",
            Self::CenterWindow => "center-window",
            Self::CenterVisibleColumns => "center-visible-columns",
            Self::FocusWorkspaceDown => "focus-workspace-down",
            Self::FocusWorkspaceUp => "focus-workspace-up",
            Self::FocusWorkspace(_) => "focus-workspace",
            Self::FocusWorkspacePrevious => "focus-workspace-previous",
            Self::MoveWorkspaceDown => "move-workspace-down",
            Self::MoveWorkspaceUp => "move-workspace-up",
            Self::SwitchPresetColumnWidth => "switch-preset-column-width",
            Self::SwitchPresetColumnWidthBack => "switch-preset-column-width-back",
            Self::SwitchPresetWindowWidth => "switch-preset-window-width",
            Self::SwitchPresetWindowWidthBack => "switch-preset-window-width-back",
            Self::SwitchPresetWindowHeight => "switch-preset-window-height",
            Self::SwitchPresetWindowHeightBack => "switch-preset-window-height-back",
            Self::MaximizeColumn => "maximize-column",
            Self::MaximizeWindowToEdges => "maximize-window-to-edges",
            Self::SetColumnWidth(_) => "set-column-width",
            Self::SetWindowWidth(_) => "set-window-width",
            Self::SetWindowHeight(_) => "set-window-height",
            Self::ResetWindowHeight => "reset-window-height",
            Self::ExpandColumnToAvailableWidth => "expand-column-to-available-width",
            Self::ToggleWindowFloating => "toggle-window-floating",
            Self::MoveWindowToFloating => "move-window-to-floating",
            Self::MoveWindowToTiling => "move-window-to-tiling",
            Self::FocusFloating => "focus-floating",
            Self::FocusTiling => "focus-tiling",
            Self::SwitchFocusBetweenFloatingAndTiling => "switch-focus-between-floating-and-tiling",
            Self::MoveFloatingWindow { .. } => "move-floating-window",
            Self::ShowHotkeyOverlay => "show-hotkey-overlay",
            Self::ToggleOverview => "toggle-overview",
            Self::OpenOverview => "open-overview",
            Self::CloseOverview => "close-overview",
        }
    }

    fn ipc_action(self) -> IpcAction {
        match self {
            Self::CloseWindow => IpcAction::CloseWindow { id: None },
            Self::FullscreenWindow => IpcAction::FullscreenWindow { id: None },
            Self::ToggleWindowedFullscreen => IpcAction::ToggleWindowedFullscreen { id: None },
            Self::FocusColumnLeft => IpcAction::FocusColumnLeft {},
            Self::FocusColumnRight => IpcAction::FocusColumnRight {},
            Self::FocusColumnFirst => IpcAction::FocusColumnFirst {},
            Self::FocusColumnLast => IpcAction::FocusColumnLast {},
            Self::FocusColumnRightOrFirst => IpcAction::FocusColumnRightOrFirst {},
            Self::FocusColumnLeftOrLast => IpcAction::FocusColumnLeftOrLast {},
            Self::FocusColumn(index) => IpcAction::FocusColumn { index },
            Self::FocusWindowDown => IpcAction::FocusWindowDown {},
            Self::FocusWindowUp => IpcAction::FocusWindowUp {},
            Self::FocusWindowTop => IpcAction::FocusWindowTop {},
            Self::FocusWindowBottom => IpcAction::FocusWindowBottom {},
            Self::FocusWindowDownOrTop => IpcAction::FocusWindowDownOrTop {},
            Self::FocusWindowUpOrBottom => IpcAction::FocusWindowUpOrBottom {},
            Self::FocusWindowOrWorkspaceDown => IpcAction::FocusWindowOrWorkspaceDown {},
            Self::FocusWindowOrWorkspaceUp => IpcAction::FocusWindowOrWorkspaceUp {},
            Self::FocusWindowOrMonitorUp => IpcAction::FocusWindowOrMonitorUp {},
            Self::FocusWindowOrMonitorDown => IpcAction::FocusWindowOrMonitorDown {},
            Self::FocusColumnOrMonitorLeft => IpcAction::FocusColumnOrMonitorLeft {},
            Self::FocusColumnOrMonitorRight => IpcAction::FocusColumnOrMonitorRight {},
            Self::MoveColumnLeft => IpcAction::MoveColumnLeft {},
            Self::MoveColumnRight => IpcAction::MoveColumnRight {},
            Self::MoveColumnToFirst => IpcAction::MoveColumnToFirst {},
            Self::MoveColumnToLast => IpcAction::MoveColumnToLast {},
            Self::MoveColumnLeftOrToMonitorLeft => IpcAction::MoveColumnLeftOrToMonitorLeft {},
            Self::MoveColumnRightOrToMonitorRight => IpcAction::MoveColumnRightOrToMonitorRight {},
            Self::MoveColumnToIndex(index) => IpcAction::MoveColumnToIndex { index },
            Self::MoveWindowDown => IpcAction::MoveWindowDown {},
            Self::MoveWindowUp => IpcAction::MoveWindowUp {},
            Self::MoveWindowDownOrToWorkspaceDown => IpcAction::MoveWindowDownOrToWorkspaceDown {},
            Self::MoveWindowUpOrToWorkspaceUp => IpcAction::MoveWindowUpOrToWorkspaceUp {},
            Self::MoveWindowToWorkspaceDown { focus } => {
                IpcAction::MoveWindowToWorkspaceDown { focus }
            }
            Self::MoveWindowToWorkspaceUp { focus } => IpcAction::MoveWindowToWorkspaceUp { focus },
            Self::MoveColumnToWorkspaceDown { focus } => {
                IpcAction::MoveColumnToWorkspaceDown { focus }
            }
            Self::MoveColumnToWorkspaceUp { focus } => IpcAction::MoveColumnToWorkspaceUp { focus },
            Self::MoveColumnToWorkspace { reference, focus } => IpcAction::MoveColumnToWorkspace {
                reference: ipc_workspace_reference(reference),
                focus,
            },
            Self::ConsumeOrExpelWindowLeft => IpcAction::ConsumeOrExpelWindowLeft { id: None },
            Self::ConsumeOrExpelWindowRight => IpcAction::ConsumeOrExpelWindowRight { id: None },
            Self::ConsumeWindowIntoColumn => IpcAction::ConsumeWindowIntoColumn {},
            Self::ExpelWindowFromColumn => IpcAction::ExpelWindowFromColumn {},
            Self::SwapWindowLeft => IpcAction::SwapWindowLeft {},
            Self::SwapWindowRight => IpcAction::SwapWindowRight {},
            Self::ToggleColumnTabbedDisplay => IpcAction::ToggleColumnTabbedDisplay {},
            Self::SetColumnDisplay(display) => IpcAction::SetColumnDisplay {
                display: ipc_column_display(display),
            },
            Self::CenterColumn => IpcAction::CenterColumn {},
            Self::CenterWindow => IpcAction::CenterWindow { id: None },
            Self::CenterVisibleColumns => IpcAction::CenterVisibleColumns {},
            Self::FocusWorkspaceDown => IpcAction::FocusWorkspaceDown {},
            Self::FocusWorkspaceUp => IpcAction::FocusWorkspaceUp {},
            Self::FocusWorkspace(reference) => IpcAction::FocusWorkspace {
                reference: ipc_workspace_reference(reference),
            },
            Self::FocusWorkspacePrevious => IpcAction::FocusWorkspacePrevious {},
            Self::MoveWorkspaceDown => IpcAction::MoveWorkspaceDown {},
            Self::MoveWorkspaceUp => IpcAction::MoveWorkspaceUp {},
            Self::SwitchPresetColumnWidth => IpcAction::SwitchPresetColumnWidth {},
            Self::SwitchPresetColumnWidthBack => IpcAction::SwitchPresetColumnWidthBack {},
            Self::SwitchPresetWindowWidth => IpcAction::SwitchPresetWindowWidth { id: None },
            Self::SwitchPresetWindowWidthBack => {
                IpcAction::SwitchPresetWindowWidthBack { id: None }
            }
            Self::SwitchPresetWindowHeight => IpcAction::SwitchPresetWindowHeight { id: None },
            Self::SwitchPresetWindowHeightBack => {
                IpcAction::SwitchPresetWindowHeightBack { id: None }
            }
            Self::MaximizeColumn => IpcAction::MaximizeColumn {},
            Self::MaximizeWindowToEdges => IpcAction::MaximizeWindowToEdges { id: None },
            Self::SetColumnWidth(change) => IpcAction::SetColumnWidth {
                change: ipc_size_change(change),
            },
            Self::SetWindowWidth(change) => IpcAction::SetWindowWidth {
                id: None,
                change: ipc_size_change(change),
            },
            Self::SetWindowHeight(change) => IpcAction::SetWindowHeight {
                id: None,
                change: ipc_size_change(change),
            },
            Self::ResetWindowHeight => IpcAction::ResetWindowHeight { id: None },
            Self::ExpandColumnToAvailableWidth => IpcAction::ExpandColumnToAvailableWidth {},
            Self::ToggleWindowFloating => IpcAction::ToggleWindowFloating { id: None },
            Self::MoveWindowToFloating => IpcAction::MoveWindowToFloating { id: None },
            Self::MoveWindowToTiling => IpcAction::MoveWindowToTiling { id: None },
            Self::FocusFloating => IpcAction::FocusFloating {},
            Self::FocusTiling => IpcAction::FocusTiling {},
            Self::SwitchFocusBetweenFloatingAndTiling => {
                IpcAction::SwitchFocusBetweenFloatingAndTiling {}
            }
            Self::MoveFloatingWindow { x, y } => IpcAction::MoveFloatingWindow {
                id: None,
                x: ipc_position_change(x),
                y: ipc_position_change(y),
            },
            Self::ShowHotkeyOverlay => IpcAction::ShowHotkeyOverlay {},
            Self::ToggleOverview => IpcAction::ToggleOverview {},
            Self::OpenOverview => IpcAction::OpenOverview {},
            Self::CloseOverview => IpcAction::CloseOverview {},
        }
    }
}

impl std::fmt::Display for NiriAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub(crate) fn niri_action_request_json(action: NiriAction) -> String {
    ipc_request_json(action.ipc_action())
}

pub(crate) fn niri_spawn_request_json(command: &[String]) -> String {
    ipc_request_json(IpcAction::Spawn {
        command: command.to_vec(),
    })
}

pub(crate) fn niri_spawn_sh_request_json(command: &str) -> String {
    ipc_request_json(IpcAction::SpawnSh {
        command: command.to_string(),
    })
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

pub(crate) fn parse_niri_action(value: &str) -> Result<NiriAction> {
    let value = value.trim();
    let (name, arg) = split_action_arg(value);
    let name = normalize_name(name);

    match name.as_str() {
        "close_window" => Ok(NiriAction::CloseWindow),
        "fullscreen_window" => Ok(NiriAction::FullscreenWindow),
        "toggle_windowed_fullscreen" => Ok(NiriAction::ToggleWindowedFullscreen),
        "focus_column_left" => Ok(NiriAction::FocusColumnLeft),
        "focus_column_right" => Ok(NiriAction::FocusColumnRight),
        "focus_column_first" => Ok(NiriAction::FocusColumnFirst),
        "focus_column_last" => Ok(NiriAction::FocusColumnLast),
        "focus_column_right_or_first" => Ok(NiriAction::FocusColumnRightOrFirst),
        "focus_column_left_or_last" => Ok(NiriAction::FocusColumnLeftOrLast),
        "focus_column" => Ok(NiriAction::FocusColumn(parse_usize_arg(name.as_str(), arg)?)),
        "focus_window_down" => Ok(NiriAction::FocusWindowDown),
        "focus_window_up" => Ok(NiriAction::FocusWindowUp),
        "focus_window_top" => Ok(NiriAction::FocusWindowTop),
        "focus_window_bottom" => Ok(NiriAction::FocusWindowBottom),
        "focus_window_down_or_top" => Ok(NiriAction::FocusWindowDownOrTop),
        "focus_window_up_or_bottom" => Ok(NiriAction::FocusWindowUpOrBottom),
        "focus_window_or_workspace_down" => Ok(NiriAction::FocusWindowOrWorkspaceDown),
        "focus_window_or_workspace_up" => Ok(NiriAction::FocusWindowOrWorkspaceUp),
        "focus_window_or_monitor_up" => Ok(NiriAction::FocusWindowOrMonitorUp),
        "focus_window_or_monitor_down" => Ok(NiriAction::FocusWindowOrMonitorDown),
        "focus_column_or_monitor_left" => Ok(NiriAction::FocusColumnOrMonitorLeft),
        "focus_column_or_monitor_right" => Ok(NiriAction::FocusColumnOrMonitorRight),
        "move_column_left" => Ok(NiriAction::MoveColumnLeft),
        "move_column_right" => Ok(NiriAction::MoveColumnRight),
        "move_column_to_first" => Ok(NiriAction::MoveColumnToFirst),
        "move_column_to_last" => Ok(NiriAction::MoveColumnToLast),
        "move_column_left_or_to_monitor_left" => Ok(NiriAction::MoveColumnLeftOrToMonitorLeft),
        "move_column_right_or_to_monitor_right" => Ok(NiriAction::MoveColumnRightOrToMonitorRight),
        "move_column_to_index" => {
            Ok(NiriAction::MoveColumnToIndex(parse_usize_arg(name.as_str(), arg)?))
        }
        "move_window_down" => Ok(NiriAction::MoveWindowDown),
        "move_window_up" => Ok(NiriAction::MoveWindowUp),
        "move_window_down_or_to_workspace_down" => Ok(NiriAction::MoveWindowDownOrToWorkspaceDown),
        "move_window_up_or_to_workspace_up" => Ok(NiriAction::MoveWindowUpOrToWorkspaceUp),
        "move_window_to_workspace_down" => Ok(NiriAction::MoveWindowToWorkspaceDown {
            focus: parse_bool_arg(arg, true)?,
        }),
        "move_window_to_workspace_up" => Ok(NiriAction::MoveWindowToWorkspaceUp {
            focus: parse_bool_arg(arg, true)?,
        }),
        "move_column_to_workspace_down" => Ok(NiriAction::MoveColumnToWorkspaceDown {
            focus: parse_bool_arg(arg, true)?,
        }),
        "move_column_to_workspace_up" => Ok(NiriAction::MoveColumnToWorkspaceUp {
            focus: parse_bool_arg(arg, true)?,
        }),
        "move_column_to_workspace" => Ok(NiriAction::MoveColumnToWorkspace {
            reference: parse_workspace_reference(required_arg(name.as_str(), arg)?)?,
            focus: true,
        }),
        "consume_or_expel_window_left" => Ok(NiriAction::ConsumeOrExpelWindowLeft),
        "consume_or_expel_window_right" => Ok(NiriAction::ConsumeOrExpelWindowRight),
        "consume_window_into_column" => Ok(NiriAction::ConsumeWindowIntoColumn),
        "expel_window_from_column" => Ok(NiriAction::ExpelWindowFromColumn),
        "swap_window_left" => Ok(NiriAction::SwapWindowLeft),
        "swap_window_right" => Ok(NiriAction::SwapWindowRight),
        "toggle_column_tabbed_display" => Ok(NiriAction::ToggleColumnTabbedDisplay),
        "set_column_display" => Ok(NiriAction::SetColumnDisplay(parse_column_display(
            required_arg(name.as_str(), arg)?,
        )?)),
        "center_column" => Ok(NiriAction::CenterColumn),
        "center_window" => Ok(NiriAction::CenterWindow),
        "center_visible_columns" => Ok(NiriAction::CenterVisibleColumns),
        "focus_workspace_down" => Ok(NiriAction::FocusWorkspaceDown),
        "focus_workspace_up" => Ok(NiriAction::FocusWorkspaceUp),
        "focus_workspace" => Ok(NiriAction::FocusWorkspace(parse_workspace_reference(
            required_arg(name.as_str(), arg)?,
        )?)),
        "focus_workspace_previous" => Ok(NiriAction::FocusWorkspacePrevious),
        "move_workspace_down" => Ok(NiriAction::MoveWorkspaceDown),
        "move_workspace_up" => Ok(NiriAction::MoveWorkspaceUp),
        "switch_preset_column_width" => Ok(NiriAction::SwitchPresetColumnWidth),
        "switch_preset_column_width_back" => Ok(NiriAction::SwitchPresetColumnWidthBack),
        "switch_preset_window_width" => Ok(NiriAction::SwitchPresetWindowWidth),
        "switch_preset_window_width_back" => Ok(NiriAction::SwitchPresetWindowWidthBack),
        "switch_preset_window_height" => Ok(NiriAction::SwitchPresetWindowHeight),
        "switch_preset_window_height_back" => Ok(NiriAction::SwitchPresetWindowHeightBack),
        "maximize_column" => Ok(NiriAction::MaximizeColumn),
        "maximize_window_to_edges" => Ok(NiriAction::MaximizeWindowToEdges),
        "set_column_width" => Ok(NiriAction::SetColumnWidth(parse_size_change(required_arg(
            name.as_str(),
            arg,
        )?)?)),
        "set_window_width" => Ok(NiriAction::SetWindowWidth(parse_size_change(required_arg(
            name.as_str(),
            arg,
        )?)?)),
        "set_window_height" => Ok(NiriAction::SetWindowHeight(parse_size_change(required_arg(
            name.as_str(),
            arg,
        )?)?)),
        "reset_window_height" => Ok(NiriAction::ResetWindowHeight),
        "expand_column_to_available_width" => Ok(NiriAction::ExpandColumnToAvailableWidth),
        "toggle_window_floating" => Ok(NiriAction::ToggleWindowFloating),
        "move_window_to_floating" => Ok(NiriAction::MoveWindowToFloating),
        "move_window_to_tiling" => Ok(NiriAction::MoveWindowToTiling),
        "focus_floating" => Ok(NiriAction::FocusFloating),
        "focus_tiling" => Ok(NiriAction::FocusTiling),
        "switch_focus_between_floating_and_tiling" => {
            Ok(NiriAction::SwitchFocusBetweenFloatingAndTiling)
        }
        "move_floating_window" => {
            let (x, y) = parse_position_pair(required_arg(name.as_str(), arg)?)?;
            Ok(NiriAction::MoveFloatingWindow { x, y })
        }
        "show_hotkey_overlay" => Ok(NiriAction::ShowHotkeyOverlay),
        "toggle_overview" => Ok(NiriAction::ToggleOverview),
        "open_overview" => Ok(NiriAction::OpenOverview),
        "close_overview" => Ok(NiriAction::CloseOverview),
        other => Err(anyhow!(
            "unsupported niri action {other}; use names like focus-column-left, move-column-left, set-window-width:+10%, set-window-height:50%, move-floating-window:+40,+0"
        )),
    }
}

fn split_action_arg(value: &str) -> (&str, Option<&str>) {
    value
        .split_once(':')
        .map(|(name, arg)| (name.trim(), Some(arg.trim())))
        .unwrap_or((value.trim(), None))
}

fn required_arg<'a>(name: &str, arg: Option<&'a str>) -> Result<&'a str> {
    arg.filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("niri action {name} requires an argument"))
}

fn parse_bool_arg(arg: Option<&str>, default: bool) -> Result<bool> {
    let Some(value) = arg else {
        return Ok(default);
    };

    match normalize_name(value).as_str() {
        "true" | "yes" | "on" | "1" | "focus" => Ok(true),
        "false" | "no" | "off" | "0" | "no_focus" | "stay" => Ok(false),
        other => Err(anyhow!("invalid boolean niri action argument {other}")),
    }
}

fn parse_usize_arg(name: &str, arg: Option<&str>) -> Result<usize> {
    required_arg(name, arg)?
        .parse()
        .map_err(|_| anyhow!("niri action {name} requires a positive integer argument"))
}

fn parse_workspace_reference(value: &str) -> Result<WorkspaceReference> {
    let index = value
        .parse::<u8>()
        .map_err(|_| anyhow!("only numeric workspace references are currently supported"))?;
    Ok(WorkspaceReference::Index(index))
}

fn parse_column_display(value: &str) -> Result<ColumnDisplay> {
    match normalize_name(value).as_str() {
        "normal" => Ok(ColumnDisplay::Normal),
        "tabbed" | "tabs" => Ok(ColumnDisplay::Tabbed),
        other => Err(anyhow!("unknown column display {other}")),
    }
}

fn parse_size_change(value: &str) -> Result<SizeChange> {
    if let Some(value) = value.strip_suffix('%') {
        if value.is_empty() {
            return Err(anyhow!("size percentage is missing a value"));
        }
        let parsed = value
            .parse::<f64>()
            .map_err(|_| anyhow!("invalid size percentage {value:?}"))?;
        if has_sign(value) {
            Ok(SizeChange::AdjustProportion(parsed))
        } else {
            Ok(SizeChange::SetProportion(parsed))
        }
    } else {
        let parsed = value
            .parse::<i32>()
            .map_err(|_| anyhow!("invalid fixed size change {value:?}"))?;
        if has_sign(value) {
            Ok(SizeChange::AdjustFixed(parsed))
        } else {
            Ok(SizeChange::SetFixed(parsed))
        }
    }
}

fn parse_position_change(value: &str) -> Result<PositionChange> {
    if let Some(value) = value.strip_suffix('%') {
        if value.is_empty() {
            return Err(anyhow!("position percentage is missing a value"));
        }
        let parsed = value
            .parse::<f64>()
            .map_err(|_| anyhow!("invalid position percentage {value:?}"))?;
        if has_sign(value) {
            Ok(PositionChange::AdjustProportion(parsed))
        } else {
            Ok(PositionChange::SetProportion(parsed))
        }
    } else {
        let parsed = value
            .parse::<f64>()
            .map_err(|_| anyhow!("invalid fixed position change {value:?}"))?;
        if has_sign(value) {
            Ok(PositionChange::AdjustFixed(parsed))
        } else {
            Ok(PositionChange::SetFixed(parsed))
        }
    }
}

fn parse_position_pair(value: &str) -> Result<(PositionChange, PositionChange)> {
    let (x, y) = value
        .split_once(',')
        .ok_or_else(|| anyhow!("move-floating-window expects x,y, for example +40,+0"))?;
    Ok((
        parse_position_change(x.trim())?,
        parse_position_change(y.trim())?,
    ))
}

fn has_sign(value: &str) -> bool {
    matches!(value.as_bytes().first(), Some(b'+' | b'-'))
}

fn ipc_request_json(action: IpcAction) -> String {
    serde_json::to_string(&IpcRequest::Action(action)).expect("serialize niri IPC request")
}

fn ipc_size_change(change: SizeChange) -> IpcSizeChange {
    match change {
        SizeChange::SetFixed(value) => IpcSizeChange::SetFixed(value),
        SizeChange::SetProportion(value) => IpcSizeChange::SetProportion(value),
        SizeChange::AdjustFixed(value) => IpcSizeChange::AdjustFixed(value),
        SizeChange::AdjustProportion(value) => IpcSizeChange::AdjustProportion(value),
    }
}

fn ipc_position_change(change: PositionChange) -> IpcPositionChange {
    match change {
        PositionChange::SetFixed(value) => IpcPositionChange::SetFixed(value),
        PositionChange::SetProportion(value) => IpcPositionChange::SetProportion(value),
        PositionChange::AdjustFixed(value) => IpcPositionChange::AdjustFixed(value),
        PositionChange::AdjustProportion(value) => IpcPositionChange::AdjustProportion(value),
    }
}

fn ipc_workspace_reference(reference: WorkspaceReference) -> IpcWorkspaceReference {
    match reference {
        WorkspaceReference::Index(value) => IpcWorkspaceReference::Index(value),
    }
}

fn ipc_column_display(display: ColumnDisplay) -> IpcColumnDisplay {
    match display {
        ColumnDisplay::Normal => IpcColumnDisplay::Normal,
        ColumnDisplay::Tabbed => IpcColumnDisplay::Tabbed,
    }
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
    fn maps_supported_niri_actions_to_ipc_json() {
        assert_json_eq(
            niri_action_request_json(parse_niri_action("focus-column-left").unwrap()),
            r#"{"Action":{"FocusColumnLeft":{}}}"#,
        );
        assert_json_eq(
            niri_action_request_json(parse_niri_action("toggle-overview").unwrap()),
            r#"{"Action":{"ToggleOverview":{}}}"#,
        );
    }

    #[test]
    fn maps_parameterized_niri_actions_to_ipc_json() {
        assert_json_eq(
            niri_action_request_json(parse_niri_action("set-window-width:+10%").unwrap()),
            r#"{"Action":{"SetWindowWidth":{"change":{"AdjustProportion":10.0},"id":null}}}"#,
        );
        assert_json_eq(
            niri_action_request_json(parse_niri_action("move-floating-window:+40,-10").unwrap()),
            r#"{"Action":{"MoveFloatingWindow":{"id":null,"x":{"AdjustFixed":40.0},"y":{"AdjustFixed":-10.0}}}}"#,
        );
        assert_json_eq(
            niri_action_request_json(
                parse_niri_action("move-window-to-workspace-up:false").unwrap(),
            ),
            r#"{"Action":{"MoveWindowToWorkspaceUp":{"focus":false}}}"#,
        );
        assert_json_eq(
            niri_action_request_json(parse_niri_action("move-column-to-workspace:5").unwrap()),
            r#"{"Action":{"MoveColumnToWorkspace":{"focus":true,"reference":{"Index":5}}}}"#,
        );
        assert_json_eq(
            niri_spawn_request_json(&["foot".to_string()]),
            r#"{"Action":{"Spawn":{"command":["foot"]}}}"#,
        );
        assert_json_eq(
            niri_spawn_sh_request_json("noctalia msg settings-toggle"),
            r#"{"Action":{"SpawnSh":{"command":"noctalia msg settings-toggle"}}}"#,
        );
    }
}
