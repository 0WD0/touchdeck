use std::fs;
use std::io::{ErrorKind, Result as IoResult};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SunshineTouchKind {
    Down,
    Motion,
    Up,
    Cancel,
}

#[derive(Clone, Debug)]
pub(crate) struct SunshineTouchRequest {
    pub(crate) addr: SocketAddr,
    pub(crate) seq: u64,
    pub(crate) output: Option<String>,
    pub(crate) kind: SunshineTouchKind,
    pub(crate) id: i32,
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) time: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SunshineRouteDecision {
    App,
    TouchDeck,
}

#[derive(Debug)]
pub(crate) struct SunshineRouter {
    socket: UnixDatagram,
    path: PathBuf,
}

impl SunshineRouter {
    pub(crate) fn bind(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create Sunshine router dir {}", parent.display()))?;
        }
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("remove stale Sunshine router socket {}", path.display())
                });
            }
        }

        let socket = UnixDatagram::bind(path)
            .with_context(|| format!("bind Sunshine router socket {}", path.display()))?;
        socket
            .set_nonblocking(true)
            .context("set Sunshine router socket nonblocking")?;

        eprintln!("touchdeck: Sunshine router listening at {}", path.display());

        Ok(Self {
            socket,
            path: path.to_path_buf(),
        })
    }

    pub(crate) fn fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }

    pub(crate) fn drain_requests(&self) -> Result<Vec<SunshineTouchRequest>> {
        let mut requests = Vec::new();
        let mut buf = [0_u8; 512];

        loop {
            match self.socket.recv_from(&mut buf) {
                Ok((len, addr)) => {
                    let line = std::str::from_utf8(&buf[..len])
                        .context("parse Sunshine router request as UTF-8")?;
                    requests.push(parse_request(line.trim_end(), addr)?);
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => return Err(err).context("read Sunshine router socket"),
            }
        }

        Ok(requests)
    }

    pub(crate) fn reply(
        &self,
        request: &SunshineTouchRequest,
        decision: SunshineRouteDecision,
    ) -> Result<()> {
        let text = match decision {
            SunshineRouteDecision::App => "app",
            SunshineRouteDecision::TouchDeck => "touchdeck",
        };
        let response = format!("touchdeck-route-v1 seq={} {text}\n", request.seq);
        send_to_addr(&self.socket, response.as_bytes(), &request.addr)
            .map(|_| ())
            .context("reply Sunshine router request")
    }
}

impl Drop for SunshineRouter {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn send_to_addr(socket: &UnixDatagram, data: &[u8], addr: &SocketAddr) -> IoResult<usize> {
    if let Some(path) = addr.as_pathname() {
        socket.send_to(data, path)
    } else {
        Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "Sunshine router client did not use a pathname socket",
        ))
    }
}

fn parse_request(line: &str, addr: SocketAddr) -> Result<SunshineTouchRequest> {
    let mut fields = line.split_whitespace();
    if fields.next() != Some("touchdeck-route-v1") {
        return Err(anyhow!("unsupported Sunshine router request {line:?}"));
    }

    let mut output = None;
    let mut seq = None;
    let mut kind = None;
    let mut id = None;
    let mut x = None;
    let mut y = None;
    let mut width = None;
    let mut height = None;
    let mut time = None;

    for field in fields {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        match key {
            "output" => {
                if value != "-" {
                    output = Some(value.to_string());
                }
            }
            "seq" => seq = Some(value.parse().context("parse request seq")?),
            "event" => {
                kind = Some(match value {
                    "down" => SunshineTouchKind::Down,
                    "move" | "motion" => SunshineTouchKind::Motion,
                    "up" => SunshineTouchKind::Up,
                    "cancel" => SunshineTouchKind::Cancel,
                    other => return Err(anyhow!("unsupported Sunshine touch event {other:?}")),
                });
            }
            "id" => id = Some(value.parse().context("parse touch id")?),
            "x" => x = Some(value.parse().context("parse touch x")?),
            "y" => y = Some(value.parse().context("parse touch y")?),
            "width" => width = Some(value.parse().context("parse touch width")?),
            "height" => height = Some(value.parse().context("parse touch height")?),
            "time" => time = Some(value.parse().context("parse touch time")?),
            _ => {}
        }
    }

    Ok(SunshineTouchRequest {
        addr,
        seq: seq.unwrap_or(0),
        output,
        kind: kind.ok_or_else(|| anyhow!("Sunshine router request missing event"))?,
        id: id.ok_or_else(|| anyhow!("Sunshine router request missing id"))?,
        x: x.ok_or_else(|| anyhow!("Sunshine router request missing x"))?,
        y: y.ok_or_else(|| anyhow!("Sunshine router request missing y"))?,
        width: width.unwrap_or(1).max(1),
        height: height.unwrap_or(1).max(1),
        time: time.unwrap_or(0),
    })
}
