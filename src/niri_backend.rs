use std::thread;

use anyhow::Result;
use touchdeck::niri;

use crate::action::{
    niri_action_request_json, niri_interactive_move_begin_request_json,
    niri_interactive_move_end_request_json, niri_interactive_move_update_request_json,
    niri_interactive_resize_begin_request_json, niri_interactive_resize_end_request_json,
    niri_interactive_resize_update_request_json, NiriCommand, NiriResizeEdge,
};

pub(crate) fn spawn_niri_action(action: NiriCommand) {
    thread::spawn(move || {
        let label = action.label().to_string();
        if let Err(err) = send_niri_action_socket(&action) {
            eprintln!("touchdeck: failed to send niri action {action}: {err:?}");
            eprintln!("touchdeck: failed niri action label: {label}");
        }
    });
}

pub(crate) fn spawn_niri_focus_at(x: f64, y: f64) {
    thread::spawn(move || match send_niri_focus_at(None, x, y) {
        Ok(()) => eprintln!("touchdeck: niri focus-at x={x:.1} y={y:.1}"),
        Err(err) => {
            eprintln!("touchdeck: failed to focus niri window at x={x:.1} y={y:.1}: {err:?}")
        }
    });
}

pub(crate) fn send_niri_action_socket(action: &NiriCommand) -> Result<()> {
    let request = niri_action_request_json(action);
    let _ = niri::send_ipc_request_json(&request)?;
    Ok(())
}

pub(crate) fn send_niri_focus_at(output: Option<&str>, x: f64, y: f64) -> Result<()> {
    niri::focus_window_at(output, x, y, false)
}

pub(crate) fn send_niri_interactive_move_begin(output: &str, x: f64, y: f64) -> Result<()> {
    let request = niri_interactive_move_begin_request_json(output, x, y);
    let _ = niri::send_ipc_request_json(&request)?;
    Ok(())
}

pub(crate) fn send_niri_interactive_move_update(
    output: &str,
    x: f64,
    y: f64,
    dx: f64,
    dy: f64,
) -> Result<()> {
    let request = niri_interactive_move_update_request_json(output, x, y, dx, dy);
    let _ = niri::send_ipc_request_json(&request)?;
    Ok(())
}

pub(crate) fn send_niri_interactive_move_end() -> Result<()> {
    let request = niri_interactive_move_end_request_json();
    let _ = niri::send_ipc_request_json(&request)?;
    Ok(())
}

pub(crate) fn send_niri_interactive_resize_begin(edge: NiriResizeEdge) -> Result<()> {
    let request = niri_interactive_resize_begin_request_json(edge);
    let _ = niri::send_ipc_request_json(&request)?;
    Ok(())
}

pub(crate) fn send_niri_interactive_resize_update(dx: f64, dy: f64) -> Result<()> {
    let request = niri_interactive_resize_update_request_json(dx, dy);
    let _ = niri::send_ipc_request_json(&request)?;
    Ok(())
}

pub(crate) fn send_niri_interactive_resize_end() -> Result<()> {
    let request = niri_interactive_resize_end_request_json();
    let _ = niri::send_ipc_request_json(&request)?;
    Ok(())
}
