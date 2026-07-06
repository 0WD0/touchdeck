use std::thread;

use anyhow::Result;
use touchdeck::niri;

use crate::action::{niri_action_request_json, NiriAction};

pub(crate) fn spawn_niri_action(action: NiriAction) {
    thread::spawn(move || {
        if let Err(err) = send_niri_action_socket(action) {
            eprintln!("touchdeck: failed to send niri action {action}: {err:?}");
        }
    });
}

pub(crate) fn send_niri_action_socket(action: NiriAction) -> Result<()> {
    let request = niri_action_request_json(action);
    let _ = niri::send_ipc_request_json(&request)?;
    Ok(())
}
