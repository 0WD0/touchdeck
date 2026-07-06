use std::env;
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use touchdeck::protocol::ImeStatus;

#[derive(Debug, Deserialize)]
pub(super) struct TouchDeckEvent {
    pub(super) protocol: String,
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) source: String,
    #[serde(default)]
    pub(super) time: u32,
    #[serde(default)]
    pub(super) key: u32,
    #[serde(default)]
    pub(super) state: String,
    #[serde(default)]
    pub(super) modifiers: u32,
    #[serde(default)]
    pub(super) translation: Option<String>,
    #[serde(default)]
    pub(super) route: Option<String>,
}

pub(super) enum TouchDeckRequest {
    Event {
        event: TouchDeckEvent,
        response: Sender<ImeStatus>,
    },
    Subscribe {
        response: Sender<ImeStatus>,
    },
}

pub(super) fn spawn_socket_listener(socket_path: PathBuf) -> Result<Receiver<TouchDeckRequest>> {
    if socket_path.exists() {
        let metadata = fs::symlink_metadata(&socket_path)
            .with_context(|| format!("stat existing socket path {}", socket_path.display()))?;
        if !metadata.file_type().is_socket() {
            return Err(anyhow!(
                "refusing to remove non-socket path {}",
                socket_path.display()
            ));
        }

        match UnixStream::connect(&socket_path) {
            Ok(_) => {
                return Err(anyhow!(
                    "touchdeck-ime socket {} is already in use",
                    socket_path.display()
                ));
            }
            Err(err) if matches!(err.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound) => {
                fs::remove_file(&socket_path)
                    .with_context(|| format!("remove stale socket {}", socket_path.display()))?;
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("connect existing socket {}", socket_path.display()));
            }
        }
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind touchdeck-ime socket {}", socket_path.display()))?;
    eprintln!("touchdeck-ime: listening on {}", socket_path.display());

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let tx = tx.clone();
                    thread::spawn(move || {
                        if let Err(err) = handle_client(stream, tx) {
                            eprintln!("touchdeck-ime: client error: {err:?}");
                        }
                    });
                }
                Err(err) => eprintln!("touchdeck-ime: accept error: {err}"),
            }
        }
    });

    Ok(rx)
}

pub(super) fn default_socket_path() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("touchdeck-ime.sock")
}

pub(super) fn handle_client(mut stream: UnixStream, tx: Sender<TouchDeckRequest>) -> Result<()> {
    let reader_stream = stream
        .try_clone()
        .context("clone touchdeck-ime client stream")?;
    let reader = BufReader::new(reader_stream);
    for line in reader.lines() {
        let line = line.context("read touchdeck-ime line")?;
        if line.trim().is_empty() {
            continue;
        }

        let event: TouchDeckEvent =
            serde_json::from_str(&line).with_context(|| format!("parse event {line}"))?;

        if event.protocol == "touchdeck-ime-v1"
            && event.kind == "subscribe_status"
            && event.source == "touchdeck"
        {
            let (status_tx, status_rx) = mpsc::channel();
            if tx
                .send(TouchDeckRequest::Subscribe {
                    response: status_tx,
                })
                .is_err()
            {
                break;
            }

            for status in status_rx {
                serde_json::to_writer(&mut stream, &status)
                    .context("write subscribed touchdeck-ime status")?;
                stream
                    .write_all(b"\n")
                    .context("write subscribed touchdeck-ime status newline")?;
            }
            break;
        }

        let (response_tx, response_rx) = mpsc::channel();
        if tx
            .send(TouchDeckRequest::Event {
                event,
                response: response_tx,
            })
            .is_err()
        {
            break;
        }
        let status = response_rx
            .recv_timeout(Duration::from_millis(500))
            .context("wait for touchdeck-ime status")?;
        serde_json::to_writer(&mut stream, &status).context("write touchdeck-ime status")?;
        stream
            .write_all(b"\n")
            .context("write touchdeck-ime status newline")?;
    }

    Ok(())
}



