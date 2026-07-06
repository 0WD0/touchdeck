use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use x11rb::connection::Connection as X11Connection;
use x11rb::protocol::xproto::{KeyPressEvent, KEY_PRESS_EVENT};
use xim::x11rb::HasConnection;
use xim::{InputStyle, Server, ServerHandler, UserInputContext, XimConnections};

use super::key::KeyState;

const XIM_EVENT_MASK: u32 = 3;

#[derive(Clone, Copy, Debug)]
pub(super) struct XimPreeditArea {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) w: i32,
    pub(super) h: i32,
}

pub(super) enum XimRequest {
    Key {
        time: u32,
        hardware_keycode: u8,
        state_mask: u16,
        state: KeyState,
        client_window: u32,
        app_window: Option<u32>,
        focus_window: Option<u32>,
        spot_x: i32,
        spot_y: i32,
        preedit_area: Option<XimPreeditArea>,
        preedit_area_needed: Option<XimPreeditArea>,
        line_space: Option<u32>,
        response: Sender<XimKeyResponse>,
    },
    Reset {
        response: Sender<String>,
    },
}

#[derive(Debug, Default)]
pub(super) struct XimKeyResponse {
    pub(super) consumed: bool,
    pub(super) preedit: String,
    pub(super) commit: Option<String>,
}

struct TouchDeckXimHandler {
    tx: Sender<XimRequest>,
}

impl TouchDeckXimHandler {
    fn new(tx: Sender<XimRequest>) -> Self {
        Self { tx }
    }

    fn request_reset(&self) -> String {
        let (response_tx, response_rx) = mpsc::channel();
        if self
            .tx
            .send(XimRequest::Reset {
                response: response_tx,
            })
            .is_err()
        {
            return String::new();
        }
        response_rx
            .recv_timeout(Duration::from_millis(500))
            .unwrap_or_default()
    }
}

impl<C: xim::x11rb::HasConnection> ServerHandler<xim::x11rb::X11rbServer<C>>
    for TouchDeckXimHandler
{
    type InputContextData = ();
    type InputStyleArray = [InputStyle; 6];

    fn new_ic_data(
        &mut self,
        _server: &mut xim::x11rb::X11rbServer<C>,
        input_style: InputStyle,
    ) -> std::result::Result<Self::InputContextData, xim::ServerError> {
        eprintln!("touchdeck-ime: xim new input context data style={input_style:?}");
        Ok(())
    }

    fn input_styles(&self) -> Self::InputStyleArray {
        [
            InputStyle::PREEDIT_CALLBACKS | InputStyle::STATUS_CALLBACKS,
            InputStyle::PREEDIT_CALLBACKS | InputStyle::STATUS_NOTHING,
            InputStyle::PREEDIT_POSITION | InputStyle::STATUS_CALLBACKS,
            InputStyle::PREEDIT_POSITION | InputStyle::STATUS_NOTHING,
            InputStyle::PREEDIT_POSITION | InputStyle::STATUS_NONE,
            InputStyle::PREEDIT_NOTHING | InputStyle::STATUS_NOTHING,
        ]
    }

    fn filter_events(&self) -> u32 {
        XIM_EVENT_MASK
    }

    fn handle_connect(
        &mut self,
        _server: &mut xim::x11rb::X11rbServer<C>,
    ) -> std::result::Result<(), xim::ServerError> {
        eprintln!("touchdeck-ime: xim client connected");
        Ok(())
    }

    fn handle_create_ic(
        &mut self,
        server: &mut xim::x11rb::X11rbServer<C>,
        user_ic: &mut UserInputContext<Self::InputContextData>,
    ) -> std::result::Result<(), xim::ServerError> {
        eprintln!(
            "touchdeck-ime: xim create input context style={:?}",
            user_ic.ic.input_style()
        );
        server.set_event_mask(&user_ic.ic, XIM_EVENT_MASK, 0)
    }

    fn handle_destroy_ic(
        &mut self,
        _server: &mut xim::x11rb::X11rbServer<C>,
        _user_ic: UserInputContext<Self::InputContextData>,
    ) -> std::result::Result<(), xim::ServerError> {
        eprintln!("touchdeck-ime: xim destroy input context");
        Ok(())
    }

    fn handle_reset_ic(
        &mut self,
        _server: &mut xim::x11rb::X11rbServer<C>,
        _user_ic: &mut UserInputContext<Self::InputContextData>,
    ) -> std::result::Result<String, xim::ServerError> {
        eprintln!("touchdeck-ime: xim reset input context");
        Ok(self.request_reset())
    }

    fn handle_set_focus(
        &mut self,
        _server: &mut xim::x11rb::X11rbServer<C>,
        _user_ic: &mut UserInputContext<Self::InputContextData>,
    ) -> std::result::Result<(), xim::ServerError> {
        eprintln!("touchdeck-ime: xim focus in");
        Ok(())
    }

    fn handle_unset_focus(
        &mut self,
        _server: &mut xim::x11rb::X11rbServer<C>,
        _user_ic: &mut UserInputContext<Self::InputContextData>,
    ) -> std::result::Result<(), xim::ServerError> {
        eprintln!("touchdeck-ime: xim focus out");
        self.request_reset();
        Ok(())
    }

    fn handle_set_ic_values(
        &mut self,
        _server: &mut xim::x11rb::X11rbServer<C>,
        user_ic: &mut UserInputContext<Self::InputContextData>,
    ) -> std::result::Result<(), xim::ServerError> {
        eprintln!(
            "touchdeck-ime: xim set input context values style={:?} spot=({},{}) area={} area_needed={} line_space={:?}",
            user_ic.ic.input_style(),
            user_ic.ic.preedit_spot().x,
            user_ic.ic.preedit_spot().y,
            format_preedit_area(user_ic.ic.preedit_area()),
            format_preedit_area(user_ic.ic.preedit_area_needed()),
            user_ic.ic.preedit_line_space()
        );
        Ok(())
    }

    fn handle_forward_event(
        &mut self,
        server: &mut xim::x11rb::X11rbServer<C>,
        user_ic: &mut UserInputContext<Self::InputContextData>,
        xev: &KeyPressEvent,
    ) -> std::result::Result<bool, xim::ServerError> {
        let state = if xev.response_type == KEY_PRESS_EVENT {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        eprintln!(
            "touchdeck-ime: xim forward key detail={} state={state:?} mask={} time={}",
            xev.detail,
            u16::from(xev.state),
            xev.time
        );
        let (response_tx, response_rx) = mpsc::channel();
        if self
            .tx
            .send(XimRequest::Key {
                time: xev.time,
                hardware_keycode: xev.detail,
                state_mask: u16::from(xev.state),
                state,
                client_window: user_ic.ic.client_win(),
                app_window: user_ic.ic.app_win().map(|window| window.get()),
                focus_window: user_ic.ic.app_focus_win().map(|window| window.get()),
                spot_x: i32::from(user_ic.ic.preedit_spot().x),
                spot_y: i32::from(user_ic.ic.preedit_spot().y),
                preedit_area: user_ic.ic.preedit_area().map(|area| XimPreeditArea {
                    x: i32::from(area.x),
                    y: i32::from(area.y),
                    w: i32::from(area.width),
                    h: i32::from(area.height),
                }),
                preedit_area_needed: user_ic.ic.preedit_area_needed().map(|area| XimPreeditArea {
                    x: i32::from(area.x),
                    y: i32::from(area.y),
                    w: i32::from(area.width),
                    h: i32::from(area.height),
                }),
                line_space: user_ic.ic.preedit_line_space(),
                response: response_tx,
            })
            .is_err()
        {
            return Ok(false);
        }

        let response = match response_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(response) => response,
            Err(_) => return Ok(false),
        };

        server.preedit_draw(&mut user_ic.ic, &response.preedit)?;
        if let Some(commit) = response.commit {
            eprintln!("touchdeck-ime: xim commit {commit:?}");
            server.commit(&user_ic.ic, &commit)?;
        }
        eprintln!(
            "touchdeck-ime: xim key consumed={} preedit={:?}",
            response.consumed, response.preedit
        );
        Ok(response.consumed)
    }
}

fn format_preedit_area(area: Option<xim::Rectangle>) -> String {
    match area {
        Some(area) => format!("({},{} {}x{})", area.x, area.y, area.width, area.height),
        None => "none".to_string(),
    }
}

pub(super) fn spawn_xim_server(tx: Sender<XimRequest>) {
    thread::spawn(move || {
        if let Err(err) = run_xim_server(tx) {
            eprintln!("touchdeck-ime: xim server stopped: {err:?}");
        }
    });
}

fn run_xim_server(tx: Sender<XimRequest>) -> Result<()> {
    let (conn, screen_num) = x11rb::rust_connection::RustConnection::connect(None)
        .context("connect to X display for XIM")?;
    let mut server = xim::x11rb::X11rbServer::init(conn, screen_num, "touchdeck", xim::ALL_LOCALES)
        .context("initialize XIM server")?;
    let mut connections = XimConnections::new();
    let mut handler = TouchDeckXimHandler::new(tx);

    eprintln!("touchdeck-ime: xim server initialized");
    loop {
        let event = server.conn().wait_for_event().context("wait for X event")?;
        match server.filter_event(&event, &mut connections, &mut handler) {
            Ok(_) => {
                if let Err(err) = server.conn().flush() {
                    eprintln!("touchdeck-ime: xim flush error: {err}");
                }
            }
            Err(err) => eprintln!("touchdeck-ime: xim event error: {err}"),
        }
    }
}
