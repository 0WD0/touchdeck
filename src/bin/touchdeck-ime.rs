use std::env;
use std::ffi::{CStr, CString};
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::raw::{c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use wayland_client::protocol::{wl_keyboard, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::{self, ZwpInputMethodManagerV2},
    zwp_input_method_v2::{self, ZwpInputMethodV2},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::{self, ZwpVirtualKeyboardManagerV1},
    zwp_virtual_keyboard_v1::{self, ZwpVirtualKeyboardV1},
};

const RIME_FALSE: c_int = 0;
const RIME_SHIFT_MASK: u32 = 1 << 0;
const RIME_CONTROL_MASK: u32 = 1 << 2;
const RIME_ALT_MASK: u32 = 1 << 3;
const RIME_SUPER_MASK: u32 = 1 << 26;
const RIME_RELEASE_MASK: u32 = 1 << 30;

const XKB_SHIFT_MASK: u32 = 1 << 0;
const XKB_CONTROL_MASK: u32 = 1 << 2;
const XKB_ALT_MASK: u32 = 1 << 3;
const XKB_SUPER_MASK: u32 = 1 << 6;

const XK_BACKSPACE: u32 = 0xff08;
const XK_TAB: u32 = 0xff09;
const XK_RETURN: u32 = 0xff0d;
const XK_ESCAPE: u32 = 0xff1b;
const XK_DELETE: u32 = 0xffff;
const XK_HOME: u32 = 0xff50;
const XK_LEFT: u32 = 0xff51;
const XK_UP: u32 = 0xff52;
const XK_RIGHT: u32 = 0xff53;
const XK_DOWN: u32 = 0xff54;
const XK_PAGE_UP: u32 = 0xff55;
const XK_PAGE_DOWN: u32 = 0xff56;
const XK_END: u32 = 0xff57;
const XK_SHIFT_L: u32 = 0xffe1;
const XK_SHIFT_R: u32 = 0xffe2;
const XK_CONTROL_L: u32 = 0xffe3;
const XK_CONTROL_R: u32 = 0xffe4;
const XK_ALT_L: u32 = 0xffe9;
const XK_ALT_R: u32 = 0xffea;
const XK_SUPER_L: u32 = 0xffeb;
const XK_SUPER_R: u32 = 0xffec;

#[derive(Default)]
struct ImeApp {
    seat: Option<wl_seat::WlSeat>,
    input_method_manager: Option<ZwpInputMethodManagerV2>,
    virtual_keyboard_manager: Option<ZwpVirtualKeyboardManagerV1>,
    input_method: Option<ZwpInputMethodV2>,
    keyboard_grab: Option<ZwpInputMethodKeyboardGrabV2>,
    virtual_keyboard: Option<ZwpVirtualKeyboardV1>,
    rime: Option<RimeEngine>,
    active: bool,
    serial: u32,
    preedit: String,
    physical_modifiers: u32,
    virtual_keyboard_has_keymap: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyState {
    Pressed,
    Released,
}

#[derive(Debug, Default)]
struct RimeOutput {
    handled: bool,
    commit: Option<String>,
    preedit: String,
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
        rime: Some(RimeEngine::new().context("initialize librime")?),
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

    fn handle_physical_key(
        &mut self,
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

        let output = {
            let Some(rime) = self.rime.as_mut() else {
                self.passthrough_physical_key(time, key, state);
                return;
            };

            match rime.process_key(keysym, key_state, self.physical_modifiers) {
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

        eprintln!(
            "touchdeck-ime: physical key={} state={:?} modifiers={} handled={} preedit={:?}",
            key, key_state, self.physical_modifiers, handled, self.preedit
        );
    }

    fn passthrough_physical_key(
        &self,
        time: u32,
        key: u32,
        state: WEnum<wl_keyboard::KeyState>,
    ) {
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

    fn handle_touchdeck_event(&mut self, event: TouchDeckEvent) {
        if event.protocol != "touchdeck-ime-v1" || event.kind != "key" || event.source != "touchdeck" {
            eprintln!("touchdeck-ime: ignored unsupported event {event:?}");
            return;
        }

        let Some(state) = parse_key_state(&event.state) else {
            eprintln!("touchdeck-ime: ignored key with unknown state {event:?}");
            return;
        };

        if !self.active {
            eprintln!(
                "touchdeck-ime: ignored key {} because input method is inactive",
                event.key
            );
            return;
        }

        let Some(keysym) = evdev_key_to_keysym(event.key) else {
            eprintln!("touchdeck-ime: ignored unknown evdev key {}", event.key);
            return;
        };

        let output = {
            let Some(rime) = self.rime.as_mut() else {
                eprintln!("touchdeck-ime: rime engine unavailable");
                return;
            };

            match rime.process_key(keysym, state, event.modifiers) {
                Ok(output) => output,
                Err(err) => {
                    eprintln!("touchdeck-ime: rime error for key {}: {err:?}", event.key);
                    return;
                }
            }
        };

        let handled = output.handled;
        self.apply_rime_output(output);

        if !handled {
            self.fallback_unhandled_key(keysym, state, event.modifiers);
        }

        eprintln!(
            "touchdeck-ime: key={} state={:?} time={} modifiers={} handled={} preedit={:?}",
            event.key, state, event.time, event.modifiers, handled, self.preedit
        );
    }

    fn apply_rime_output(&mut self, output: RimeOutput) {
        if output.preedit != self.preedit {
            self.set_preedit(output.preedit);
        }

        if let Some(text) = output.commit {
            self.commit_text(text);
        }
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

    fn fallback_unhandled_key(&mut self, keysym: u32, state: KeyState, modifiers: u32) {
        if state != KeyState::Pressed {
            return;
        }

        let rime_mask = rime_modifier_mask(modifiers);
        if rime_mask & (RIME_CONTROL_MASK | RIME_ALT_MASK | RIME_SUPER_MASK) != 0 {
            return;
        }

        match keysym {
            XK_ESCAPE => {
                if let Some(rime) = self.rime.as_mut() {
                    rime.clear();
                }
                self.clear_preedit();
            }
            XK_BACKSPACE => {
                if let Some(input_method) = &self.input_method {
                    input_method.delete_surrounding_text(1, 0);
                    input_method.commit(self.serial);
                }
            }
            XK_DELETE => {
                if let Some(input_method) = &self.input_method {
                    input_method.delete_surrounding_text(0, 1);
                    input_method.commit(self.serial);
                }
            }
            XK_RETURN => self.commit_text("\n".to_string()),
            XK_TAB => self.commit_text("\t".to_string()),
            _ => {
                if let Some(text) = keysym_to_text(keysym, rime_mask) {
                    self.commit_text(text);
                }
            }
        }
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
                state.clear_preedit();
                if let Some(rime) = state.rime.as_mut() {
                    rime.clear();
                }
                eprintln!("touchdeck-ime: activate");
            }
            zwp_input_method_v2::Event::Deactivate => {
                state.active = false;
                state.clear_preedit();
                if let Some(rime) = state.rime.as_mut() {
                    rime.clear();
                }
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

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for ImeApp {
    fn event(
        state: &mut Self,
        _grab: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
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
                state.handle_physical_key(time, key, key_state);
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

struct RimeEngine {
    api: NonNull<RimeApi>,
    session: RimeSessionId,
    _shared_data_dir: CString,
    _user_data_dir: CString,
    _app_name: CString,
    _log_dir: CString,
}

impl RimeEngine {
    fn new() -> Result<Self> {
        let shared_data_dir = env_path("TOUCHDECK_RIME_SHARED_DATA_DIR")
            .unwrap_or_else(default_rime_shared_data_dir);
        let user_data_dir = env_path("TOUCHDECK_RIME_USER_DATA_DIR")
            .unwrap_or_else(default_rime_user_data_dir);
        fs::create_dir_all(&user_data_dir)
            .with_context(|| format!("create Rime user data dir {}", user_data_dir.display()))?;

        let shared_data_dir = path_to_cstring(&shared_data_dir)?;
        let user_data_dir = path_to_cstring(&user_data_dir)?;
        let app_name = CString::new("rime.touchdeck").expect("static string has no NUL");
        let log_dir = CString::new(env::var("TOUCHDECK_RIME_LOG_DIR").unwrap_or_default())
            .context("TOUCHDECK_RIME_LOG_DIR contains NUL")?;

        let api = NonNull::new(unsafe { rime_get_api() }).context("rime_get_api returned null")?;

        let mut traits = RimeTraits {
            data_size: rime_traits_data_size(),
            shared_data_dir: shared_data_dir.as_ptr(),
            user_data_dir: user_data_dir.as_ptr(),
            distribution_name: ptr::null(),
            distribution_code_name: ptr::null(),
            distribution_version: ptr::null(),
            app_name: app_name.as_ptr(),
            modules: ptr::null(),
            min_log_level: env::var("TOUCHDECK_RIME_LOG_LEVEL")
                .ok()
                .and_then(|value| value.parse::<c_int>().ok())
                .unwrap_or(1),
            log_dir: log_dir.as_ptr(),
            prebuilt_data_dir: ptr::null(),
            staging_dir: ptr::null(),
        };

        unsafe {
            let api_ref = api.as_ref();
            call_void(api_ref.setup, "RimeApi.setup")?(&mut traits);
            call_void(api_ref.initialize, "RimeApi.initialize")?(&mut traits);
            if env::var("TOUCHDECK_RIME_DEPLOY").ok().as_deref() != Some("0") {
                if let (Some(start), Some(join)) =
                    (api_ref.start_maintenance, api_ref.join_maintenance_thread)
                {
                    start(RIME_FALSE);
                    join();
                }
            }
        }

        let session = unsafe {
            let create_session = call_ret(api.as_ref().create_session, "RimeApi.create_session")?;
            create_session()
        };
        if session == 0 {
            return Err(anyhow!("RimeApi.create_session returned 0"));
        }

        let engine = Self {
            api,
            session,
            _shared_data_dir: shared_data_dir,
            _user_data_dir: user_data_dir,
            _app_name: app_name,
            _log_dir: log_dir,
        };

        eprintln!("touchdeck-ime: librime initialized session={session}");
        Ok(engine)
    }

    fn process_key(
        &mut self,
        keysym: u32,
        state: KeyState,
        xkb_modifiers: u32,
    ) -> Result<RimeOutput> {
        let mut mask = rime_modifier_mask(xkb_modifiers);
        if state == KeyState::Released {
            mask |= RIME_RELEASE_MASK;
        }

        let handled = unsafe {
            let process_key = call_ret(self.api().process_key, "RimeApi.process_key")?;
            process_key(self.session, keysym as c_int, mask as c_int) != RIME_FALSE
        };

        let commit = self.take_commit()?;
        let preedit = self.current_preedit()?;

        Ok(RimeOutput {
            handled,
            commit,
            preedit,
        })
    }

    fn clear(&mut self) {
        unsafe {
            if let Some(clear) = self.api().clear_composition {
                clear(self.session);
            }
        }
    }

    fn api(&self) -> &RimeApi {
        unsafe { self.api.as_ref() }
    }

    fn take_commit(&self) -> Result<Option<String>> {
        unsafe {
            let Some(get_commit) = self.api().get_commit else {
                return Ok(None);
            };
            let Some(free_commit) = self.api().free_commit else {
                return Ok(None);
            };

            let mut commit = RimeCommit {
                data_size: rime_commit_data_size(),
                text: ptr::null_mut(),
            };

            if get_commit(self.session, &mut commit) == RIME_FALSE {
                return Ok(None);
            }

            let text = if commit.text.is_null() {
                None
            } else {
                Some(CStr::from_ptr(commit.text).to_string_lossy().into_owned())
            };
            free_commit(&mut commit);
            Ok(text)
        }
    }

    fn current_preedit(&self) -> Result<String> {
        unsafe {
            let Some(get_context) = self.api().get_context else {
                return Ok(String::new());
            };
            let Some(free_context) = self.api().free_context else {
                return Ok(String::new());
            };

            let mut context = empty_rime_context();
            if get_context(self.session, &mut context) == RIME_FALSE {
                return Ok(String::new());
            }

            let preedit = if context.composition.preedit.is_null() {
                String::new()
            } else {
                CStr::from_ptr(context.composition.preedit)
                    .to_string_lossy()
                    .into_owned()
            };
            free_context(&mut context);
            Ok(preedit)
        }
    }
}

impl Drop for RimeEngine {
    fn drop(&mut self) {
        unsafe {
            if let Some(destroy_session) = self.api().destroy_session {
                destroy_session(self.session);
            }
            if let Some(finalize) = self.api().finalize {
                finalize();
            }
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

fn default_rime_shared_data_dir() -> PathBuf {
    let local = PathBuf::from("/home/disk/Projects/librime/data/minimal");
    if local.exists() {
        return local;
    }

    PathBuf::from("/usr/share/rime-data")
}

fn default_rime_user_data_dir() -> PathBuf {
    if let Some(path) = env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(path).join("touchdeck").join("rime");
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("touchdeck")
            .join("rime");
    }

    PathBuf::from("/tmp/touchdeck-rime")
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from)
}

fn path_to_cstring(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("path contains NUL: {}", path.display()))
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

fn parse_key_state(state: &str) -> Option<KeyState> {
    match state {
        "pressed" => Some(KeyState::Pressed),
        "released" => Some(KeyState::Released),
        _ => None,
    }
}

fn parse_wayland_key_state(state: &WEnum<wl_keyboard::KeyState>) -> Option<KeyState> {
    match state {
        WEnum::Value(wl_keyboard::KeyState::Pressed) => Some(KeyState::Pressed),
        WEnum::Value(wl_keyboard::KeyState::Released) => Some(KeyState::Released),
        WEnum::Unknown(2) => Some(KeyState::Pressed),
        _ => None,
    }
}

fn evdev_key_to_keysym(key: u32) -> Option<u32> {
    Some(match key {
        1 => XK_ESCAPE,
        2 => '1' as u32,
        3 => '2' as u32,
        4 => '3' as u32,
        5 => '4' as u32,
        6 => '5' as u32,
        7 => '6' as u32,
        8 => '7' as u32,
        9 => '8' as u32,
        10 => '9' as u32,
        11 => '0' as u32,
        12 => '-' as u32,
        13 => '=' as u32,
        14 => XK_BACKSPACE,
        15 => XK_TAB,
        16 => 'q' as u32,
        17 => 'w' as u32,
        18 => 'e' as u32,
        19 => 'r' as u32,
        20 => 't' as u32,
        21 => 'y' as u32,
        22 => 'u' as u32,
        23 => 'i' as u32,
        24 => 'o' as u32,
        25 => 'p' as u32,
        26 => '[' as u32,
        27 => ']' as u32,
        28 => XK_RETURN,
        29 => XK_CONTROL_L,
        30 => 'a' as u32,
        31 => 's' as u32,
        32 => 'd' as u32,
        33 => 'f' as u32,
        34 => 'g' as u32,
        35 => 'h' as u32,
        36 => 'j' as u32,
        37 => 'k' as u32,
        38 => 'l' as u32,
        39 => ';' as u32,
        40 => '\'' as u32,
        41 => '`' as u32,
        42 => XK_SHIFT_L,
        43 => '\\' as u32,
        44 => 'z' as u32,
        45 => 'x' as u32,
        46 => 'c' as u32,
        47 => 'v' as u32,
        48 => 'b' as u32,
        49 => 'n' as u32,
        50 => 'm' as u32,
        51 => ',' as u32,
        52 => '.' as u32,
        53 => '/' as u32,
        54 => XK_SHIFT_R,
        56 => XK_ALT_L,
        57 => ' ' as u32,
        97 => XK_CONTROL_R,
        100 => XK_ALT_R,
        102 => XK_HOME,
        103 => XK_UP,
        104 => XK_PAGE_UP,
        105 => XK_LEFT,
        106 => XK_RIGHT,
        107 => XK_END,
        108 => XK_DOWN,
        109 => XK_PAGE_DOWN,
        111 => XK_DELETE,
        125 => XK_SUPER_L,
        126 => XK_SUPER_R,
        _ => return None,
    })
}

fn rime_modifier_mask(xkb_modifiers: u32) -> u32 {
    let mut mask = 0;
    if xkb_modifiers & XKB_SHIFT_MASK != 0 {
        mask |= RIME_SHIFT_MASK;
    }
    if xkb_modifiers & XKB_CONTROL_MASK != 0 {
        mask |= RIME_CONTROL_MASK;
    }
    if xkb_modifiers & XKB_ALT_MASK != 0 {
        mask |= RIME_ALT_MASK;
    }
    if xkb_modifiers & XKB_SUPER_MASK != 0 {
        mask |= RIME_SUPER_MASK;
    }
    mask
}

fn keysym_to_text(keysym: u32, rime_mask: u32) -> Option<String> {
    let shifted = rime_mask & RIME_SHIFT_MASK != 0;
    if (97..=122).contains(&keysym) {
        let ch = char::from_u32(keysym)?;
        return Some(if shifted {
            ch.to_ascii_uppercase()
        } else {
            ch
        }
        .to_string());
    }

    let ch = match keysym {
        49 => {
            if shifted {
                '!'
            } else {
                '1'
            }
        }
        50 => {
            if shifted {
                '@'
            } else {
                '2'
            }
        }
        51 => {
            if shifted {
                '#'
            } else {
                '3'
            }
        }
        52 => {
            if shifted {
                '$'
            } else {
                '4'
            }
        }
        53 => {
            if shifted {
                '%'
            } else {
                '5'
            }
        }
        54 => {
            if shifted {
                '^'
            } else {
                '6'
            }
        }
        55 => {
            if shifted {
                '&'
            } else {
                '7'
            }
        }
        56 => {
            if shifted {
                '*'
            } else {
                '8'
            }
        }
        57 => {
            if shifted {
                '('
            } else {
                '9'
            }
        }
        48 => {
            if shifted {
                ')'
            } else {
                '0'
            }
        }
        45 => {
            if shifted {
                '_'
            } else {
                '-'
            }
        }
        61 => {
            if shifted {
                '+'
            } else {
                '='
            }
        }
        91 => {
            if shifted {
                '{'
            } else {
                '['
            }
        }
        93 => {
            if shifted {
                '}'
            } else {
                ']'
            }
        }
        92 => {
            if shifted {
                '|'
            } else {
                '\\'
            }
        }
        59 => {
            if shifted {
                ':'
            } else {
                ';'
            }
        }
        39 => {
            if shifted {
                '"'
            } else {
                '\''
            }
        }
        96 => {
            if shifted {
                '~'
            } else {
                '`'
            }
        }
        44 => {
            if shifted {
                '<'
            } else {
                ','
            }
        }
        46 => {
            if shifted {
                '>'
            } else {
                '.'
            }
        }
        47 => {
            if shifted {
                '?'
            } else {
                '/'
            }
        }
        32 => ' ',
        _ => return None,
    };

    Some(ch.to_string())
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

type Bool = c_int;
type RimeSessionId = usize;

#[repr(C)]
struct RimeTraits {
    data_size: c_int,
    shared_data_dir: *const c_char,
    user_data_dir: *const c_char,
    distribution_name: *const c_char,
    distribution_code_name: *const c_char,
    distribution_version: *const c_char,
    app_name: *const c_char,
    modules: *const *const c_char,
    min_log_level: c_int,
    log_dir: *const c_char,
    prebuilt_data_dir: *const c_char,
    staging_dir: *const c_char,
}

#[repr(C)]
struct RimeComposition {
    length: c_int,
    cursor_pos: c_int,
    sel_start: c_int,
    sel_end: c_int,
    preedit: *mut c_char,
}

#[repr(C)]
struct RimeCandidate {
    text: *mut c_char,
    comment: *mut c_char,
    reserved: *mut c_void,
}

#[repr(C)]
struct RimeMenu {
    page_size: c_int,
    page_no: c_int,
    is_last_page: Bool,
    highlighted_candidate_index: c_int,
    num_candidates: c_int,
    candidates: *mut RimeCandidate,
    select_keys: *mut c_char,
}

#[repr(C)]
struct RimeCommit {
    data_size: c_int,
    text: *mut c_char,
}

#[repr(C)]
struct RimeContext {
    data_size: c_int,
    composition: RimeComposition,
    menu: RimeMenu,
    commit_text_preview: *mut c_char,
    select_labels: *mut *mut c_char,
}

#[repr(C)]
struct RimeApi {
    data_size: c_int,
    setup: Option<unsafe extern "C" fn(*mut RimeTraits)>,
    set_notification_handler: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    initialize: Option<unsafe extern "C" fn(*mut RimeTraits)>,
    finalize: Option<unsafe extern "C" fn()>,
    start_maintenance: Option<unsafe extern "C" fn(Bool) -> Bool>,
    is_maintenance_mode: Option<unsafe extern "C" fn() -> Bool>,
    join_maintenance_thread: Option<unsafe extern "C" fn()>,
    deployer_initialize: Option<unsafe extern "C" fn(*mut RimeTraits)>,
    prebuild: Option<unsafe extern "C" fn() -> Bool>,
    deploy: Option<unsafe extern "C" fn() -> Bool>,
    deploy_schema: Option<unsafe extern "C" fn(*const c_char) -> Bool>,
    deploy_config_file: Option<unsafe extern "C" fn(*const c_char, *const c_char) -> Bool>,
    sync_user_data: Option<unsafe extern "C" fn() -> Bool>,
    create_session: Option<unsafe extern "C" fn() -> RimeSessionId>,
    find_session: Option<unsafe extern "C" fn(RimeSessionId) -> Bool>,
    destroy_session: Option<unsafe extern "C" fn(RimeSessionId) -> Bool>,
    cleanup_stale_sessions: Option<unsafe extern "C" fn()>,
    cleanup_all_sessions: Option<unsafe extern "C" fn()>,
    process_key: Option<unsafe extern "C" fn(RimeSessionId, c_int, c_int) -> Bool>,
    commit_composition: Option<unsafe extern "C" fn(RimeSessionId) -> Bool>,
    clear_composition: Option<unsafe extern "C" fn(RimeSessionId)>,
    get_commit: Option<unsafe extern "C" fn(RimeSessionId, *mut RimeCommit) -> Bool>,
    free_commit: Option<unsafe extern "C" fn(*mut RimeCommit) -> Bool>,
    get_context: Option<unsafe extern "C" fn(RimeSessionId, *mut RimeContext) -> Bool>,
    free_context: Option<unsafe extern "C" fn(*mut RimeContext) -> Bool>,
}

unsafe extern "C" {
    fn rime_get_api() -> *mut RimeApi;
}

fn rime_traits_data_size() -> c_int {
    (std::mem::size_of::<RimeTraits>() - std::mem::size_of::<c_int>()) as c_int
}

fn rime_commit_data_size() -> c_int {
    (std::mem::size_of::<RimeCommit>() - std::mem::size_of::<c_int>()) as c_int
}

fn empty_rime_context() -> RimeContext {
    RimeContext {
        data_size: (std::mem::size_of::<RimeContext>() - std::mem::size_of::<c_int>()) as c_int,
        composition: RimeComposition {
            length: 0,
            cursor_pos: 0,
            sel_start: 0,
            sel_end: 0,
            preedit: ptr::null_mut(),
        },
        menu: RimeMenu {
            page_size: 0,
            page_no: 0,
            is_last_page: RIME_FALSE,
            highlighted_candidate_index: 0,
            num_candidates: 0,
            candidates: ptr::null_mut(),
            select_keys: ptr::null_mut(),
        },
        commit_text_preview: ptr::null_mut(),
        select_labels: ptr::null_mut(),
    }
}

fn call_void<T>(func: Option<T>, name: &'static str) -> Result<T> {
    func.ok_or_else(|| anyhow!("{name} unavailable"))
}

fn call_ret<T>(func: Option<T>, name: &'static str) -> Result<T> {
    func.ok_or_else(|| anyhow!("{name} unavailable"))
}
