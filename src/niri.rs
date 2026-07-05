use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};

#[derive(Clone, Copy, Debug)]
pub struct FocusedWindowLayout {
    pub window_rect_in_output: (f64, f64, i32, i32),
}

pub fn focused_window_layout() -> Result<Option<FocusedWindowLayout>> {
    let value = send_ipc_request_json("\"FocusedWindow\"")?;
    let focused = value
        .get("Ok")
        .and_then(|ok| ok.get("FocusedWindow"))
        .or_else(|| value.get("FocusedWindow"));
    let Some(focused) = focused else {
        return Ok(None);
    };
    if focused.is_null() {
        return Ok(None);
    }

    let Some(layout) = focused.get("layout") else {
        return Ok(None);
    };

    let window_rect_in_output = layout
        .get("window_rect_in_output")
        .and_then(json_window_rect)
        .ok_or_else(|| anyhow!("niri FocusedWindow layout missing window_rect_in_output"))?;

    Ok(Some(FocusedWindowLayout {
        window_rect_in_output,
    }))
}

pub fn send_ipc_request_json(request: &str) -> Result<serde_json::Value> {
    let socket_path = env::var_os("NIRI_SOCKET")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("NIRI_SOCKET is not set"))?;
    let mut stream = UnixStream::connect(&socket_path)
        .with_context(|| format!("connect niri IPC socket {}", socket_path.display()))?;

    stream
        .write_all(request.as_bytes())
        .context("write niri IPC request")?;
    stream.write_all(b"\n").context("write niri IPC newline")?;
    stream.flush().context("flush niri IPC request")?;

    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    let bytes = reader
        .read_line(&mut reply)
        .context("read niri IPC response")?;
    if bytes == 0 {
        return Err(anyhow!("empty niri IPC response"));
    }

    let reply = reply.trim();
    let value: serde_json::Value =
        serde_json::from_str(reply).with_context(|| format!("parse niri IPC response {reply}"))?;
    if let Some(err) = value.get("Err") {
        return Err(anyhow!("niri IPC error: {err}"));
    }

    Ok(value)
}

fn json_window_rect(value: &serde_json::Value) -> Option<(f64, f64, i32, i32)> {
    let values = value.as_array()?;
    if values.len() != 4 {
        return None;
    }
    Some((
        values[0].as_f64()?,
        values[1].as_f64()?,
        i32::try_from(values[2].as_i64()?).ok()?,
        i32::try_from(values[3].as_i64()?).ok()?,
    ))
}
