use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};

#[derive(Clone, Copy, Debug)]
pub struct FocusedWindowLayout {
    pub window_rect_in_output: (f64, f64, i32, i32),
    pub working_area_in_output: (f64, f64, f64, f64),
}

#[derive(Clone, Copy, Debug)]
pub struct FocusedOutputLayout {
    pub width: u32,
    pub height: u32,
    pub transform: OutputTransform,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputTransform {
    Normal,
    _90,
    _180,
    _270,
    Flipped,
    Flipped90,
    Flipped180,
    Flipped270,
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
    let working_area_in_output = layout
        .get("working_area_in_output")
        .and_then(json_rect_f64)
        .ok_or_else(|| anyhow!("niri FocusedWindow layout missing working_area_in_output"))?;

    Ok(Some(FocusedWindowLayout {
        window_rect_in_output,
        working_area_in_output,
    }))
}

pub fn focused_output_layout() -> Result<Option<FocusedOutputLayout>> {
    let value = send_ipc_request_json("\"FocusedOutput\"")?;
    let focused = value
        .get("Ok")
        .and_then(|ok| ok.get("FocusedOutput"))
        .or_else(|| value.get("FocusedOutput"));
    let Some(focused) = focused else {
        return Ok(None);
    };
    if focused.is_null() {
        return Ok(None);
    }

    let Some(logical) = focused.get("logical") else {
        return Ok(None);
    };
    if logical.is_null() {
        return Ok(None);
    }

    let width = logical
        .get("width")
        .and_then(|value| u32::try_from(value.as_u64()?).ok())
        .ok_or_else(|| anyhow!("niri FocusedOutput logical missing width"))?;
    let height = logical
        .get("height")
        .and_then(|value| u32::try_from(value.as_u64()?).ok())
        .ok_or_else(|| anyhow!("niri FocusedOutput logical missing height"))?;
    let transform = logical
        .get("transform")
        .and_then(|value| value.as_str())
        .and_then(parse_transform)
        .ok_or_else(|| anyhow!("niri FocusedOutput logical missing transform"))?;

    Ok(Some(FocusedOutputLayout {
        width,
        height,
        transform,
    }))
}

pub fn focused_output_name() -> Result<Option<String>> {
    let value = send_ipc_request_json("\"FocusedOutput\"")?;
    let focused = value
        .get("Ok")
        .and_then(|ok| ok.get("FocusedOutput"))
        .or_else(|| value.get("FocusedOutput"));
    let Some(focused) = focused else {
        return Ok(None);
    };
    if focused.is_null() {
        return Ok(None);
    }

    Ok(focused
        .get("name")
        .and_then(|value| value.as_str())
        .map(str::to_string))
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

fn json_rect_f64(value: &serde_json::Value) -> Option<(f64, f64, f64, f64)> {
    let values = value.as_array()?;
    if values.len() != 4 {
        return None;
    }
    Some((
        values[0].as_f64()?,
        values[1].as_f64()?,
        values[2].as_f64()?,
        values[3].as_f64()?,
    ))
}

fn parse_transform(value: &str) -> Option<OutputTransform> {
    match value {
        "Normal" | "normal" => Some(OutputTransform::Normal),
        "90" => Some(OutputTransform::_90),
        "180" => Some(OutputTransform::_180),
        "270" => Some(OutputTransform::_270),
        "Flipped" | "flipped" => Some(OutputTransform::Flipped),
        "Flipped90" | "flipped-90" | "flipped90" => Some(OutputTransform::Flipped90),
        "Flipped180" | "flipped-180" | "flipped180" => Some(OutputTransform::Flipped180),
        "Flipped270" | "flipped-270" | "flipped270" => Some(OutputTransform::Flipped270),
        _ => None,
    }
}
