use anyhow::{anyhow, Result};
use serde_json::{Map, Value};

use crate::key::{normalize_name, KeyChord};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ActionStep {
    KeyDown(u32),
    KeyUp(u32),
    TapKey(u32),
    KeySequence(Vec<KeyChord>),
    Niri(NiriAction),
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
    MoveWindowToWorkspaceDown { focus: bool },
    MoveWindowToWorkspaceUp { focus: bool },
    MoveColumnToWorkspaceDown { focus: bool },
    MoveColumnToWorkspaceUp { focus: bool },
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

    fn ipc_request_json(self) -> Value {
        match self {
            Self::CloseWindow => action("CloseWindow", object_with_null_id()),
            Self::FullscreenWindow => action("FullscreenWindow", object_with_null_id()),
            Self::ToggleWindowedFullscreen => {
                action("ToggleWindowedFullscreen", object_with_null_id())
            }
            Self::FocusColumnLeft => action("FocusColumnLeft", object()),
            Self::FocusColumnRight => action("FocusColumnRight", object()),
            Self::FocusColumnFirst => action("FocusColumnFirst", object()),
            Self::FocusColumnLast => action("FocusColumnLast", object()),
            Self::FocusColumnRightOrFirst => action("FocusColumnRightOrFirst", object()),
            Self::FocusColumnLeftOrLast => action("FocusColumnLeftOrLast", object()),
            Self::FocusColumn(index) => action("FocusColumn", field("index", Value::from(index as u64))),
            Self::FocusWindowDown => action("FocusWindowDown", object()),
            Self::FocusWindowUp => action("FocusWindowUp", object()),
            Self::FocusWindowTop => action("FocusWindowTop", object()),
            Self::FocusWindowBottom => action("FocusWindowBottom", object()),
            Self::FocusWindowDownOrTop => action("FocusWindowDownOrTop", object()),
            Self::FocusWindowUpOrBottom => action("FocusWindowUpOrBottom", object()),
            Self::FocusWindowOrWorkspaceDown => action("FocusWindowOrWorkspaceDown", object()),
            Self::FocusWindowOrWorkspaceUp => action("FocusWindowOrWorkspaceUp", object()),
            Self::FocusWindowOrMonitorUp => action("FocusWindowOrMonitorUp", object()),
            Self::FocusWindowOrMonitorDown => action("FocusWindowOrMonitorDown", object()),
            Self::FocusColumnOrMonitorLeft => action("FocusColumnOrMonitorLeft", object()),
            Self::FocusColumnOrMonitorRight => action("FocusColumnOrMonitorRight", object()),
            Self::MoveColumnLeft => action("MoveColumnLeft", object()),
            Self::MoveColumnRight => action("MoveColumnRight", object()),
            Self::MoveColumnToFirst => action("MoveColumnToFirst", object()),
            Self::MoveColumnToLast => action("MoveColumnToLast", object()),
            Self::MoveColumnLeftOrToMonitorLeft => action("MoveColumnLeftOrToMonitorLeft", object()),
            Self::MoveColumnRightOrToMonitorRight => {
                action("MoveColumnRightOrToMonitorRight", object())
            }
            Self::MoveColumnToIndex(index) => {
                action("MoveColumnToIndex", field("index", Value::from(index as u64)))
            }
            Self::MoveWindowDown => action("MoveWindowDown", object()),
            Self::MoveWindowUp => action("MoveWindowUp", object()),
            Self::MoveWindowDownOrToWorkspaceDown => {
                action("MoveWindowDownOrToWorkspaceDown", object())
            }
            Self::MoveWindowUpOrToWorkspaceUp => action("MoveWindowUpOrToWorkspaceUp", object()),
            Self::MoveWindowToWorkspaceDown { focus } => {
                action("MoveWindowToWorkspaceDown", field("focus", Value::from(focus)))
            }
            Self::MoveWindowToWorkspaceUp { focus } => {
                action("MoveWindowToWorkspaceUp", field("focus", Value::from(focus)))
            }
            Self::MoveColumnToWorkspaceDown { focus } => {
                action("MoveColumnToWorkspaceDown", field("focus", Value::from(focus)))
            }
            Self::MoveColumnToWorkspaceUp { focus } => {
                action("MoveColumnToWorkspaceUp", field("focus", Value::from(focus)))
            }
            Self::ConsumeOrExpelWindowLeft => {
                action("ConsumeOrExpelWindowLeft", object_with_null_id())
            }
            Self::ConsumeOrExpelWindowRight => {
                action("ConsumeOrExpelWindowRight", object_with_null_id())
            }
            Self::ConsumeWindowIntoColumn => action("ConsumeWindowIntoColumn", object()),
            Self::ExpelWindowFromColumn => action("ExpelWindowFromColumn", object()),
            Self::SwapWindowLeft => action("SwapWindowLeft", object()),
            Self::SwapWindowRight => action("SwapWindowRight", object()),
            Self::ToggleColumnTabbedDisplay => action("ToggleColumnTabbedDisplay", object()),
            Self::SetColumnDisplay(display) => {
                action("SetColumnDisplay", field("display", column_display(display)))
            }
            Self::CenterColumn => action("CenterColumn", object()),
            Self::CenterWindow => action("CenterWindow", object_with_null_id()),
            Self::CenterVisibleColumns => action("CenterVisibleColumns", object()),
            Self::FocusWorkspaceDown => action("FocusWorkspaceDown", object()),
            Self::FocusWorkspaceUp => action("FocusWorkspaceUp", object()),
            Self::FocusWorkspace(reference) => {
                action("FocusWorkspace", field("reference", workspace_reference(reference)))
            }
            Self::FocusWorkspacePrevious => action("FocusWorkspacePrevious", object()),
            Self::MoveWorkspaceDown => action("MoveWorkspaceDown", object()),
            Self::MoveWorkspaceUp => action("MoveWorkspaceUp", object()),
            Self::SwitchPresetColumnWidth => action("SwitchPresetColumnWidth", object()),
            Self::SwitchPresetColumnWidthBack => action("SwitchPresetColumnWidthBack", object()),
            Self::SwitchPresetWindowWidth => {
                action("SwitchPresetWindowWidth", object_with_null_id())
            }
            Self::SwitchPresetWindowWidthBack => {
                action("SwitchPresetWindowWidthBack", object_with_null_id())
            }
            Self::SwitchPresetWindowHeight => {
                action("SwitchPresetWindowHeight", object_with_null_id())
            }
            Self::SwitchPresetWindowHeightBack => {
                action("SwitchPresetWindowHeightBack", object_with_null_id())
            }
            Self::MaximizeColumn => action("MaximizeColumn", object()),
            Self::MaximizeWindowToEdges => action("MaximizeWindowToEdges", object_with_null_id()),
            Self::SetColumnWidth(change) => {
                action("SetColumnWidth", field("change", size_change(change)))
            }
            Self::SetWindowWidth(change) => action(
                "SetWindowWidth",
                fields([("id", Value::Null), ("change", size_change(change))]),
            ),
            Self::SetWindowHeight(change) => action(
                "SetWindowHeight",
                fields([("id", Value::Null), ("change", size_change(change))]),
            ),
            Self::ResetWindowHeight => action("ResetWindowHeight", object_with_null_id()),
            Self::ExpandColumnToAvailableWidth => action("ExpandColumnToAvailableWidth", object()),
            Self::ToggleWindowFloating => action("ToggleWindowFloating", object_with_null_id()),
            Self::MoveWindowToFloating => action("MoveWindowToFloating", object_with_null_id()),
            Self::MoveWindowToTiling => action("MoveWindowToTiling", object_with_null_id()),
            Self::FocusFloating => action("FocusFloating", object()),
            Self::FocusTiling => action("FocusTiling", object()),
            Self::SwitchFocusBetweenFloatingAndTiling => {
                action("SwitchFocusBetweenFloatingAndTiling", object())
            }
            Self::MoveFloatingWindow { x, y } => action(
                "MoveFloatingWindow",
                fields([
                    ("id", Value::Null),
                    ("x", position_change(x)),
                    ("y", position_change(y)),
                ]),
            ),
            Self::ShowHotkeyOverlay => action("ShowHotkeyOverlay", object()),
            Self::ToggleOverview => action("ToggleOverview", object()),
            Self::OpenOverview => action("OpenOverview", object()),
            Self::CloseOverview => action("CloseOverview", object()),
        }
    }
}

impl std::fmt::Display for NiriAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub(crate) fn niri_action_request_json(action: NiriAction) -> String {
    action.ipc_request_json().to_string()
}

pub(crate) fn niri_interactive_move_begin_request_json(output: &str, x: f64, y: f64) -> String {
    action(
        "InteractiveMoveBegin",
        fields([
            ("id", Value::Null),
            ("output", Value::String(output.to_string())),
            ("x", Value::from(x)),
            ("y", Value::from(y)),
        ]),
    )
    .to_string()
}

pub(crate) fn niri_interactive_move_update_request_json(
    output: &str,
    x: f64,
    y: f64,
    dx: f64,
    dy: f64,
) -> String {
    action(
        "InteractiveMoveUpdate",
        fields([
            ("output", Value::String(output.to_string())),
            ("x", Value::from(x)),
            ("y", Value::from(y)),
            ("dx", Value::from(dx)),
            ("dy", Value::from(dy)),
        ]),
    )
    .to_string()
}

pub(crate) fn niri_interactive_move_end_request_json() -> String {
    action("InteractiveMoveEnd", object()).to_string()
}

pub(crate) fn niri_interactive_resize_begin_request_json(edge: NiriResizeEdge) -> String {
    action(
        "InteractiveResizeBegin",
        fields([
            ("id", Value::Null),
            ("edges", Value::String(edge.as_str().to_string())),
        ]),
    )
    .to_string()
}

pub(crate) fn niri_interactive_resize_update_request_json(dx: f64, dy: f64) -> String {
    action(
        "InteractiveResizeUpdate",
        fields([("dx", Value::from(dx)), ("dy", Value::from(dy))]),
    )
    .to_string()
}

pub(crate) fn niri_interactive_resize_end_request_json() -> String {
    action("InteractiveResizeEnd", object()).to_string()
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
    Ok((parse_position_change(x.trim())?, parse_position_change(y.trim())?))
}

fn has_sign(value: &str) -> bool {
    matches!(value.as_bytes().first(), Some(b'+' | b'-'))
}

fn action(name: &str, payload: Value) -> Value {
    let mut action = Map::new();
    action.insert(name.to_string(), payload);

    let mut root = Map::new();
    root.insert("Action".to_string(), Value::Object(action));
    Value::Object(root)
}

fn object() -> Value {
    Value::Object(Map::new())
}

fn object_with_null_id() -> Value {
    field("id", Value::Null)
}

fn field(name: &str, value: impl Into<Value>) -> Value {
    fields([(name, value.into())])
}

fn fields<const N: usize>(fields: [(&str, Value); N]) -> Value {
    let mut object = Map::new();
    for (name, value) in fields {
        object.insert(name.to_string(), value);
    }
    Value::Object(object)
}

fn size_change(change: SizeChange) -> Value {
    match change {
        SizeChange::SetFixed(value) => tagged("SetFixed", value),
        SizeChange::SetProportion(value) => tagged("SetProportion", value),
        SizeChange::AdjustFixed(value) => tagged("AdjustFixed", value),
        SizeChange::AdjustProportion(value) => tagged("AdjustProportion", value),
    }
}

fn position_change(change: PositionChange) -> Value {
    match change {
        PositionChange::SetFixed(value) => tagged("SetFixed", value),
        PositionChange::SetProportion(value) => tagged("SetProportion", value),
        PositionChange::AdjustFixed(value) => tagged("AdjustFixed", value),
        PositionChange::AdjustProportion(value) => tagged("AdjustProportion", value),
    }
}

fn workspace_reference(reference: WorkspaceReference) -> Value {
    match reference {
        WorkspaceReference::Index(value) => tagged("Index", value),
    }
}

fn column_display(display: ColumnDisplay) -> Value {
    match display {
        ColumnDisplay::Normal => Value::String("Normal".to_string()),
        ColumnDisplay::Tabbed => Value::String("Tabbed".to_string()),
    }
}

fn tagged(name: &str, value: impl Into<Value>) -> Value {
    let mut object = Map::new();
    object.insert(name.to_string(), value.into());
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_supported_niri_actions_to_ipc_json() {
        assert_eq!(
            niri_action_request_json(parse_niri_action("focus-column-left").unwrap()),
            r#"{"Action":{"FocusColumnLeft":{}}}"#
        );
        assert_eq!(
            niri_action_request_json(parse_niri_action("toggle-overview").unwrap()),
            r#"{"Action":{"ToggleOverview":{}}}"#
        );
    }

    #[test]
    fn maps_parameterized_niri_actions_to_ipc_json() {
        assert_eq!(
            niri_action_request_json(parse_niri_action("set-window-width:+10%").unwrap()),
            r#"{"Action":{"SetWindowWidth":{"change":{"AdjustProportion":10.0},"id":null}}}"#
        );
        assert_eq!(
            niri_action_request_json(parse_niri_action("move-floating-window:+40,-10").unwrap()),
            r#"{"Action":{"MoveFloatingWindow":{"id":null,"x":{"AdjustFixed":40.0},"y":{"AdjustFixed":-10.0}}}}"#
        );
        assert_eq!(
            niri_action_request_json(parse_niri_action("move-window-to-workspace-up:false").unwrap()),
            r#"{"Action":{"MoveWindowToWorkspaceUp":{"focus":false}}}"#
        );
    }
}
