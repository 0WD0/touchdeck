use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Default, Debug)]
struct ImeState {
    composing: String,
}

#[derive(Debug, Deserialize)]
struct TouchDeckEvent {
    protocol: String,
    #[serde(rename = "type")]
    kind: String,
    source: String,
    time: u32,
    key: u32,
    state: String,
    modifiers: u32,
}

fn main() -> Result<()> {
    let socket_path = env::var_os("TOUCHDECK_IME_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(default_socket_path);

    if socket_path.exists() {
        fs::remove_file(&socket_path)
            .with_context(|| format!("remove stale socket {}", socket_path.display()))?;
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind touchdeck-ime socket {}", socket_path.display()))?;
    eprintln!("touchdeck-ime: listening on {}", socket_path.display());
    eprintln!("touchdeck-ime: IPC receiver only; Wayland input-method/Rime is not implemented yet");

    let state = Arc::new(Mutex::new(ImeState::default()));
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let state = Arc::clone(&state);
                thread::spawn(move || {
                    if let Err(err) = handle_client(stream, state) {
                        eprintln!("touchdeck-ime: client error: {err:?}");
                    }
                });
            }
            Err(err) => eprintln!("touchdeck-ime: accept error: {err}"),
        }
    }

    Ok(())
}

fn default_socket_path() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("touchdeck-ime.sock")
}

fn handle_client(stream: UnixStream, state: Arc<Mutex<ImeState>>) -> Result<()> {
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line.context("read touchdeck-ime line")?;
        if line.trim().is_empty() {
            continue;
        }

        let event: TouchDeckEvent =
            serde_json::from_str(&line).with_context(|| format!("parse event {line}"))?;
        handle_event(event, &state);
    }

    Ok(())
}

fn handle_event(event: TouchDeckEvent, state: &Arc<Mutex<ImeState>>) {
    if event.protocol != "touchdeck-ime-v1" || event.kind != "key" || event.source != "touchdeck" {
        eprintln!("touchdeck-ime: ignored unsupported event {event:?}");
        return;
    }

    if event.state != "pressed" {
        return;
    }

    let mut state = state.lock().unwrap();
    match event.key {
        1 => state.composing.clear(),            // Escape
        14 => {
            state.composing.pop();               // Backspace
        }
        28 | 57 => {                               // Enter or Space
            if !state.composing.is_empty() {
                eprintln!("touchdeck-ime: commit {:?}", state.composing);
                state.composing.clear();
            }
        }
        key => {
            if let Some(ch) = evdev_key_to_ascii(key, event.modifiers) {
                state.composing.push(ch);
            }
        }
    }

    eprintln!(
        "touchdeck-ime: key={} time={} modifiers={} composing={:?}",
        event.key, event.time, event.modifiers, state.composing
    );
}

fn evdev_key_to_ascii(key: u32, modifiers: u32) -> Option<char> {
    let shifted = modifiers & 1 != 0;
    let ch = match key {
        2 => '1',
        3 => '2',
        4 => '3',
        5 => '4',
        6 => '5',
        7 => '6',
        8 => '7',
        9 => '8',
        10 => '9',
        11 => '0',
        16 => 'q',
        17 => 'w',
        18 => 'e',
        19 => 'r',
        20 => 't',
        21 => 'y',
        22 => 'u',
        23 => 'i',
        24 => 'o',
        25 => 'p',
        30 => 'a',
        31 => 's',
        32 => 'd',
        33 => 'f',
        34 => 'g',
        35 => 'h',
        36 => 'j',
        37 => 'k',
        38 => 'l',
        44 => 'z',
        45 => 'x',
        46 => 'c',
        47 => 'v',
        48 => 'b',
        49 => 'n',
        50 => 'm',
        51 => ',',
        52 => '.',
        53 => '/',
        _ => return None,
    };

    if shifted && ch.is_ascii_alphabetic() {
        Some(ch.to_ascii_uppercase())
    } else {
        Some(ch)
    }
}
