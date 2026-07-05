use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_manager_v2::{self, ZwpInputMethodManagerV2},
    zwp_input_method_v2::{self, ZwpInputMethodV2},
};

#[derive(Default)]
struct ImeApp {
    seat: Option<wl_seat::WlSeat>,
    input_method_manager: Option<ZwpInputMethodManagerV2>,
    input_method: Option<ZwpInputMethodV2>,
    active: bool,
    serial: u32,
    composing: String,
    running: bool,
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
    let event_rx = spawn_socket_listener(socket_path)?;

    let conn = Connection::connect_to_env().context("connect to Wayland display")?;
    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();

    let mut app = ImeApp {
        running: true,
        ..Default::default()
    };

    display.get_registry(&qh, ());
    event_queue
        .roundtrip(&mut app)
        .context("collect Wayland globals")?;
    app.maybe_init_input_method(&qh);

    eprintln!("touchdeck-ime: ready; waiting for input-method activation");

    while app.running {
        event_queue
            .dispatch_pending(&mut app)
            .context("dispatch pending Wayland events")?;

        while let Ok(event) = event_rx.try_recv() {
            app.handle_touchdeck_event(event);
        }

        event_queue.flush().context("flush Wayland requests")?;
        let Some(guard) = event_queue.prepare_read() else {
            continue;
        };
        event_queue.flush().context("flush Wayland requests")?;
        if poll_fd(event_queue.as_fd().as_raw_fd(), Some(Duration::from_millis(16)))
            .context("poll Wayland fd")?
        {
            guard.read().context("read Wayland events")?;
        }
    }

    Ok(())
}

impl ImeApp {
    fn maybe_init_input_method(&mut self, qh: &QueueHandle<Self>) {
        if self.input_method.is_some() {
            return;
        }

        let (Some(manager), Some(seat)) = (&self.input_method_manager, &self.seat) else {
            return;
        };

        self.input_method = Some(manager.get_input_method(seat, qh, ()));
        eprintln!("touchdeck-ime: input-method-v2 object created");
    }

    fn handle_touchdeck_event(&mut self, event: TouchDeckEvent) {
        if event.protocol != "touchdeck-ime-v1" || event.kind != "key" || event.source != "touchdeck" {
            eprintln!("touchdeck-ime: ignored unsupported event {event:?}");
            return;
        }

        if event.state != "pressed" {
            return;
        }

        if !self.active {
            eprintln!(
                "touchdeck-ime: ignored key {} because input method is inactive",
                event.key
            );
            return;
        }

        match event.key {
            1 => {
                self.composing.clear();
                self.update_preedit();
            }
            14 => {
                self.composing.pop();
                self.update_preedit();
            }
            28 | 57 => {
                if self.composing.is_empty() {
                    eprintln!("touchdeck-ime: nothing to commit for key {}", event.key);
                } else {
                    self.commit_composing();
                }
            }
            key => {
                if let Some(ch) = evdev_key_to_ascii(key, event.modifiers) {
                    self.composing.push(ch);
                    self.update_preedit();
                }
            }
        }

        eprintln!(
            "touchdeck-ime: key={} time={} modifiers={} composing={:?}",
            event.key, event.time, event.modifiers, self.composing
        );
    }

    fn update_preedit(&self) {
        let Some(input_method) = &self.input_method else {
            return;
        };

        let cursor = self.composing.len().min(i32::MAX as usize) as i32;
        input_method.set_preedit_string(self.composing.clone(), cursor, cursor);
        input_method.commit(self.serial);
    }

    fn commit_composing(&mut self) {
        let Some(input_method) = &self.input_method else {
            self.composing.clear();
            return;
        };

        let text = std::mem::take(&mut self.composing);
        eprintln!("touchdeck-ime: commit {text:?}");
        input_method.set_preedit_string(String::new(), 0, 0);
        input_method.commit_string(text);
        input_method.commit(self.serial);
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for ImeApp {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };

        match interface.as_str() {
            "wl_seat" if state.seat.is_none() => {
                state.seat = Some(registry.bind::<wl_seat::WlSeat, _, _>(
                    name,
                    version.min(9),
                    qh,
                    (),
                ));
                eprintln!("touchdeck-ime: bound wl_seat");
                state.maybe_init_input_method(qh);
            }
            "zwp_input_method_manager_v2" if state.input_method_manager.is_none() => {
                state.input_method_manager =
                    Some(registry.bind::<ZwpInputMethodManagerV2, _, _>(
                        name,
                        version.min(1),
                        qh,
                        (),
                    ));
                eprintln!("touchdeck-ime: bound zwp_input_method_manager_v2");
                state.maybe_init_input_method(qh);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for ImeApp {
    fn event(
        _state: &mut Self,
        _seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        eprintln!("touchdeck-ime: seat event {event:?}");
    }
}

impl Dispatch<ZwpInputMethodManagerV2, ()> for ImeApp {
    fn event(
        _state: &mut Self,
        _manager: &ZwpInputMethodManagerV2,
        _event: zwp_input_method_manager_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for ImeApp {
    fn event(
        state: &mut Self,
        _input_method: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => {
                state.active = true;
                state.composing.clear();
                eprintln!("touchdeck-ime: activate");
            }
            zwp_input_method_v2::Event::Deactivate => {
                state.active = false;
                state.composing.clear();
                eprintln!("touchdeck-ime: deactivate");
            }
            zwp_input_method_v2::Event::SurroundingText {
                text,
                cursor,
                anchor,
            } => {
                eprintln!(
                    "touchdeck-ime: surrounding text len={} cursor={} anchor={}",
                    text.len(),
                    cursor,
                    anchor
                );
            }
            zwp_input_method_v2::Event::TextChangeCause { cause } => {
                eprintln!("touchdeck-ime: text change cause {cause:?}");
            }
            zwp_input_method_v2::Event::ContentType { hint, purpose } => {
                eprintln!("touchdeck-ime: content type hint={hint:?} purpose={purpose:?}");
            }
            zwp_input_method_v2::Event::Done => {
                state.serial = state.serial.wrapping_add(1);
                eprintln!(
                    "touchdeck-ime: done serial={} active={}",
                    state.serial, state.active
                );
            }
            zwp_input_method_v2::Event::Unavailable => {
                eprintln!("touchdeck-ime: input method unavailable");
                state.running = false;
            }
            _ => {}
        }
    }
}

fn spawn_socket_listener(socket_path: PathBuf) -> Result<Receiver<TouchDeckEvent>> {
    if socket_path.exists() {
        fs::remove_file(&socket_path)
            .with_context(|| format!("remove stale socket {}", socket_path.display()))?;
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

fn default_socket_path() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("touchdeck-ime.sock")
}

fn handle_client(stream: UnixStream, tx: Sender<TouchDeckEvent>) -> Result<()> {
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line.context("read touchdeck-ime line")?;
        if line.trim().is_empty() {
            continue;
        }

        let event: TouchDeckEvent =
            serde_json::from_str(&line).with_context(|| format!("parse event {line}"))?;
        if tx.send(event).is_err() {
            break;
        }
    }

    Ok(())
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

fn poll_fd(fd: RawFd, timeout: Option<Duration>) -> Result<bool> {
    let timeout_ms = timeout
        .map(|timeout| timeout.as_millis().min(i32::MAX as u128) as i32)
        .unwrap_or(-1);
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };

    loop {
        let rc = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
        if rc > 0 {
            return Ok(true);
        }
        if rc == 0 {
            return Ok(false);
        }

        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(err).context("poll failed");
    }
}
