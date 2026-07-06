use std::env;
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::raw::c_int;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use touchdeck::protocol::{ImeCursorRect, ImeStatus};
use touchdeck::x11_geometry::{X11GeometryProbe, X11WindowGeometry};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_region, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::{self, ZwpInputMethodManagerV2},
    zwp_input_method_v2::{self, ZwpInputMethodV2},
    zwp_input_popup_surface_v2::{self, ZwpInputPopupSurfaceV2},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::{self, ZwpVirtualKeyboardManagerV1},
    zwp_virtual_keyboard_v1::{self, ZwpVirtualKeyboardV1},
};
use x11rb::connection::Connection as X11Connection;
use x11rb::protocol::xproto::{KeyPressEvent, KEY_PRESS_EVENT};
use xim::x11rb::HasConnection;
use xim::{InputStyle, Server, ServerHandler, UserInputContext, XimConnections};

#[path = "touchdeck-ime/fcitx_dbus.rs"]
mod fcitx_dbus;
#[path = "touchdeck-ime/config.rs"]
mod ime_config;
#[path = "touchdeck-ime/key.rs"]
mod ime_key;
#[path = "touchdeck-ime/popup.rs"]
mod popup;
#[path = "touchdeck-ime/rime_engine.rs"]
mod rime_engine;
#[path = "touchdeck-ime/touchdeck_socket.rs"]
mod touchdeck_socket;

use fcitx_dbus::*;
use ime_config::*;
use ime_key::*;
use popup::*;
use rime_engine::*;
use touchdeck_socket::*;

#[derive(Default)]
struct ImeApp {
    config: ImeRuntimeConfig,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    input_method_manager: Option<ZwpInputMethodManagerV2>,
    virtual_keyboard_manager: Option<ZwpVirtualKeyboardManagerV1>,
    input_method: Option<ZwpInputMethodV2>,
    popup_surface: Option<wl_surface::WlSurface>,
    input_popup_surface: Option<ZwpInputPopupSurfaceV2>,
    keyboard_grab: Option<ZwpInputMethodKeyboardGrabV2>,
    virtual_keyboard: Option<ZwpVirtualKeyboardV1>,
    popup_renderer: PopupRenderer,
    rime: Option<RimeEngine>,
    active: bool,
    serial: u32,
    preedit: String,
    status: ImeStatus,
    status_subscribers: Vec<Sender<ImeStatus>>,
    fcitx_output_tx: Option<Sender<FcitxDbusOutput>>,
    fcitx_focus: Option<FcitxDbusTarget>,
    fcitx_cursor_rect: Option<FcitxCursorRect>,
    fcitx_capability: u64,
    fcitx_supported_capability: u64,
    x11_geometry: Option<X11GeometryProbe>,
    physical_modifiers: u32,
    virtual_keyboard_has_keymap: bool,
    running: bool,
}

enum XimRequest {
    Key {
        time: u32,
        hardware_keycode: u8,
        state_mask: u16,
        state: KeyState,
        response: Sender<XimKeyResponse>,
    },
    Reset {
        response: Sender<String>,
    },
}

#[derive(Debug, Default)]
struct XimKeyResponse {
    consumed: bool,
    preedit: String,
    commit: Option<String>,
}

fn main() -> Result<()> {
    let config = load_ime_config().context("load touchdeck-ime config")?;
    let socket_path = env::var_os("TOUCHDECK_IME_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(default_socket_path);
    let event_rx = spawn_socket_listener(socket_path)?;
    let (xim_tx, xim_rx) = mpsc::channel();
    if env::var("TOUCHDECK_IME_XIM").ok().as_deref() != Some("0") {
        spawn_xim_server(xim_tx);
    }
    let (fcitx_tx, fcitx_rx) = mpsc::channel();
    let fcitx_output_tx = if env::var("TOUCHDECK_IME_FCITX_DBUS").ok().as_deref() != Some("0") {
        Some(spawn_fcitx_dbus_server(fcitx_tx))
    } else {
        None
    };

    let conn = Connection::connect_to_env().context("connect to Wayland display")?;
    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();

    let key_translation = config.key_translation;
    let mut app = ImeApp {
        config,
        rime: Some(RimeEngine::new(key_translation).context("initialize librime")?),
        fcitx_output_tx,
        x11_geometry: X11GeometryProbe::connect().ok(),
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
        event_queue
            .flush()
            .context("flush Wayland configure acknowledgements")?;

        while let Ok(request) = event_rx.try_recv() {
            match request {
                TouchDeckRequest::Event { event, response } => {
                    let status = app.handle_touchdeck_event(&qh, event);
                    app.broadcast_status("touchdeck");
                    let _ = response.send(status);
                }
                TouchDeckRequest::Subscribe { response } => {
                    app.add_status_subscriber(response);
                }
            }
        }

        while let Ok(request) = xim_rx.try_recv() {
            app.handle_xim_request(request);
        }

        while let Ok(request) = fcitx_rx.try_recv() {
            app.handle_fcitx_dbus_request(&qh, request);
        }

        event_queue.flush().context("flush Wayland requests")?;
        let Some(guard) = event_queue.prepare_read() else {
            continue;
        };
        event_queue.flush().context("flush Wayland requests")?;
        if poll_fd(
            event_queue.as_fd().as_raw_fd(),
            Some(Duration::from_millis(16)),
        )
        .context("poll Wayland fd")?
        {
            guard.read().context("read Wayland events")?;
        }
    }

    Ok(())
}

impl ImeApp {
    fn maybe_init_input_method(&mut self, qh: &QueueHandle<Self>) {
        if self.input_method.is_none() {
            let (Some(manager), Some(seat)) = (&self.input_method_manager, &self.seat) else {
                return;
            };

            self.input_method = Some(manager.get_input_method(seat, qh, ()));
            eprintln!("touchdeck-ime: input-method-v2 object created");
        }

        if self.virtual_keyboard.is_none() {
            if let (Some(manager), Some(seat)) = (&self.virtual_keyboard_manager, &self.seat) {
                self.virtual_keyboard = Some(manager.create_virtual_keyboard(seat, qh, ()));
                eprintln!("touchdeck-ime: virtual-keyboard object created");
            }
        }

        if self.keyboard_grab.is_none()
            && self.input_method.is_some()
            && self.virtual_keyboard.is_some()
        {
            if let Some(input_method) = &self.input_method {
                self.keyboard_grab = Some(input_method.grab_keyboard(qh, ()));
                eprintln!("touchdeck-ime: input-method keyboard grab created");
            }
        } else if self.input_method.is_some()
            && self.virtual_keyboard_manager.is_none()
            && self.keyboard_grab.is_none()
        {
            eprintln!(
                "touchdeck-ime: no virtual-keyboard manager yet; physical keyboard grab disabled"
            );
        }
    }

    fn ensure_popup(&mut self, qh: &QueueHandle<Self>) -> Result<bool> {
        if self.popup_surface.is_some() && self.input_popup_surface.is_some() {
            return Ok(true);
        }

        let (Some(compositor), Some(input_method)) = (&self.compositor, &self.input_method) else {
            return Ok(false);
        };

        let surface = compositor.create_surface(qh, ());
        let region = compositor.create_region(qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();

        let input_popup = input_method.get_input_popup_surface(&surface, qh, ());
        self.popup_surface = Some(surface);
        self.input_popup_surface = Some(input_popup);
        eprintln!("touchdeck-ime: input popup surface created");

        Ok(true)
    }

    fn hide_popup(&mut self, _qh: &QueueHandle<Self>) {
        if let Some(surface) = &self.popup_surface {
            self.popup_renderer.hide_surface(surface);
        }
    }

    fn update_popup(&mut self, qh: &QueueHandle<Self>, source: &str) -> Result<()> {
        if source != "physical" {
            self.hide_popup(qh);
            return Ok(());
        }

        let status = self.current_status_with_source(source);
        if !status.active || status_is_empty(&status) {
            self.hide_popup(qh);
            return Ok(());
        }

        if self.fcitx_uses_client_side_input_panel() {
            self.hide_popup(qh);
            return Ok(());
        }

        if self.fcitx_focus.is_some() {
            self.hide_popup(qh);
            return Ok(());
        }

        if !self.ensure_popup(qh)? {
            return Ok(());
        }

        self.render_input_popup(qh, &status)
    }

    fn render_input_popup(&mut self, qh: &QueueHandle<Self>, status: &ImeStatus) -> Result<()> {
        let surface = self
            .popup_surface
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("input popup surface is unavailable"))?;
        self.render_popup_to_surface(qh, &surface, status)
    }

    fn render_popup_to_surface(
        &mut self,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        status: &ImeStatus,
    ) -> Result<()> {
        let shm = self
            .shm
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("wl_shm global is unavailable"))?;
        let popup = self.config.popup.clone();
        self.popup_renderer
            .render_to_surface(qh, &shm, surface, status, &popup)
    }

    fn handle_physical_key(
        &mut self,
        qh: &QueueHandle<Self>,
        time: u32,
        key: u32,
        state: WEnum<wl_keyboard::KeyState>,
    ) {
        let Some(key_state) = parse_wayland_key_state(&state) else {
            self.passthrough_physical_key(time, key, state);
            return;
        };

        if !self.active {
            self.passthrough_physical_key(time, key, state);
            return;
        }

        let Some(keysym) = evdev_key_to_keysym(key) else {
            self.passthrough_physical_key(time, key, state);
            return;
        };
        if self.rime_state_is_empty() && is_empty_state_passthrough_key(keysym) {
            self.passthrough_physical_key(time, key, state);
            return;
        }

        let output = {
            let Some(rime) = self.rime.as_mut() else {
                self.passthrough_physical_key(time, key, state);
                return;
            };

            match rime.process_key(keysym, key_state, self.physical_modifiers, None) {
                Ok(output) => output,
                Err(err) => {
                    eprintln!("touchdeck-ime: rime error for physical key {key}: {err:?}");
                    self.passthrough_physical_key(time, key, state);
                    return;
                }
            }
        };

        let handled = output.handled;
        self.apply_rime_output(output);

        if !handled {
            self.passthrough_physical_key(time, key, state);
        }
        if self.fcitx_uses_client_side_input_panel() {
            self.hide_popup(qh);
        } else if let Err(err) = self.update_popup(qh, "physical") {
            eprintln!("touchdeck-ime: failed to update popup: {err:?}");
        }

        eprintln!(
            "touchdeck-ime: physical key={} state={:?} modifiers={} handled={} preedit={:?}",
            key, key_state, self.physical_modifiers, handled, self.preedit
        );
        self.broadcast_status("physical");
    }

    fn passthrough_physical_key(&self, time: u32, key: u32, state: WEnum<wl_keyboard::KeyState>) {
        if let Some(virtual_keyboard) = &self.virtual_keyboard {
            virtual_keyboard.key(time, key, state.into());
        }
    }

    fn passthrough_physical_modifiers(
        &self,
        mods_depressed: u32,
        mods_latched: u32,
        mods_locked: u32,
        group: u32,
    ) {
        if let Some(virtual_keyboard) = &self.virtual_keyboard {
            virtual_keyboard.modifiers(mods_depressed, mods_latched, mods_locked, group);
        }
    }

    fn passthrough_touchdeck_key(&self, time: u32, key: u32, state: KeyState) {
        if let Some(virtual_keyboard) = &self.virtual_keyboard {
            virtual_keyboard.key(
                time,
                key,
                match state {
                    KeyState::Pressed => 1,
                    KeyState::Released => 0,
                },
            );
        }
    }

    fn handle_xim_request(&mut self, request: XimRequest) {
        match request {
            XimRequest::Key {
                time,
                hardware_keycode,
                state_mask,
                state,
                response,
            } => {
                let result = self.handle_xim_key(time, hardware_keycode, state_mask, state);
                let _ = response.send(result);
            }
            XimRequest::Reset { response } => {
                if let Some(rime) = self.rime.as_mut() {
                    rime.clear();
                }
                self.preedit.clear();
                self.status = ImeStatus::default();
                self.broadcast_status("xim");
                let _ = response.send(String::new());
            }
        }
    }

    fn handle_fcitx_dbus_request(&mut self, qh: &QueueHandle<Self>, request: FcitxDbusRequest) {
        match request {
            FcitxDbusRequest::FocusIn { target, response } => {
                self.status.source = "fcitx-dbus".to_string();
                eprintln!(
                    "touchdeck-ime: fcitx dbus focus in path={} client={} display={}",
                    target.path.as_str(),
                    target.client.as_str(),
                    target.display
                );
                self.fcitx_focus = Some(target);
                self.broadcast_status("fcitx-dbus");
                let _ = response.send(());
            }
            FcitxDbusRequest::FocusOut { target, response } => {
                eprintln!(
                    "touchdeck-ime: fcitx dbus focus out path={} client={} display={}",
                    target.path.as_str(),
                    target.client.as_str(),
                    target.display
                );
                if self
                    .fcitx_focus
                    .as_ref()
                    .map(|focus| focus.matches(&target))
                    .unwrap_or(false)
                {
                    self.fcitx_focus = None;
                    self.fcitx_cursor_rect = None;
                    self.fcitx_capability = 0;
                    self.fcitx_supported_capability = 0;
                }
                if !self.active {
                    if let Some(rime) = self.rime.as_mut() {
                        rime.clear();
                    }
                    self.clear_preedit();
                    self.status.active = false;
                }
                self.broadcast_status("fcitx-dbus");
                let _ = response.send(());
            }
            FcitxDbusRequest::Reset { response } => {
                if let Some(rime) = self.rime.as_mut() {
                    rime.clear();
                }
                self.clear_preedit();
                self.broadcast_status("fcitx-dbus");
                let _ = response.send(());
            }
            FcitxDbusRequest::Key {
                keyval,
                keycode,
                state,
                is_release,
                time,
                response,
            } => {
                let result = self.handle_fcitx_dbus_key(keyval, keycode, state, is_release, time);
                let _ = response.send(result);
            }
            FcitxDbusRequest::SetCursorRect {
                target,
                x,
                y,
                w,
                h,
                scale,
            } => {
                eprintln!(
                    "touchdeck-ime: fcitx dbus cursor rect path={} client={} display={} x={x} y={y} w={w} h={h} scale={scale}",
                    target.path.as_str(),
                    target.client.as_str(),
                    target.display,
                );
                if self
                    .fcitx_focus
                    .as_ref()
                    .map(|focus| focus.matches(&target))
                    .unwrap_or(false)
                {
                    let is_x11 = target.display.starts_with("x11:");
                    let x11_window = if is_x11 {
                        self.query_x11_active_window_geometry()
                    } else {
                        None
                    };
                    if is_x11 && x11_window.is_none() {
                        eprintln!(
                            "touchdeck-ime: x11 cursor rect has no active-window geometry; server popup will not guess"
                        );
                    }
                    self.fcitx_cursor_rect = Some(FcitxCursorRect {
                        target,
                        x,
                        y,
                        w,
                        h,
                        scale,
                        space: if is_x11 { "x11-root" } else { "surface" }.to_string(),
                        x11_window,
                    });
                    if self.active && !status_is_empty(&self.status) {
                        if let Err(err) = self.update_popup(qh, "physical") {
                            eprintln!(
                                "touchdeck-ime: failed to update popup after cursor rect: {err:?}"
                            );
                        }
                        self.broadcast_status("physical");
                    }
                }
            }
            FcitxDbusRequest::SetCapability { target, capability } => {
                let client_side = (capability & FCITX_CAPABILITY_CLIENT_SIDE_INPUT_PANEL) != 0;
                eprintln!(
                    "touchdeck-ime: fcitx dbus capability path={} client={} capability=0x{capability:x} client_side_input_panel={client_side}",
                    target.path.as_str(),
                    target.client.as_str()
                );
                if self
                    .fcitx_focus
                    .as_ref()
                    .map(|focus| focus.matches(&target))
                    .unwrap_or(false)
                {
                    self.fcitx_capability = capability;
                    self.broadcast_status("physical");
                }
            }
            FcitxDbusRequest::SetSupportedCapability { target, capability } => {
                let client_side = (capability & FCITX_CAPABILITY_CLIENT_SIDE_INPUT_PANEL) != 0;
                eprintln!(
                    "touchdeck-ime: fcitx dbus supported capability path={} client={} capability=0x{capability:x} client_side_input_panel={client_side}",
                    target.path.as_str(),
                    target.client.as_str()
                );
                if self
                    .fcitx_focus
                    .as_ref()
                    .map(|focus| focus.matches(&target))
                    .unwrap_or(false)
                {
                    self.fcitx_supported_capability = capability;
                    self.broadcast_status("physical");
                }
            }
            FcitxDbusRequest::SetSurroundingText {
                text,
                cursor,
                anchor,
            } => {
                eprintln!(
                    "touchdeck-ime: fcitx dbus surrounding text len={} cursor={} anchor={}",
                    text.len(),
                    cursor,
                    anchor
                );
            }
        }
    }

    fn handle_fcitx_dbus_key(
        &mut self,
        keyval: u32,
        keycode: u32,
        state: u32,
        is_release: bool,
        time: u32,
    ) -> FcitxDbusKeyResponse {
        let key_state = if is_release {
            KeyState::Released
        } else {
            KeyState::Pressed
        };

        if self.rime_state_is_empty() && is_empty_state_passthrough_key(keyval) {
            return FcitxDbusKeyResponse::default();
        }

        let output = {
            let Some(rime) = self.rime.as_mut() else {
                eprintln!("touchdeck-ime: rime engine unavailable for fcitx dbus key");
                return FcitxDbusKeyResponse::default();
            };

            match rime.process_key(keyval, key_state, state, Some(KeyTranslationPolicy::Raw)) {
                Ok(output) => output,
                Err(err) => {
                    eprintln!(
                        "touchdeck-ime: rime error for fcitx dbus keyval={keyval} keycode={keycode}: {err:?}"
                    );
                    return FcitxDbusKeyResponse::default();
                }
            }
        };

        let handled = output.handled;
        let preedit = output.status.preedit.clone();
        let commit = output.commit;

        self.status = output.status;
        self.status.active = true;
        self.status.source = "fcitx-dbus".to_string();
        self.preedit = preedit.clone();
        let status = self.status.clone();
        self.broadcast_status("fcitx-dbus");

        eprintln!(
            "touchdeck-ime: fcitx dbus keyval={keyval} keycode={keycode} state={state} release={is_release} time={time} handled={handled} preedit={:?}",
            self.preedit
        );

        FcitxDbusKeyResponse {
            handled,
            preedit,
            commit,
            status,
        }
    }

    fn handle_xim_key(
        &mut self,
        time: u32,
        hardware_keycode: u8,
        state_mask: u16,
        state: KeyState,
    ) -> XimKeyResponse {
        let Some(keysym) = x_keycode_to_keysym(hardware_keycode) else {
            eprintln!(
                "touchdeck-ime: xim forward unknown hardware keycode {}",
                hardware_keycode
            );
            return XimKeyResponse::default();
        };

        if self.rime_state_is_empty() && is_empty_state_passthrough_key(keysym) {
            return XimKeyResponse::default();
        }

        let output = {
            let Some(rime) = self.rime.as_mut() else {
                eprintln!("touchdeck-ime: rime engine unavailable for xim key");
                return XimKeyResponse::default();
            };

            match rime.process_key(keysym, state, u32::from(state_mask), None) {
                Ok(output) => output,
                Err(err) => {
                    eprintln!(
                        "touchdeck-ime: rime error for xim keycode {}: {err:?}",
                        hardware_keycode
                    );
                    return XimKeyResponse::default();
                }
            }
        };

        let consumed = output.handled;
        let preedit = output.status.preedit.clone();
        let commit = output.commit;

        self.status = output.status;
        self.status.active = true;
        self.status.source = "xim".to_string();
        self.preedit = preedit.clone();
        self.broadcast_status("xim");

        eprintln!(
            "touchdeck-ime: xim keycode={} keysym={} state={:?} time={} modifiers={} consumed={} preedit={:?}",
            hardware_keycode, keysym, state, time, state_mask, consumed, self.preedit
        );

        XimKeyResponse {
            consumed,
            preedit,
            commit,
        }
    }

    fn handle_touchdeck_event(
        &mut self,
        qh: &QueueHandle<Self>,
        event: TouchDeckEvent,
    ) -> ImeStatus {
        if event.protocol != "touchdeck-ime-v1"
            || event.kind != "key"
            || event.source != "touchdeck"
        {
            eprintln!("touchdeck-ime: ignored unsupported event {event:?}");
            return self.current_status_with_source("touchdeck");
        }

        let Some(state) = parse_key_state(&event.state) else {
            eprintln!("touchdeck-ime: ignored key with unknown state {event:?}");
            return self.current_status_with_source("touchdeck");
        };

        let Some(keysym) = evdev_key_to_keysym(event.key) else {
            eprintln!("touchdeck-ime: ignored unknown evdev key {}", event.key);
            return self.current_status_with_source("touchdeck");
        };

        let route = match event.route.as_deref() {
            Some(value) => match parse_key_route(value) {
                Ok(route) => route,
                Err(err) => {
                    eprintln!("touchdeck-ime: ignored invalid key route: {err:?}");
                    KeyRoute::ImeKey
                }
            },
            None => KeyRoute::ImeKey,
        };

        let translation = match event.translation.as_deref() {
            Some(value) => match parse_key_translation_policy(value) {
                Ok(policy) => Some(policy),
                Err(err) => {
                    eprintln!("touchdeck-ime: ignored invalid key translation policy: {err:?}");
                    None
                }
            },
            None => None,
        };

        if !self.active {
            match route {
                KeyRoute::ImeOnly => {
                    eprintln!(
                        "touchdeck-ime: ignored ime-only key {} because input method is inactive",
                        event.key
                    );
                }
                KeyRoute::ImeKey | KeyRoute::ImeText | KeyRoute::AppKey => {
                    self.passthrough_touchdeck_key(event.time, event.key, state);
                }
            }
            return self.current_status_with_source("touchdeck");
        }

        match route {
            KeyRoute::AppKey => {
                self.passthrough_touchdeck_key(event.time, event.key, state);
                self.hide_popup(qh);
                return self.current_status_with_source("touchdeck");
            }
            KeyRoute::ImeText => {
                self.commit_touchdeck_text_or_forward(
                    event.time,
                    event.key,
                    keysym,
                    state,
                    event.modifiers,
                );
                self.hide_popup(qh);
                return self.current_status_with_source("touchdeck");
            }
            KeyRoute::ImeKey | KeyRoute::ImeOnly => {}
        }

        let output = {
            let Some(rime) = self.rime.as_mut() else {
                eprintln!("touchdeck-ime: rime engine unavailable");
                if route == KeyRoute::ImeKey {
                    self.passthrough_touchdeck_key(event.time, event.key, state);
                }
                return self.current_status_with_source("touchdeck");
            };

            match rime.process_key(keysym, state, event.modifiers, translation) {
                Ok(output) => output,
                Err(err) => {
                    eprintln!("touchdeck-ime: rime error for key {}: {err:?}", event.key);
                    if route == KeyRoute::ImeKey {
                        self.passthrough_touchdeck_key(event.time, event.key, state);
                    }
                    return self.current_status_with_source("touchdeck");
                }
            }
        };

        let handled = output.handled;
        self.apply_rime_output(output);

        if !handled && route == KeyRoute::ImeKey {
            self.passthrough_touchdeck_key(event.time, event.key, state);
        }

        eprintln!(
            "touchdeck-ime: key={} state={:?} time={} modifiers={} route={:?} handled={} preedit={:?}",
            event.key, state, event.time, event.modifiers, route, handled, self.preedit
        );

        self.hide_popup(qh);
        self.current_status_with_source("touchdeck")
    }

    fn apply_rime_output(&mut self, output: RimeOutput) {
        let preedit = output.status.preedit.clone();
        let commit = output.commit;
        let status = output.status.clone();
        self.status = output.status;
        self.status.active = self.active;

        if self.fcitx_focus.is_some() {
            self.emit_fcitx_output(preedit, commit, status);
            return;
        }

        if preedit != self.preedit {
            self.set_preedit(preedit);
        }

        if let Some(text) = commit {
            self.commit_text(text);
        }
    }

    fn fcitx_uses_client_side_input_panel(&self) -> bool {
        self.fcitx_focus.is_some()
            && (self.fcitx_capability & FCITX_CAPABILITY_CLIENT_SIDE_INPUT_PANEL) != 0
    }

    fn emit_fcitx_output(&mut self, preedit: String, commit: Option<String>, status: ImeStatus) {
        let target = match self.fcitx_focus.clone() {
            Some(target) => target,
            None => return,
        };

        let preedit_changed = preedit != self.preedit;
        self.preedit = preedit.clone();

        if preedit_changed || commit.is_some() {
            if let Some(tx) = &self.fcitx_output_tx {
                let cursor_rect = self
                    .fcitx_cursor_rect
                    .as_ref()
                    .filter(|rect| rect.target.matches(&target))
                    .cloned();
                eprintln!(
                    "touchdeck-ime: fcitx dbus output path={} commit={:?} preedit={:?} cursor_rect={:?}",
                    target.path.as_str(),
                    commit,
                    preedit,
                    cursor_rect
                );
                let _ = tx.send(FcitxDbusOutput {
                    target,
                    preedit: Some(preedit),
                    commit,
                    status,
                    cursor_rect,
                });
            }
        }
    }

    fn rime_state_is_empty(&self) -> bool {
        self.preedit.is_empty() && status_is_empty(&self.status)
    }

    fn set_preedit(&mut self, text: String) {
        self.preedit = text;

        let Some(input_method) = &self.input_method else {
            return;
        };

        let cursor = self.preedit.len().min(i32::MAX as usize) as i32;
        input_method.set_preedit_string(self.preedit.clone(), cursor, cursor);
        input_method.commit(self.serial);
    }

    fn clear_preedit(&mut self) {
        self.set_preedit(String::new());
        self.status.preedit.clear();
        self.status.commit_preview.clear();
        self.status.candidates.clear();
        self.status.highlighted_candidate_index = None;
    }

    fn commit_text(&mut self, text: String) {
        let Some(input_method) = &self.input_method else {
            return;
        };

        if !self.preedit.is_empty() {
            input_method.set_preedit_string(String::new(), 0, 0);
            self.preedit.clear();
        }

        eprintln!("touchdeck-ime: commit {text:?}");
        input_method.commit_string(text);
        input_method.commit(self.serial);
    }

    fn commit_touchdeck_text_or_forward(
        &mut self,
        time: u32,
        key: u32,
        keysym: u32,
        state: KeyState,
        modifiers: u32,
    ) {
        if state != KeyState::Pressed {
            return;
        }

        let rime_mask = rime_modifier_mask(modifiers);
        if rime_mask & (RIME_CONTROL_MASK | RIME_ALT_MASK | RIME_SUPER_MASK) != 0 {
            self.passthrough_touchdeck_key(time, key, state);
            return;
        }

        if let Some(text) = keysym_to_text(keysym, rime_mask) {
            self.commit_text(text);
        } else {
            self.passthrough_touchdeck_key(time, key, state);
        }
    }

    fn current_status(&self) -> ImeStatus {
        self.current_status_with_source("unknown")
    }

    fn current_status_with_source(&self, source: &str) -> ImeStatus {
        let mut status = self.status.clone();
        status.source = source.to_string();
        status.display_kind = if source == "touchdeck" {
            "touchdeck".to_string()
        } else if source == "fcitx-dbus" || self.fcitx_focus.is_some() {
            "fcitx-dbus".to_string()
        } else if source == "xim" {
            "xim".to_string()
        } else {
            "wayland-im".to_string()
        };
        status.active = match status.display_kind.as_str() {
            "fcitx-dbus" => self.fcitx_focus.is_some(),
            _ => self.active,
        };
        status.client_side_input_panel = self.fcitx_uses_client_side_input_panel();
        status.ui_owner = match status.display_kind.as_str() {
            "touchdeck" => "touchdeck-overlay".to_string(),
            "fcitx-dbus" if status.client_side_input_panel => "client".to_string(),
            "fcitx-dbus" => "touchdeck-server-popup".to_string(),
            "wayland-im" if source == "physical" => "native-popup".to_string(),
            _ => "none".to_string(),
        };
        status.cursor_rect = self
            .fcitx_cursor_rect
            .as_ref()
            .filter(|rect| {
                self.fcitx_focus
                    .as_ref()
                    .map(|target| rect.target.matches(target))
                    .unwrap_or(false)
            })
            .map(|rect| ImeCursorRect {
                x: rect.x,
                y: rect.y,
                w: rect.w,
                h: rect.h,
                scale: rect.scale,
                space: rect.space.clone(),
                window_x: rect.x11_window.map(|window| window.x),
                window_y: rect.x11_window.map(|window| window.y),
                window_w: rect.x11_window.map(|window| window.w),
                window_h: rect.x11_window.map(|window| window.h),
                root_w: rect.x11_window.map(|window| window.root_w),
                root_h: rect.x11_window.map(|window| window.root_h),
            });
        status
    }

    fn query_x11_active_window_geometry(&mut self) -> Option<X11WindowGeometry> {
        if self.x11_geometry.is_none() {
            self.x11_geometry = match X11GeometryProbe::connect() {
                Ok(probe) => Some(probe),
                Err(err) => {
                    eprintln!("touchdeck-ime: failed to initialize x11 geometry probe: {err:?}");
                    None
                }
            };
        }

        let Some(probe) = self.x11_geometry.as_ref() else {
            return None;
        };

        let active = match probe.active_window_geometry() {
            Ok(geometry) => geometry,
            Err(err) => {
                eprintln!("touchdeck-ime: failed to query x11 active window geometry: {err:?}");
                self.x11_geometry = None;
                return None;
            }
        };
        let focus = match probe.input_focus_geometry() {
            Ok(geometry) => geometry,
            Err(err) => {
                eprintln!("touchdeck-ime: failed to query x11 input focus geometry: {err:?}");
                None
            }
        };

        eprintln!(
            "touchdeck-ime: x11 geometry active={} focus={}",
            format_x11_geometry(active),
            format_x11_geometry(focus)
        );

        active
    }

    fn add_status_subscriber(&mut self, response: Sender<ImeStatus>) {
        let _ = response.send(self.current_status());
        self.status_subscribers.push(response);
        eprintln!(
            "touchdeck-ime: status subscriber connected; count={}",
            self.status_subscribers.len()
        );
    }

    fn broadcast_status(&mut self, source: &str) {
        let status = self.current_status_with_source(source);
        self.status_subscribers
            .retain(|subscriber| subscriber.send(status.clone()).is_ok());
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
            "wl_compositor" if state.compositor.is_none() => {
                state.compositor = Some(registry.bind::<wl_compositor::WlCompositor, _, _>(
                    name,
                    version.min(6),
                    qh,
                    (),
                ));
                eprintln!("touchdeck-ime: bound wl_compositor");
            }
            "wl_shm" if state.shm.is_none() => {
                state.shm =
                    Some(registry.bind::<wl_shm::WlShm, _, _>(name, version.min(1), qh, ()));
                eprintln!("touchdeck-ime: bound wl_shm");
            }
            "wl_seat" if state.seat.is_none() => {
                state.seat =
                    Some(registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(9), qh, ()));
                eprintln!("touchdeck-ime: bound wl_seat");
                state.maybe_init_input_method(qh);
            }
            "zwp_input_method_manager_v2" if state.input_method_manager.is_none() => {
                state.input_method_manager = Some(registry.bind::<ZwpInputMethodManagerV2, _, _>(
                    name,
                    version.min(1),
                    qh,
                    (),
                ));
                eprintln!("touchdeck-ime: bound zwp_input_method_manager_v2");
                state.maybe_init_input_method(qh);
            }
            "zwp_virtual_keyboard_manager_v1" if state.virtual_keyboard_manager.is_none() => {
                state.virtual_keyboard_manager =
                    Some(registry.bind::<ZwpVirtualKeyboardManagerV1, _, _>(
                        name,
                        version.min(1),
                        qh,
                        (),
                    ));
                eprintln!("touchdeck-ime: bound zwp_virtual_keyboard_manager_v1");
                state.maybe_init_input_method(qh);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for ImeApp {
    fn event(
        _state: &mut Self,
        _compositor: &wl_compositor::WlCompositor,
        _event: wl_compositor::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_region::WlRegion, ()> for ImeApp {
    fn event(
        _state: &mut Self,
        _region: &wl_region::WlRegion,
        _event: wl_region::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm::WlShm, ()> for ImeApp {
    fn event(
        _state: &mut Self,
        _shm: &wl_shm::WlShm,
        _event: wl_shm::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for ImeApp {
    fn event(
        _state: &mut Self,
        _pool: &wl_shm_pool::WlShmPool,
        _event: wl_shm_pool::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for ImeApp {
    fn event(
        state: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_buffer::Event::Release) {
            state.popup_renderer.release_buffer(buffer);
        }
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for ImeApp {
    fn event(
        _state: &mut Self,
        _surface: &wl_surface::WlSurface,
        _event: wl_surface::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
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
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => {
                state.active = true;
                state.clear_preedit();
                state.status.active = true;
                if let Some(rime) = state.rime.as_mut() {
                    rime.clear();
                }
                state.hide_popup(qh);
                state.broadcast_status("physical");
                eprintln!("touchdeck-ime: activate");
            }
            zwp_input_method_v2::Event::Deactivate => {
                state.active = false;
                state.clear_preedit();
                state.status.active = false;
                if let Some(rime) = state.rime.as_mut() {
                    rime.clear();
                }
                state.hide_popup(qh);
                state.broadcast_status("physical");
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

impl Dispatch<ZwpInputPopupSurfaceV2, ()> for ImeApp {
    fn event(
        _state: &mut Self,
        _popup: &ZwpInputPopupSurfaceV2,
        event: zwp_input_popup_surface_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_popup_surface_v2::Event::TextInputRectangle {
                x,
                y,
                width,
                height,
            } => {
                eprintln!(
                    "touchdeck-ime: text input rectangle x={} y={} width={} height={}",
                    x, y, width, height
                );
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for ImeApp {
    fn event(
        state: &mut Self,
        _grab: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_keyboard_grab_v2::Event::Keymap { format, fd, size } => {
                if state.virtual_keyboard_has_keymap {
                    return;
                }

                if let Some(virtual_keyboard) = &state.virtual_keyboard {
                    virtual_keyboard.keymap(format.into(), fd.as_fd(), size);
                    state.virtual_keyboard_has_keymap = true;
                    eprintln!("touchdeck-ime: forwarded physical keymap to virtual keyboard");
                }
            }
            zwp_input_method_keyboard_grab_v2::Event::Key {
                time,
                key,
                state: key_state,
                ..
            } => {
                state.handle_physical_key(qh, time, key, key_state);
            }
            zwp_input_method_keyboard_grab_v2::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                state.physical_modifiers = mods_depressed;
                state.passthrough_physical_modifiers(
                    mods_depressed,
                    mods_latched,
                    mods_locked,
                    group,
                );
            }
            zwp_input_method_keyboard_grab_v2::Event::RepeatInfo { rate, delay } => {
                eprintln!("touchdeck-ime: physical repeat_info rate={rate} delay={delay}");
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for ImeApp {
    fn event(
        _state: &mut Self,
        _manager: &ZwpVirtualKeyboardManagerV1,
        _event: zwp_virtual_keyboard_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for ImeApp {
    fn event(
        _state: &mut Self,
        _keyboard: &ZwpVirtualKeyboardV1,
        _event: zwp_virtual_keyboard_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

const XIM_EVENT_MASK: u32 = 3;

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
        _input_style: InputStyle,
    ) -> std::result::Result<Self::InputContextData, xim::ServerError> {
        Ok(())
    }

    fn input_styles(&self) -> Self::InputStyleArray {
        [
            InputStyle::PREEDIT_NOTHING | InputStyle::STATUS_NOTHING,
            InputStyle::PREEDIT_POSITION | InputStyle::STATUS_NONE,
            InputStyle::PREEDIT_POSITION | InputStyle::STATUS_NOTHING,
            InputStyle::PREEDIT_POSITION | InputStyle::STATUS_CALLBACKS,
            InputStyle::PREEDIT_CALLBACKS | InputStyle::STATUS_NOTHING,
            InputStyle::PREEDIT_CALLBACKS | InputStyle::STATUS_CALLBACKS,
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
        eprintln!("touchdeck-ime: xim create input context");
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
        server: &mut xim::x11rb::X11rbServer<C>,
        user_ic: &mut UserInputContext<Self::InputContextData>,
    ) -> std::result::Result<(), xim::ServerError> {
        eprintln!("touchdeck-ime: xim set input context values");
        server.preedit_draw(&mut user_ic.ic, "")
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

fn spawn_xim_server(tx: Sender<XimRequest>) {
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

fn status_is_empty(status: &ImeStatus) -> bool {
    status.preedit.is_empty() && status.commit_preview.is_empty() && status.candidates.is_empty()
}

fn format_x11_geometry(geometry: Option<X11WindowGeometry>) -> String {
    match geometry {
        Some(geometry) => format!(
            "0x{:x}=({}, {} {}x{}) root={}x{}",
            geometry.window,
            geometry.x,
            geometry.y,
            geometry.w,
            geometry.h,
            geometry.root_w,
            geometry.root_h
        ),
        None => "none".to_string(),
    }
}

fn poll_fd(fd: RawFd, timeout: Option<Duration>) -> Result<bool> {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout_ms = timeout
        .map(|duration| duration.as_millis().min(c_int::MAX as u128) as c_int)
        .unwrap_or(-1);
    let ret = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error()).context("poll");
    }

    Ok(ret > 0 && poll_fd.revents & libc::POLLIN != 0)
}
