use std::collections::HashSet;
use std::env;
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::raw::c_int;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use crate::niri;
use crate::protocol::{ImeCursorRect, ImeStatus};
use crate::x11_geometry::{X11GeometryProbe, X11WindowGeometry};
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

mod app_state;
mod config;
mod event;
mod fcitx_dbus;
mod key;
mod physical_keyboard;
mod popup;
mod rime_engine;
mod xim_frontend;

use app_state::ImeSource;
use config::{
    load_ime_config, parse_key_route, parse_key_translation_policy, ImeRuntimeConfig, KeyRoute,
    KeyTranslationPolicy,
};
use fcitx_dbus::{
    spawn_fcitx_dbus_server, FcitxCursorRect, FcitxDbusKeyResponse, FcitxDbusOutput,
    FcitxDbusRequest, FcitxDbusTarget, FCITX_CAPABILITY_CLIENT_SIDE_INPUT_PANEL,
};
use key::{
    evdev_key_to_keysym, is_empty_state_passthrough_key, keysym_to_text, parse_key_state,
    parse_wayland_key_state, rime_modifier_mask, x_keycode_to_keysym, KeyState, RIME_ALT_MASK,
    RIME_CONTROL_MASK, RIME_SUPER_MASK,
};
use physical_keyboard::PhysicalKeyboard;
use popup::PopupRenderer;
use rime_engine::RimeEngine;
pub use event::TouchDeckEvent;
use xim_frontend::{spawn_xim_server, XimKeyResponse, XimPreeditArea, XimRequest};

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
    xim_cursor_rect: Option<ImeCursorRect>,
    xim_consumed_keys: HashSet<u8>,
    physical_keyboard: Option<PhysicalKeyboard>,
    physical_modifiers: u32,
    virtual_keyboard_has_keymap: bool,
    running: bool,
}

pub fn spawn_embedded(status_tx: Sender<ImeStatus>) -> Sender<TouchDeckEvent> {
    let (event_tx, event_rx) = mpsc::channel();
    thread::spawn(move || {
        let result = load_ime_config()
            .context("load embedded touchdeck-ime config")
            .and_then(|config| run_embedded(config, event_rx, status_tx));
        if let Err(err) = result {
            eprintln!("touchdeck-ime: embedded runtime stopped: {err:?}");
        }
    });
    event_tx
}

fn run_embedded(
    config: ImeRuntimeConfig,
    touchdeck_event_rx: Receiver<TouchDeckEvent>,
    status_tx: Sender<ImeStatus>,
) -> Result<()> {
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
    app.add_status_subscriber(status_tx);

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

        while let Ok(event) = touchdeck_event_rx.try_recv() {
            app.handle_touchdeck_event(&qh, event);
            app.broadcast_status("touchdeck");
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

        let Some(keysym) = self
            .physical_keyboard
            .as_ref()
            .and_then(|keyboard| keyboard.keysym_for_evdev_key(key))
            .or_else(|| evdev_key_to_keysym(key))
        else {
            self.passthrough_physical_key(time, key, state);
            return;
        };
        if self.rime_state_is_empty() && is_empty_state_passthrough_key(keysym) {
            self.passthrough_physical_key(time, key, state);
            return;
        }

        let Some(effects) = self.process_rime_key(
            &format!("physical key {key}"),
            keysym,
            key_state,
            self.physical_modifiers,
            None,
        ) else {
            self.passthrough_physical_key(time, key, state);
            return;
        };

        let handled = effects.handled;
        self.apply_local_effects(ImeSource::Physical, effects);

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
                client_window,
                app_window,
                focus_window,
                spot_x,
                spot_y,
                preedit_area,
                preedit_area_needed,
                line_space,
                response,
            } => {
                let result = self.handle_xim_key(
                    time,
                    hardware_keycode,
                    state_mask,
                    state,
                    client_window,
                    app_window,
                    focus_window,
                    spot_x,
                    spot_y,
                    preedit_area,
                    preedit_area_needed,
                    line_space,
                );
                let _ = response.send(result);
            }
            XimRequest::Reset { response } => {
                if let Some(rime) = self.rime.as_mut() {
                    rime.clear();
                }
                self.preedit.clear();
                self.status = ImeStatus::default();
                self.xim_cursor_rect = None;
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

        let Some(effects) = self.process_rime_key(
            &format!("fcitx dbus keyval={keyval} keycode={keycode}"),
            keyval,
            key_state,
            state,
            Some(KeyTranslationPolicy::Raw),
        ) else {
            return FcitxDbusKeyResponse::default();
        };

        let handled = effects.handled;
        let effects = self.apply_response_effects(ImeSource::FcitxDbus, effects);

        eprintln!(
            "touchdeck-ime: fcitx dbus keyval={keyval} keycode={keycode} state={state} release={is_release} time={time} handled={handled} preedit={:?}",
            effects.preedit
        );

        FcitxDbusKeyResponse {
            handled,
            preedit: effects.preedit,
            commit: effects.commit,
            status: effects.status,
        }
    }

    fn handle_xim_key(
        &mut self,
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
    ) -> XimKeyResponse {
        self.update_xim_cursor_rect(
            client_window,
            app_window,
            focus_window,
            spot_x,
            spot_y,
            preedit_area,
            preedit_area_needed,
            line_space,
        );

        let Some(keysym) = x_keycode_to_keysym(hardware_keycode) else {
            eprintln!(
                "touchdeck-ime: xim forward unknown hardware keycode {}",
                hardware_keycode
            );
            return XimKeyResponse::default();
        };

        let consumed_press_release =
            state == KeyState::Released && self.xim_consumed_keys.remove(&hardware_keycode);

        if !consumed_press_release
            && self.rime_state_is_empty()
            && is_empty_state_passthrough_key(keysym)
        {
            return XimKeyResponse::default();
        }

        let Some(effects) = self.process_rime_key(
            &format!("xim keycode {hardware_keycode}"),
            keysym,
            state,
            u32::from(state_mask),
            None,
        ) else {
            return XimKeyResponse::default();
        };

        let consumed = effects.handled || consumed_press_release;
        let effects = self.apply_response_effects(ImeSource::Xim, effects);

        if state == KeyState::Pressed && consumed {
            self.xim_consumed_keys.insert(hardware_keycode);
        }

        eprintln!(
            "touchdeck-ime: xim keycode={} keysym={} state={:?} time={} modifiers={} consumed={} preedit={:?}",
            hardware_keycode, keysym, state, time, state_mask, consumed, effects.preedit
        );

        XimKeyResponse {
            consumed,
            preedit: effects.preedit,
            commit: effects.commit,
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

        let Some(effects) = self.process_rime_key(
            &format!("touchdeck key {}", event.key),
            keysym,
            state,
            event.modifiers,
            translation,
        ) else {
            if route == KeyRoute::ImeKey {
                self.passthrough_touchdeck_key(event.time, event.key, state);
            }
            return self.current_status_with_source("touchdeck");
        };

        let handled = effects.handled;
        self.apply_local_effects(ImeSource::Touchdeck, effects);

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
            "xim" => !status_is_empty(&status),
            _ => self.active,
        };
        status.client_side_input_panel = self.fcitx_uses_client_side_input_panel();
        status.ui_owner = match status.display_kind.as_str() {
            "touchdeck" => "touchdeck-overlay".to_string(),
            "fcitx-dbus" if status.client_side_input_panel => "client".to_string(),
            "fcitx-dbus" => "touchdeck-server-popup".to_string(),
            "xim" => "touchdeck-server-popup".to_string(),
            "wayland-im" if source == "physical" => "native-popup".to_string(),
            _ => "none".to_string(),
        };
        status.cursor_rect = if status.display_kind == "xim" {
            self.xim_cursor_rect.clone()
        } else {
            self.fcitx_cursor_rect
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
                })
        };
        status
    }

    fn update_xim_cursor_rect(
        &mut self,
        client_window: u32,
        app_window: Option<u32>,
        focus_window: Option<u32>,
        spot_x: i32,
        spot_y: i32,
        preedit_area: Option<XimPreeditArea>,
        preedit_area_needed: Option<XimPreeditArea>,
        line_space: Option<u32>,
    ) {
        let client_geometry = self.query_x11_window_geometry(client_window);
        let app_geometry = app_window.and_then(|window| self.query_x11_window_geometry(window));
        let focus_geometry = focus_window.and_then(|window| self.query_x11_window_geometry(window));

        let (anchor_window, anchor) = if let Some(window) = focus_window {
            match focus_geometry {
                Some(geometry) => (window, geometry),
                None => {
                    eprintln!(
                        "touchdeck-ime: xim cursor rect focus window has no geometry focus=0x{window:x}"
                    );
                    self.xim_cursor_rect = None;
                    return;
                }
            }
        } else if let Some(window) = app_window {
            match app_geometry {
                Some(geometry) => (window, geometry),
                None => {
                    eprintln!(
                        "touchdeck-ime: xim cursor rect app window has no geometry app=0x{window:x}"
                    );
                    self.xim_cursor_rect = None;
                    return;
                }
            }
        } else {
            match client_geometry {
                Some(geometry) => (client_window, geometry),
                None => {
                    eprintln!(
                        "touchdeck-ime: xim cursor rect client window has no geometry client=0x{client_window:x}"
                    );
                    self.xim_cursor_rect = None;
                    return;
                }
            }
        };

        eprintln!(
            "touchdeck-ime: xim geometry client=0x{client_window:x} {} app={} focus={} anchor=0x{anchor_window:x} {} spot=({spot_x},{spot_y}) area={} area_needed={} line_space={:?}",
            format_x11_geometry(client_geometry),
            format_x11_window_geometry(app_window, app_geometry),
            format_x11_window_geometry(focus_window, focus_geometry),
            format_x11_geometry(Some(anchor)),
            format_xim_preedit_area(preedit_area),
            format_xim_preedit_area(preedit_area_needed),
            line_space
        );

        let Some(top_level) = self.query_x11_active_window_geometry() else {
            eprintln!(
                "touchdeck-ime: xim cursor rect has no active-window geometry client=0x{client_window:x} app={app_window:?} focus={focus_window:?}"
            );
            self.xim_cursor_rect = None;
            return;
        };

        let (source, x, y, w, h) = if let Some(area) = preedit_area {
            (
                "area",
                anchor.x + area.x,
                anchor.y + area.y,
                area.w.max(0),
                area.h.max(0),
            )
        } else {
            let h = line_space
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or(0);
            ("spot", anchor.x + spot_x, anchor.y + spot_y, 0, h)
        };
        let Some((surface_x, surface_y, surface_h)) = map_x11_cursor_to_surface(x, y, h, top_level)
        else {
            eprintln!(
                "touchdeck-ime: xim cursor rect could not map x11 root to touchdeck surface root=({x},{y} {w}x{h}) top=0x{:x}=({},{} {}x{})",
                top_level.window,
                top_level.x,
                top_level.y,
                top_level.w,
                top_level.h
            );
            self.xim_cursor_rect = None;
            return;
        };

        self.xim_cursor_rect = Some(ImeCursorRect {
            x: surface_x,
            y: surface_y,
            w,
            h: surface_h,
            scale: 1.0,
            space: "surface".to_string(),
            window_x: None,
            window_y: None,
            window_w: None,
            window_h: None,
            root_w: None,
            root_h: None,
        });
        eprintln!(
            "touchdeck-ime: xim cursor rect source={source} client=0x{client_window:x} app={app_window:?} focus={focus_window:?} spot=({spot_x},{spot_y}) area={} root=({x},{y} {w}x{h}) surface=({surface_x},{surface_y} h={surface_h}) top=0x{:x}=({},{} {}x{}) root={}x{}",
            format_xim_preedit_area(preedit_area),
            top_level.window,
            top_level.x,
            top_level.y,
            top_level.w,
            top_level.h,
            top_level.root_w,
            top_level.root_h
        );
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

        let probe = self.x11_geometry.as_ref()?;

        let active = match probe.active_window_geometry() {
            Ok(geometry) => geometry,
            Err(err) => {
                eprintln!("touchdeck-ime: failed to query X11 active window geometry: {err:?}");
                None
            }
        };
        let focus = match probe.input_focus_geometry() {
            Ok(geometry) => geometry,
            Err(err) => {
                eprintln!("touchdeck-ime: failed to query X11 input focus geometry: {err:?}");
                None
            }
        };

        if let Some(active) = active {
            eprintln!(
                "touchdeck-ime: x11 geometry active=0x{:x}=({}, {} {}x{}) root={}x{} focus={}",
                active.window,
                active.x,
                active.y,
                active.w,
                active.h,
                active.root_w,
                active.root_h,
                format_x11_geometry(focus)
            );
            Some(active)
        } else if let Some(focus) = focus {
            eprintln!(
                "touchdeck-ime: x11 geometry active=none focus=0x{:x}=({}, {} {}x{}) root={}x{}",
                focus.window, focus.x, focus.y, focus.w, focus.h, focus.root_w, focus.root_h
            );
            Some(focus)
        } else {
            eprintln!("touchdeck-ime: x11 geometry active=none focus=none");
            None
        }
    }

    fn query_x11_window_geometry(&mut self, window: u32) -> Option<X11WindowGeometry> {
        if self.x11_geometry.is_none() {
            self.x11_geometry = match X11GeometryProbe::connect() {
                Ok(probe) => Some(probe),
                Err(err) => {
                    eprintln!("touchdeck-ime: failed to initialize x11 geometry probe: {err:?}");
                    None
                }
            };
        }

        let probe = self.x11_geometry.as_ref()?;

        match probe.window_geometry(window) {
            Ok(geometry) => Some(geometry),
            Err(err) => {
                eprintln!(
                    "touchdeck-ime: failed to query X11 window 0x{window:x} geometry: {err:?}"
                );
                None
            }
        }
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
        log_ime_ownership(|| {
            eprintln!(
                "touchdeck-ime: ownership broadcast requested_source={} status_source={} display={} ui_owner={} active={} client_side_panel={} preedit={:?} candidates={} cursor_rect={} subscribers={}",
                source,
                status.source,
                status.display_kind,
                status.ui_owner,
                status.active,
                status.client_side_input_panel,
                status.preedit,
                status.candidates.len(),
                status.cursor_rect.is_some(),
                self.status_subscribers.len()
            );
        });
        self.status_subscribers
            .retain(|subscriber| subscriber.send(status.clone()).is_ok());
    }
}

fn log_ime_ownership(log: impl FnOnce()) {
    if std::env::var_os("TOUCHDECK_LOG_IME_OWNERSHIP").is_some() {
        log();
    }
}

fn map_x11_cursor_to_surface(
    x11_x: i32,
    x11_y: i32,
    x11_h: i32,
    top_level: X11WindowGeometry,
) -> Option<(i32, i32, i32)> {
    let layout = match niri::focused_window_layout() {
        Ok(Some(layout)) => layout,
        Ok(None) => {
            eprintln!("touchdeck-ime: xim cursor rect has no focused niri window");
            return None;
        }
        Err(err) => {
            eprintln!("touchdeck-ime: failed to query niri focused window for xim cursor: {err:?}");
            return None;
        }
    };
    let (window_output_x, window_output_y, window_output_w, window_output_h) =
        layout.window_rect_in_output;
    if window_output_w <= 0 || window_output_h <= 0 || top_level.w <= 0 || top_level.h <= 0 {
        return None;
    }

    let output_layout = niri::focused_output_layout().ok().flatten();
    let origin_x = window_output_x;
    let origin_y = window_output_y;
    let scale_x = f64::from(top_level.w) / f64::from(window_output_w);
    let scale_y = f64::from(top_level.h) / f64::from(window_output_h);
    if !scale_x.is_finite() || !scale_y.is_finite() || scale_x <= 0.0 || scale_y <= 0.0 {
        return None;
    }

    let local_x = f64::from(x11_x - top_level.x);
    let local_y = f64::from(x11_y - top_level.y);
    let surface_x = (origin_x + local_x / scale_x).round() as i32;
    let surface_y = (origin_y + local_y / scale_y).round() as i32;
    let surface_h = (f64::from(x11_h.max(0)) / scale_y).round() as i32;

    eprintln!(
        "touchdeck-ime: xim surface geometry x11-root=({x11_x},{x11_y} h={x11_h}) top=0x{:x}=({},{} {}x{}) niri_output={:?} niri_window=({window_output_x:.2},{window_output_y:.2} {window_output_w}x{window_output_h}) overlay_origin=({origin_x:.2},{origin_y:.2}) scale=({scale_x:.4},{scale_y:.4}) surface=({surface_x},{surface_y} h={surface_h})",
        top_level.window,
        top_level.x,
        top_level.y,
        top_level.w,
        top_level.h,
        output_layout
    );

    Some((surface_x, surface_y, surface_h))
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
        if let zwp_input_popup_surface_v2::Event::TextInputRectangle {
            x,
            y,
            width,
            height,
        } = event
        {
            eprintln!(
                "touchdeck-ime: text input rectangle x={} y={} width={} height={}",
                x, y, width, height
            );
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
                let format: u32 = format.into();
                if state.virtual_keyboard_has_keymap {
                    return;
                }

                if let Some(virtual_keyboard) = &state.virtual_keyboard {
                    virtual_keyboard.keymap(format, fd.as_fd(), size);
                    state.virtual_keyboard_has_keymap = true;
                    eprintln!("touchdeck-ime: forwarded physical keymap to virtual keyboard");
                }

                if format == xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1 {
                    match PhysicalKeyboard::from_keymap_fd(fd, size) {
                        Ok(keyboard) => {
                            state.physical_keyboard = Some(keyboard);
                            eprintln!("touchdeck-ime: initialized physical keyboard xkb state");
                        }
                        Err(err) => {
                            state.physical_keyboard = None;
                            eprintln!("touchdeck-ime: failed to initialize xkb keymap: {err:?}");
                        }
                    }
                } else {
                    state.physical_keyboard = None;
                    eprintln!("touchdeck-ime: unsupported physical keymap format {format}");
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
                if let Some(keyboard) = state.physical_keyboard.as_mut() {
                    keyboard.update_modifiers(mods_depressed, mods_latched, mods_locked, group);
                }
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

fn format_x11_window_geometry(window: Option<u32>, geometry: Option<X11WindowGeometry>) -> String {
    match window {
        Some(window) => format!("0x{window:x} {}", format_x11_geometry(geometry)),
        None => "none".to_string(),
    }
}

fn format_xim_preedit_area(area: Option<XimPreeditArea>) -> String {
    match area {
        Some(area) => format!("({}, {} {}x{})", area.x, area.y, area.w, area.h),
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
