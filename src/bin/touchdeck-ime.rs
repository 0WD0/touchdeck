use std::collections::VecDeque;
use std::env;
use std::ffi::{CStr, CString};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::{
    atomic::{AtomicU32, Ordering},
    mpsc::{self, Receiver, Sender},
    Arc,
};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache};
use memmap2::MmapMut;
use serde::{Deserialize, Serialize};
use tempfile::tempfile;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_region, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use touchdeck::protocol::{ImeCandidate, ImeCursorRect, ImeStatus};
use touchdeck::rime::*;
use touchdeck::x11_geometry::{X11GeometryProbe, X11WindowGeometry};
use x11rb::connection::Connection as X11Connection;
use x11rb::protocol::xproto::{KeyPressEvent, KEY_PRESS_EVENT};
use xim::x11rb::HasConnection;
use xim::{InputStyle, Server, ServerHandler, UserInputContext, XimConnections};
use zbus::{interface, message::Header, object_server::SignalEmitter, ObjectServer};
use zbus::names::OwnedBusName;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Structure, Value};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::{self, ZwpInputMethodManagerV2},
    zwp_input_popup_surface_v2::{self, ZwpInputPopupSurfaceV2},
    zwp_input_method_v2::{self, ZwpInputMethodV2},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::{self, ZwpVirtualKeyboardManagerV1},
    zwp_virtual_keyboard_v1::{self, ZwpVirtualKeyboardV1},
};

const RIME_FALSE: c_int = 0;
const RIME_TRUE: c_int = 1;
const RIME_SHIFT_MASK: u32 = 1 << 0;
const RIME_CONTROL_MASK: u32 = 1 << 2;
const RIME_ALT_MASK: u32 = 1 << 3;
const RIME_SUPER_MASK: u32 = 1 << 26;
const RIME_RELEASE_MASK: u32 = 1 << 30;
const RIME_MODULE_DEFAULT: &[u8] = b"default\0";
const RIME_MODULE_PLUGINS: &[u8] = b"plugins\0";
const RIME_ASCII_MODE: &[u8] = b"ascii_mode\0";

const XKB_SHIFT_MASK: u32 = 1 << 0;
const XKB_CONTROL_MASK: u32 = 1 << 2;
const XKB_ALT_MASK: u32 = 1 << 3;
const XKB_SUPER_MASK: u32 = 1 << 6;

const FCITX_INPUT_CONTEXT_INTERFACE: &str = "org.fcitx.Fcitx.InputContext1";

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
    popup_buffers: VecDeque<PopupBuffer>,
    text_renderer: TextRenderer,
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

struct PopupBuffer {
    _file: File,
    _mmap: MmapMut,
    _pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
    released: bool,
}

#[derive(Clone, Copy, Debug)]
struct RectPx {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

struct TextRenderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

#[derive(Clone, Debug)]
struct ImeRuntimeConfig {
    key_translation: KeyTranslationPolicy,
    popup: PopupConfig,
}

impl Default for ImeRuntimeConfig {
    fn default() -> Self {
        Self {
            key_translation: KeyTranslationPolicy::Effective,
            popup: PopupConfig::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyTranslationPolicy {
    Effective,
    Raw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyRoute {
    ImeKey,
    ImeText,
    AppKey,
    ImeOnly,
}

#[derive(Clone, Debug)]
struct PopupConfig {
    width: u32,
    max_candidates: usize,
    height_empty: u32,
    height_candidates: u32,
    header_height: i32,
    padding_x: i32,
    header_y: i32,
    candidate_gap: i32,
    candidate_min_width: i32,
    candidate_max_width: i32,
    candidate_unit_width: i32,
    candidate_extra_width: i32,
    preedit_font_size: f32,
    candidate_font_size: f32,
    background_color: Rgba,
    border_color: Rgba,
    separator_color: Rgba,
    preedit_color: Rgba,
    candidate_text_color: Rgba,
    highlight_background_color: Rgba,
    first_candidate_background_color: Rgba,
}

impl Default for PopupConfig {
    fn default() -> Self {
        Self {
            width: 560,
            max_candidates: 6,
            height_empty: 48,
            height_candidates: 88,
            header_height: 32,
            padding_x: 10,
            header_y: 5,
            candidate_gap: 6,
            candidate_min_width: 48,
            candidate_max_width: 154,
            candidate_unit_width: 8,
            candidate_extra_width: 26,
            preedit_font_size: 15.5,
            candidate_font_size: 16.0,
            background_color: Rgba::new(0x1a, 0x22, 0x26, 0xe6),
            border_color: Rgba::new(0x79, 0x8b, 0x86, 0x96),
            separator_color: Rgba::new(0x6c, 0x78, 0x72, 0x70),
            preedit_color: Rgba::new(0xd8, 0xde, 0xe8, 0xee),
            candidate_text_color: Rgba::new(0xff, 0xff, 0xff, 0xf0),
            highlight_background_color: Rgba::new(0x3b, 0x86, 0xf2, 0xdc),
            first_candidate_background_color: Rgba::new(0x2e, 0x3d, 0x44, 0x70),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Rgba {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl Rgba {
    const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    fn rgba(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    fn bgra(self) -> [u8; 4] {
        [self.b, self.g, self.r, self.a]
    }
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        }
    }
}

impl TextRenderer {
    fn draw_text(
        &mut self,
        buf: &mut [u8],
        width: u32,
        height: u32,
        rect: RectPx,
        text: &str,
        font_size: f32,
        color: [u8; 4],
    ) {
        if text.trim().is_empty() || rect.w <= 0 || rect.h <= 0 {
            return;
        }

        let metrics = Metrics::new(font_size.max(1.0), (font_size * 1.25).max(1.0));
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(Some(rect.w.max(1) as f32), Some(rect.h.max(1) as f32));
        let attrs = Attrs::new().family(Family::Name("Noto Sans CJK SC"));
        buffer.set_text(text, &attrs, Shaping::Advanced, None);

        let text_color = Color::rgba(color[0], color[1], color[2], color[3]);
        buffer.draw(
            &mut self.font_system,
            &mut self.swash_cache,
            text_color,
            |x, y, w, h, color| {
                blend_text_rect(
                    buf,
                    width,
                    height,
                    RectPx {
                        x: rect.x + x,
                        y: rect.y + y,
                        w: w as i32,
                        h: h as i32,
                    },
                    color,
                );
            },
        );
    }
}

#[derive(Debug, Deserialize)]
struct TouchDeckEvent {
    protocol: String,
    #[serde(rename = "type")]
    kind: String,
    source: String,
    #[serde(default)]
    time: u32,
    #[serde(default)]
    key: u32,
    #[serde(default)]
    state: String,
    #[serde(default)]
    modifiers: u32,
    #[serde(default)]
    translation: Option<String>,
    #[serde(default)]
    route: Option<String>,
}

enum TouchDeckRequest {
    Event {
        event: TouchDeckEvent,
        response: Sender<ImeStatus>,
    },
    Subscribe {
        response: Sender<ImeStatus>,
    },
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

enum FcitxDbusRequest {
    FocusIn {
        target: FcitxDbusTarget,
        response: Sender<()>,
    },
    FocusOut {
        target: FcitxDbusTarget,
        response: Sender<()>,
    },
    Reset {
        response: Sender<()>,
    },
    Key {
        keyval: u32,
        keycode: u32,
        state: u32,
        is_release: bool,
        time: u32,
        response: Sender<FcitxDbusKeyResponse>,
    },
    SetCursorRect {
        target: FcitxDbusTarget,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        scale: f64,
    },
    SetCapability {
        target: FcitxDbusTarget,
        capability: u64,
    },
    SetSupportedCapability {
        target: FcitxDbusTarget,
        capability: u64,
    },
    SetSurroundingText {
        text: String,
        cursor: u32,
        anchor: u32,
    },
}

#[derive(Clone, Debug)]
struct FcitxDbusTarget {
    path: OwnedObjectPath,
    client: OwnedBusName,
    display: String,
}

impl FcitxDbusTarget {
    fn matches(&self, other: &Self) -> bool {
        self.path.as_str() == other.path.as_str() && self.client.as_str() == other.client.as_str()
    }
}

#[derive(Debug)]
struct FcitxDbusOutput {
    target: FcitxDbusTarget,
    preedit: Option<String>,
    commit: Option<String>,
    status: ImeStatus,
    cursor_rect: Option<FcitxCursorRect>,
}

#[derive(Clone, Debug)]
struct FcitxCursorRect {
    target: FcitxDbusTarget,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    scale: f64,
    space: String,
    x11_window: Option<X11WindowGeometry>,
}

#[derive(Debug, Default)]
struct FcitxDbusKeyResponse {
    handled: bool,
    preedit: String,
    commit: Option<String>,
    status: ImeStatus,
}

const FCITX_BATCHED_COMMIT_STRING: u32 = 0;
const FCITX_BATCHED_PREEDIT: u32 = 1;
const FCITX_CAPABILITY_CLIENT_SIDE_INPUT_PANEL: u64 = 1 << 39;

type FcitxFormattedText = Vec<(String, i32)>;
type FcitxCandidateList = Vec<(String, String)>;
type FcitxClientSideUiBody = (
    FcitxFormattedText,
    i32,
    FcitxFormattedText,
    FcitxFormattedText,
    FcitxCandidateList,
    i32,
    i32,
    bool,
    bool,
);

#[derive(Clone, Copy, Debug)]
enum FcitxDbusUnitKind {
    FocusIn,
    FocusOut,
    Reset,
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
    status: ImeStatus,
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
            surface.attach(None::<&wl_buffer::WlBuffer>, 0, 0);
            surface.commit();
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
        let (backing, width, height) = self.create_popup_buffer(qh, &shm, status)?;
        surface.attach(Some(&backing.buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();

        self.popup_buffers.push_back(backing);
        self.popup_buffers.retain(|buffer| !buffer.released);
        while self.popup_buffers.len() > 8 {
            self.popup_buffers.pop_front();
        }

        Ok(())
    }

    fn create_popup_buffer(
        &mut self,
        qh: &QueueHandle<Self>,
        shm: &wl_shm::WlShm,
        status: &ImeStatus,
    ) -> Result<(PopupBuffer, i32, i32)> {
        let popup = self.config.popup.clone();
        let (width, height) = self.popup_dimensions(status);
        let stride = width
            .checked_mul(4)
            .ok_or_else(|| anyhow!("invalid popup buffer stride"))?;
        let len = stride
            .checked_mul(height)
            .ok_or_else(|| anyhow!("invalid popup buffer size"))?;

        let file = tempfile().context("create popup shm backing file")?;
        file.set_len(u64::from(len))
            .context("resize popup shm backing file")?;
        let mut mmap = unsafe { MmapMut::map_mut(&file).context("map popup shm backing file")? };
        mmap.fill(0);
        draw_popup_status(
            &mut self.text_renderer,
            &mut mmap,
            width,
            height,
            status,
            &popup,
        );
        mmap.flush().context("flush popup shm backing file")?;

        let pool = shm.create_pool(file.as_fd(), len as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            qh,
            (),
        );

        Ok((
            PopupBuffer {
                _file: file,
                _mmap: mmap,
                _pool: pool,
                buffer,
                released: false,
            },
            width as i32,
            height as i32,
        ))
    }

    fn popup_dimensions(&self, status: &ImeStatus) -> (u32, u32) {
        let popup = &self.config.popup;
        let width = env_u32("TOUCHDECK_IME_POPUP_WIDTH", popup.width).clamp(220, 1600);
        let candidate_count = status.candidates.iter().take(popup.max_candidates).count();
        let height = if candidate_count == 0 {
            popup.height_empty
        } else {
            popup.height_candidates
        };

        (width, height)
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

    fn handle_fcitx_dbus_request(
        &mut self,
        qh: &QueueHandle<Self>,
        request: FcitxDbusRequest,
    ) {
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
                            eprintln!("touchdeck-ime: failed to update popup after cursor rect: {err:?}");
                        }
                        self.broadcast_status("physical");
                    }
                }
            }
            FcitxDbusRequest::SetCapability { target, capability } => {
                let client_side =
                    (capability & FCITX_CAPABILITY_CLIENT_SIDE_INPUT_PANEL) != 0;
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
                let client_side =
                    (capability & FCITX_CAPABILITY_CLIENT_SIDE_INPUT_PANEL) != 0;
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

    fn handle_touchdeck_event(&mut self, qh: &QueueHandle<Self>, event: TouchDeckEvent) -> ImeStatus {
        if event.protocol != "touchdeck-ime-v1" || event.kind != "key" || event.source != "touchdeck" {
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
                state.shm = Some(registry.bind::<wl_shm::WlShm, _, _>(
                    name,
                    version.min(1),
                    qh,
                    (),
                ));
                eprintln!("touchdeck-ime: bound wl_shm");
            }
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
            for backing in &mut state.popup_buffers {
                if backing.buffer == buffer.clone() {
                    backing.released = true;
                    break;
                }
            }
            state.popup_buffers.retain(|backing| !backing.released);
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

struct RimeEngine {
    api: NonNull<RimeApi>,
    session: RimeSessionId,
    key_translation: KeyTranslationPolicy,
    _shared_data_dir: CString,
    _user_data_dir: CString,
    _prebuilt_data_dir: CString,
    _staging_dir: CString,
    _app_name: CString,
    _log_dir: CString,
}

impl RimeEngine {
    fn new(key_translation: KeyTranslationPolicy) -> Result<Self> {
        let shared_data_dir_path = default_rime_shared_data_dir();
        let user_data_dir_path = env_path("TOUCHDECK_RIME_USER_DATA_DIR")
            .unwrap_or_else(default_rime_user_data_dir);
        let prebuilt_data_dir_path = shared_data_dir_path.join("build");
        let staging_dir_path = user_data_dir_path.join("build");

        if !shared_data_dir_path.join("default.yaml").exists() {
            return Err(anyhow!(
                "Rime shared data dir {} does not contain default.yaml",
                shared_data_dir_path.display()
            ));
        }

        fs::create_dir_all(&user_data_dir_path).with_context(|| {
            format!(
                "create Rime user data dir {}",
                user_data_dir_path.display()
            )
        })?;

        eprintln!(
            "touchdeck-ime: rime dirs shared={} user={} prebuilt={} staging={}",
            shared_data_dir_path.display(),
            user_data_dir_path.display(),
            prebuilt_data_dir_path.display(),
            staging_dir_path.display()
        );

        let shared_data_dir = path_to_cstring(&shared_data_dir_path)?;
        let user_data_dir = path_to_cstring(&user_data_dir_path)?;
        let prebuilt_data_dir = path_to_cstring(&prebuilt_data_dir_path)?;
        let staging_dir = path_to_cstring(&staging_dir_path)?;
        let app_name = CString::new("rime.touchdeck").expect("static string has no NUL");
        let log_dir = CString::new(env::var("TOUCHDECK_RIME_LOG_DIR").unwrap_or_default())
            .context("TOUCHDECK_RIME_LOG_DIR contains NUL")?;

        let api = NonNull::new(unsafe { rime_get_api() }).context("rime_get_api returned null")?;
        let rime_modules = [
            RIME_MODULE_DEFAULT.as_ptr() as *const c_char,
            RIME_MODULE_PLUGINS.as_ptr() as *const c_char,
            ptr::null(),
        ];

        let mut traits = RimeTraits {
            data_size: rime_traits_data_size(),
            shared_data_dir: shared_data_dir.as_ptr(),
            user_data_dir: user_data_dir.as_ptr(),
            distribution_name: ptr::null(),
            distribution_code_name: ptr::null(),
            distribution_version: ptr::null(),
            app_name: app_name.as_ptr(),
            modules: rime_modules.as_ptr(),
            min_log_level: env::var("TOUCHDECK_RIME_LOG_LEVEL")
                .ok()
                .and_then(|value| value.parse::<c_int>().ok())
                .unwrap_or(1),
            log_dir: log_dir.as_ptr(),
            prebuilt_data_dir: prebuilt_data_dir.as_ptr(),
            staging_dir: staging_dir.as_ptr(),
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
            key_translation,
            _shared_data_dir: shared_data_dir,
            _user_data_dir: user_data_dir,
            _prebuilt_data_dir: prebuilt_data_dir,
            _staging_dir: staging_dir,
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
        translation: Option<KeyTranslationPolicy>,
    ) -> Result<RimeOutput> {
        let mut mask = rime_modifier_mask(xkb_modifiers);
        if state == KeyState::Released {
            mask |= RIME_RELEASE_MASK;
        }
        let keysym = match translation.unwrap_or(self.key_translation) {
            KeyTranslationPolicy::Effective => rime_effective_keysym(keysym, mask),
            KeyTranslationPolicy::Raw => keysym,
        };

        let handled = unsafe {
            let process_key = call_ret(self.api().process_key, "RimeApi.process_key")?;
            process_key(self.session, keysym as c_int, mask as c_int) != RIME_FALSE
        };

        let commit = self.take_commit()?;
        let status = self.current_status()?;

        Ok(RimeOutput {
            handled,
            commit,
            status,
        })
    }

    fn clear(&mut self) {
        unsafe {
            if let Some(clear) = self.api().clear_composition {
                clear(self.session);
            }
        }
    }

    fn set_ascii_mode(&mut self, ascii: bool) {
        unsafe {
            if let Some(set_option) = self.api().set_option {
                set_option(
                    self.session,
                    RIME_ASCII_MODE.as_ptr() as *const c_char,
                    if ascii { RIME_TRUE } else { RIME_FALSE },
                );
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

    fn current_status(&self) -> Result<ImeStatus> {
        unsafe {
            let Some(get_context) = self.api().get_context else {
                return Ok(ImeStatus::default());
            };
            let Some(free_context) = self.api().free_context else {
                return Ok(ImeStatus::default());
            };

            let mut context = empty_rime_context();
            if get_context(self.session, &mut context) == RIME_FALSE {
                return Ok(ImeStatus::default());
            }

            let preedit = if context.composition.preedit.is_null() {
                String::new()
            } else {
                CStr::from_ptr(context.composition.preedit)
                    .to_string_lossy()
                    .into_owned()
            };
            let commit_preview = c_string_lossy(context.commit_text_preview);
            let candidates = context_candidates(&context);
            let highlighted_candidate_index = if context.menu.highlighted_candidate_index >= 0 {
                Some(context.menu.highlighted_candidate_index as usize)
            } else {
                None
            };
            let status = ImeStatus {
                active: true,
                preedit,
                commit_preview,
                candidates,
                highlighted_candidate_index,
                page_no: context.menu.page_no,
                is_last_page: context.menu.is_last_page != RIME_FALSE,
                ..ImeStatus::default()
            };
            free_context(&mut context);
            Ok(status)
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
        if self.tx.send(XimRequest::Reset { response: response_tx }).is_err() {
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

#[derive(Clone)]
struct FcitxInputMethod {
    tx: Sender<FcitxDbusRequest>,
    next_id: Arc<AtomicU32>,
}

#[interface(name = "org.fcitx.Fcitx.InputMethod1")]
impl FcitxInputMethod {
    #[zbus(name = "CreateInputContext")]
    async fn create_input_context(
        &self,
        args: Vec<(String, String)>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<(OwnedObjectPath, Vec<u8>)> {
        let sender = dbus_sender(&header)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let path_string = format!("/org/freedesktop/portal/inputcontext/{id}");
        let path = OwnedObjectPath::try_from(path_string.clone())
            .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))?;
        let display = args
            .iter()
            .find(|(key, _)| key == "display")
            .map(|(_, value)| value.clone())
            .unwrap_or_default();
        let ic = FcitxInputContext {
            tx: self.tx.clone(),
            path: path.clone(),
            client: sender.clone(),
            display: display.clone(),
        };

        server
            .at(path_string.as_str(), ic)
            .await
            .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))?;

        eprintln!(
            "touchdeck-ime: fcitx dbus create input context id={id} sender={} display={display:?} args={args:?}",
            sender.as_str(),
        );

        Ok((path, fcitx_uuid_bytes(id)))
    }

    #[zbus(name = "Version")]
    fn version(&self) -> u32 {
        1
    }
}

struct FcitxInputContext {
    tx: Sender<FcitxDbusRequest>,
    path: OwnedObjectPath,
    client: OwnedBusName,
    display: String,
}

impl FcitxInputContext {
    fn check_sender(&self, header: &Header<'_>) -> bool {
        header
            .sender()
            .map(|sender| sender.to_string() == self.client.to_string())
            .unwrap_or(false)
    }

    fn send_unit(&self, kind: FcitxDbusUnitKind, what: &str) -> zbus::fdo::Result<()> {
        let (response_tx, response_rx) = mpsc::channel();
        let target = FcitxDbusTarget {
            path: self.path.clone(),
            client: self.client.clone(),
            display: self.display.clone(),
        };
        let request = match kind {
            FcitxDbusUnitKind::FocusIn => FcitxDbusRequest::FocusIn {
                target,
                response: response_tx,
            },
            FcitxDbusUnitKind::FocusOut => FcitxDbusRequest::FocusOut {
                target,
                response: response_tx,
            },
            FcitxDbusUnitKind::Reset => FcitxDbusRequest::Reset {
                response: response_tx,
            },
        };
        self.tx
            .send(request)
            .map_err(|err| zbus::fdo::Error::Failed(format!("{what}: {err}")))?;
        response_rx
            .recv_timeout(Duration::from_millis(500))
            .map_err(|err| zbus::fdo::Error::Failed(format!("{what}: {err}")))
    }

    fn request_key(
        &self,
        keyval: u32,
        keycode: u32,
        state: u32,
        is_release: bool,
        time: u32,
    ) -> zbus::fdo::Result<FcitxDbusKeyResponse> {
        let (response_tx, response_rx) = mpsc::channel();
        self.tx
            .send(FcitxDbusRequest::Key {
                keyval,
                keycode,
                state,
                is_release,
                time,
                response: response_tx,
            })
            .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))?;
        response_rx
            .recv_timeout(Duration::from_millis(500))
            .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))
    }

    fn send_cursor_rect(&self, x: i32, y: i32, w: i32, h: i32, scale: f64) {
        let _ = self.tx.send(FcitxDbusRequest::SetCursorRect {
            target: FcitxDbusTarget {
                path: self.path.clone(),
                client: self.client.clone(),
                display: self.display.clone(),
            },
            x,
            y,
            w,
            h,
            scale,
        });
    }

    fn send_capability(&self, capability: u64, supported: bool) {
        let target = FcitxDbusTarget {
            path: self.path.clone(),
            client: self.client.clone(),
            display: self.display.clone(),
        };
        let request = if supported {
            FcitxDbusRequest::SetSupportedCapability { target, capability }
        } else {
            FcitxDbusRequest::SetCapability { target, capability }
        };
        let _ = self.tx.send(request);
    }

    fn send_surrounding_text(&self, text: String, cursor: u32, anchor: u32) {
        let _ = self.tx.send(FcitxDbusRequest::SetSurroundingText {
            text,
            cursor,
            anchor,
        });
    }

    async fn emit_commit_string(
        &self,
        conn: &zbus::Connection,
        text: &str,
    ) -> zbus::fdo::Result<()> {
        conn.emit_signal(
            Some(&self.client),
            &self.path,
            FCITX_INPUT_CONTEXT_INTERFACE,
            "CommitString",
            &text,
        )
        .await
        .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))
    }

    async fn emit_current_im(&self, conn: &zbus::Connection) -> zbus::fdo::Result<()> {
        conn.emit_signal(
            Some(&self.client),
            &self.path,
            FCITX_INPUT_CONTEXT_INTERFACE,
            "CurrentIM",
            &("rime".to_string(), "Rime".to_string(), "zh".to_string()),
        )
        .await
        .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))
    }

    async fn emit_update_formatted_preedit(
        &self,
        conn: &zbus::Connection,
        preedit: &str,
    ) -> zbus::fdo::Result<()> {
        let body = fcitx_formatted_preedit_body(preedit);
        conn.emit_signal(
            Some(&self.client),
            &self.path,
            FCITX_INPUT_CONTEXT_INTERFACE,
            "UpdateFormattedPreedit",
            &body,
        )
        .await
        .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))
    }

    async fn emit_update_client_side_ui(
        &self,
        conn: &zbus::Connection,
        status: &ImeStatus,
    ) -> zbus::fdo::Result<()> {
        let body = fcitx_client_side_ui_body(status);
        log_fcitx_client_side_ui("signal", status, &body);
        conn.emit_signal(
            Some(&self.client),
            &self.path,
            FCITX_INPUT_CONTEXT_INTERFACE,
            "UpdateClientSideUI",
            &body,
        )
        .await
        .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))
    }

    fn batched_events(
        &self,
        response: &FcitxDbusKeyResponse,
    ) -> zbus::fdo::Result<Vec<(u32, OwnedValue)>> {
        let mut events = Vec::new();

        if let Some(commit) = &response.commit {
            events.push((
                FCITX_BATCHED_COMMIT_STRING,
                owned_value(Value::from(commit.clone()))?,
            ));
        }

        if response.handled || response.commit.is_some() || !response.preedit.is_empty() {
            events.push((
                FCITX_BATCHED_PREEDIT,
                owned_value(fcitx_formatted_preedit_value(&response.preedit))?,
            ));
        }

        Ok(events)
    }
}

fn fcitx_formatted_preedit_value(preedit: &str) -> Value<'static> {
    Value::Structure(Structure::from(fcitx_formatted_preedit_body(preedit)))
}

fn fcitx_formatted_preedit_body(preedit: &str) -> (Vec<(String, i32)>, i32) {
    let formatted = fcitx_formatted_text(preedit);
    let cursor = preedit.len().min(i32::MAX as usize) as i32;
    (formatted, cursor)
}

fn fcitx_formatted_text(text: &str) -> FcitxFormattedText {
    if text.is_empty() {
        Vec::new()
    } else {
        vec![(text.to_string(), 0_i32)]
    }
}

fn fcitx_client_side_ui_body(status: &ImeStatus) -> FcitxClientSideUiBody {
    let preedit = fcitx_formatted_text(&status.preedit);
    let preedit_cursor = status.preedit.len().min(i32::MAX as usize) as i32;
    let aux_up = FcitxFormattedText::new();
    let aux_down = if status.commit_preview.is_empty() {
        FcitxFormattedText::new()
    } else {
        fcitx_formatted_text(&status.commit_preview)
    };
    let candidates = status
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let label = if candidate.label.is_empty() {
                (index + 1).to_string()
            } else {
                candidate.label.clone()
            };
            let text = if candidate.comment.is_empty() {
                candidate.text.clone()
            } else {
                format!("{} {}", candidate.text, candidate.comment)
            };
            (label, text)
        })
        .collect::<Vec<_>>();
    let cursor_index = if candidates.is_empty() {
        -1
    } else {
        status
            .highlighted_candidate_index
            .unwrap_or(0)
            .min(i32::MAX as usize) as i32
    };
    let layout_hint = 0;
    let has_prev = status.page_no > 0;
    let has_next = !status.is_last_page && !candidates.is_empty();

    (
        preedit,
        preedit_cursor,
        aux_up,
        aux_down,
        candidates,
        cursor_index,
        layout_hint,
        has_prev,
        has_next,
    )
}

fn fcitx_client_side_ui_visible(body: &FcitxClientSideUiBody) -> bool {
    !body.0.is_empty() || !body.2.is_empty() || !body.3.is_empty() || !body.4.is_empty()
}

fn log_fcitx_client_side_ui(what: &str, status: &ImeStatus, body: &FcitxClientSideUiBody) {
    eprintln!(
        "touchdeck-ime: fcitx dbus {what} UpdateClientSideUI visible={} preedit={:?} candidates={} candidate_index={} has_prev={} has_next={}",
        fcitx_client_side_ui_visible(body),
        status.preedit,
        body.4.len(),
        body.5,
        body.7,
        body.8
    );
}

fn owned_value(value: Value<'static>) -> zbus::fdo::Result<OwnedValue> {
    OwnedValue::try_from(value).map_err(|err| zbus::fdo::Error::Failed(err.to_string()))
}

#[interface(name = "org.fcitx.Fcitx.InputContext1")]
impl FcitxInputContext {
    #[zbus(name = "FocusIn")]
    async fn focus_in(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if !self.check_sender(&header) {
            return Ok(());
        }
        self.send_unit(FcitxDbusUnitKind::FocusIn, "FocusIn")?;
        self.emit_current_im(conn).await
    }

    #[zbus(name = "FocusOut")]
    fn focus_out(&self, #[zbus(header)] header: Header<'_>) -> zbus::fdo::Result<()> {
        if !self.check_sender(&header) {
            return Ok(());
        }
        self.send_unit(FcitxDbusUnitKind::FocusOut, "FocusOut")
    }

    #[zbus(name = "Reset")]
    fn reset(&self, #[zbus(header)] header: Header<'_>) -> zbus::fdo::Result<()> {
        if !self.check_sender(&header) {
            return Ok(());
        }
        self.send_unit(FcitxDbusUnitKind::Reset, "Reset")
    }

    #[zbus(name = "SetCursorRect")]
    fn set_cursor_rect(
        &self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        #[zbus(header)] header: Header<'_>,
    ) {
        if self.check_sender(&header) {
            self.send_cursor_rect(x, y, w, h, 1.0);
        }
    }

    #[zbus(name = "SetCursorRectV2")]
    fn set_cursor_rect_v2(
        &self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        scale: f64,
        #[zbus(header)] header: Header<'_>,
    ) {
        if self.check_sender(&header) {
            self.send_cursor_rect(x, y, w, h, scale);
        }
    }

    #[zbus(name = "SetCapability")]
    fn set_capability(&self, capability: u64, #[zbus(header)] header: Header<'_>) {
        if self.check_sender(&header) {
            self.send_capability(capability, false);
        }
    }

    #[zbus(name = "SetSupportedCapability")]
    fn set_supported_capability(&self, capability: u64, #[zbus(header)] header: Header<'_>) {
        if self.check_sender(&header) {
            self.send_capability(capability, true);
        }
    }

    #[zbus(name = "SetSurroundingText")]
    fn set_surrounding_text(
        &self,
        text: String,
        cursor: u32,
        anchor: u32,
        #[zbus(header)] header: Header<'_>,
    ) {
        if self.check_sender(&header) {
            self.send_surrounding_text(text, cursor, anchor);
        }
    }

    #[zbus(name = "SetSurroundingTextPosition")]
    fn set_surrounding_text_position(
        &self,
        cursor: u32,
        anchor: u32,
        #[zbus(header)] header: Header<'_>,
    ) {
        if self.check_sender(&header) {
            self.send_surrounding_text(String::new(), cursor, anchor);
        }
    }

    #[zbus(name = "DestroyIC")]
    fn destroy_ic(&self, #[zbus(header)] header: Header<'_>) -> zbus::fdo::Result<()> {
        if !self.check_sender(&header) {
            return Ok(());
        }
        self.send_unit(FcitxDbusUnitKind::FocusOut, "DestroyIC")
    }

    #[zbus(name = "ProcessKeyEvent")]
    async fn process_key_event(
        &self,
        keyval: u32,
        keycode: u32,
        state: u32,
        is_release: bool,
        time: u32,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<bool> {
        if !self.check_sender(&header) {
            return Ok(false);
        }
        let response = self.request_key(keyval, keycode, state, is_release, time)?;
        if let Some(commit) = &response.commit {
            self.emit_commit_string(conn, commit).await?;
        }
        self.emit_update_formatted_preedit(conn, &response.preedit)
            .await?;
        self.emit_update_client_side_ui(conn, &response.status)
            .await?;
        Ok(response.handled)
    }

    #[zbus(name = "ProcessKeyEventBatch")]
    async fn process_key_event_batch(
        &self,
        keyval: u32,
        keycode: u32,
        state: u32,
        is_release: bool,
        time: u32,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(Vec<(u32, OwnedValue)>, bool)> {
        if !self.check_sender(&header) {
            return Ok((Vec::new(), false));
        }
        let response = self.request_key(keyval, keycode, state, is_release, time)?;
        let events = self.batched_events(&response)?;
        self.emit_update_client_side_ui(conn, &response.status)
            .await?;
        Ok((events, response.handled))
    }

    #[zbus(name = "PrevPage")]
    fn prev_page(&self, #[zbus(header)] _header: Header<'_>) {}

    #[zbus(name = "NextPage")]
    fn next_page(&self, #[zbus(header)] _header: Header<'_>) {}

    #[zbus(name = "SelectCandidate")]
    fn select_candidate(&self, _idx: i32, #[zbus(header)] _header: Header<'_>) {}

    #[zbus(name = "InvokeAction")]
    fn invoke_action(&self, _action: u32, _cursor: i32, #[zbus(header)] _header: Header<'_>) {}

    #[zbus(name = "IsVirtualKeyboardVisible")]
    fn is_virtual_keyboard_visible(&self, #[zbus(header)] _header: Header<'_>) -> bool {
        false
    }

    #[zbus(name = "ShowVirtualKeyboard")]
    fn show_virtual_keyboard(&self, #[zbus(header)] _header: Header<'_>) {}

    #[zbus(name = "HideVirtualKeyboard")]
    fn hide_virtual_keyboard(&self, #[zbus(header)] _header: Header<'_>) {}

    #[zbus(signal, name = "CommitString")]
    async fn commit_string_signal(
        emitter: &SignalEmitter<'_>,
        str: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "CurrentIM")]
    async fn current_im_signal(
        emitter: &SignalEmitter<'_>,
        name: &str,
        unique_name: &str,
        lang_code: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "UpdateFormattedPreedit")]
    async fn update_formatted_preedit_signal(
        emitter: &SignalEmitter<'_>,
        str: FcitxFormattedText,
        cursorpos: i32,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "UpdateClientSideUI")]
    async fn update_client_side_ui_signal(
        emitter: &SignalEmitter<'_>,
        preedit: FcitxFormattedText,
        cursorpos: i32,
        aux_up: FcitxFormattedText,
        aux_down: FcitxFormattedText,
        candidates: FcitxCandidateList,
        candidate_index: i32,
        layout_hint: i32,
        has_prev: bool,
        has_next: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "DeleteSurroundingText")]
    async fn delete_surrounding_text_signal(
        emitter: &SignalEmitter<'_>,
        offset: i32,
        nchar: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "ForwardKey")]
    async fn forward_key_signal(
        emitter: &SignalEmitter<'_>,
        keyval: u32,
        state: u32,
        type_: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "NotifyFocusOut")]
    async fn notify_focus_out_signal(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal, name = "VirtualKeyboardVisibilityChanged")]
    async fn virtual_keyboard_visibility_changed_signal(
        emitter: &SignalEmitter<'_>,
        visible: bool,
    ) -> zbus::Result<()>;
}

fn dbus_sender(header: &Header<'_>) -> zbus::fdo::Result<OwnedBusName> {
    let sender = header
        .sender()
        .ok_or_else(|| zbus::fdo::Error::Failed("D-Bus message has no sender".to_string()))?;
    OwnedBusName::try_from(sender.to_string())
        .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))
}

fn fcitx_uuid_bytes(id: u32) -> Vec<u8> {
    let mut bytes = vec![0_u8; 16];
    bytes[0..4].copy_from_slice(&id.to_le_bytes());
    bytes[4..8].copy_from_slice(b"TDIM");
    bytes[8..12].copy_from_slice(&id.wrapping_mul(1103515245).to_le_bytes());
    bytes[12..16].copy_from_slice(b"RIME");
    bytes
}

fn spawn_fcitx_dbus_server(tx: Sender<FcitxDbusRequest>) -> Sender<FcitxDbusOutput> {
    let (output_tx, output_rx) = mpsc::channel();
    thread::spawn(move || {
        if let Err(err) = zbus::block_on(run_fcitx_dbus_server(tx, output_rx)) {
            eprintln!("touchdeck-ime: fcitx dbus server stopped: {err:?}");
        }
    });
    output_tx
}

async fn run_fcitx_dbus_server(
    tx: Sender<FcitxDbusRequest>,
    output_rx: Receiver<FcitxDbusOutput>,
) -> Result<()> {
    let next_id = Arc::new(AtomicU32::new(1));
    let input_method = FcitxInputMethod { tx, next_id };
    let conn = zbus::connection::Builder::session()
        .context("connect to session D-Bus for fcitx frontend")?
        .serve_at(
            "/org/freedesktop/portal/inputmethod",
            input_method.clone(),
        )
        .context("serve fcitx input method object")?
        .serve_at("/inputmethod", input_method)
        .context("serve compatible fcitx input method object")?
        .name("org.fcitx.Fcitx5")
        .context("request org.fcitx.Fcitx5")?
        .name("org.freedesktop.portal.Fcitx")
        .context("request org.freedesktop.portal.Fcitx")?
        .build()
        .await
        .context("build fcitx D-Bus service")?;

    let output_conn = conn.clone();
    thread::spawn(move || {
        while let Ok(output) = output_rx.recv() {
            if let Err(err) = zbus::block_on(emit_fcitx_dbus_output(&output_conn, output)) {
                eprintln!("touchdeck-ime: fcitx dbus output error: {err:?}");
            }
        }
    });

    eprintln!(
        "touchdeck-ime: fcitx dbus frontend initialized as org.fcitx.Fcitx5 and org.freedesktop.portal.Fcitx"
    );
    std::future::pending::<()>().await;
    Ok(())
}

async fn emit_fcitx_dbus_output(
    conn: &zbus::Connection,
    output: FcitxDbusOutput,
) -> zbus::fdo::Result<()> {
    if let Some(rect) = &output.cursor_rect {
        eprintln!(
            "touchdeck-ime: fcitx dbus ui anchor path={} x={} y={} w={} h={} scale={}",
            rect.target.path.as_str(),
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            rect.scale
        );
    }

    if let Some(commit) = output.commit {
        conn.emit_signal(
            Some(&output.target.client),
            &output.target.path,
            FCITX_INPUT_CONTEXT_INTERFACE,
            "CommitString",
            &commit,
        )
        .await
        .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))?;
    }

    if let Some(preedit) = output.preedit {
        let formatted = fcitx_formatted_preedit_body(&preedit);
        conn.emit_signal(
            Some(&output.target.client),
            &output.target.path,
            FCITX_INPUT_CONTEXT_INTERFACE,
            "UpdateFormattedPreedit",
            &formatted,
        )
        .await
        .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))?;
    }

    let client_side_ui = fcitx_client_side_ui_body(&output.status);
    log_fcitx_client_side_ui("output", &output.status, &client_side_ui);
    conn.emit_signal(
        Some(&output.target.client),
        &output.target.path,
        FCITX_INPUT_CONTEXT_INTERFACE,
        "UpdateClientSideUI",
        &client_side_ui,
    )
    .await
    .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))?;

    Ok(())
}

fn spawn_socket_listener(socket_path: PathBuf) -> Result<Receiver<TouchDeckRequest>> {
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

fn load_ime_config() -> Result<ImeRuntimeConfig> {
    let Some(path) = config_path() else {
        return Ok(ImeRuntimeConfig::default());
    };
    let source = fs::read_to_string(&path)
        .with_context(|| format!("read touchdeck config {}", path.display()))?;
    let file_config: TouchDeckImeConfigFile = toml::from_str(&source)
        .with_context(|| format!("parse touchdeck config {}", path.display()))?;
    let mut config = ImeRuntimeConfig::default();
    if let Some(ime) = file_config.ime {
        if let Some(policy) = ime.key_translation {
            config.key_translation = parse_key_translation_policy(&policy)?;
        }
        if let Some(popup) = ime.popup {
            config.popup.apply(popup)?;
        }
    }
    Ok(config)
}

fn config_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("TOUCHDECK_CONFIG") {
        return Some(PathBuf::from(path));
    }
    let default_path = PathBuf::from("touchdeck.toml");
    default_path.exists().then_some(default_path)
}

#[derive(Debug, Default, Deserialize)]
struct TouchDeckImeConfigFile {
    ime: Option<ImeConfigFile>,
}

#[derive(Debug, Default, Deserialize)]
struct ImeConfigFile {
    key_translation: Option<String>,
    popup: Option<PopupConfigFile>,
}

#[derive(Debug, Default, Deserialize)]
struct PopupConfigFile {
    width: Option<u32>,
    max_candidates: Option<usize>,
    height_empty: Option<u32>,
    height_candidates: Option<u32>,
    header_height: Option<i32>,
    padding_x: Option<i32>,
    header_y: Option<i32>,
    candidate_gap: Option<i32>,
    candidate_min_width: Option<i32>,
    candidate_max_width: Option<i32>,
    candidate_unit_width: Option<i32>,
    candidate_extra_width: Option<i32>,
    preedit_font_size: Option<f32>,
    candidate_font_size: Option<f32>,
    background_color: Option<String>,
    border_color: Option<String>,
    separator_color: Option<String>,
    preedit_color: Option<String>,
    candidate_text_color: Option<String>,
    highlight_background_color: Option<String>,
    first_candidate_background_color: Option<String>,
}

fn parse_key_translation_policy(value: &str) -> Result<KeyTranslationPolicy> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "effective" | "effective_keysym" | "translated" => Ok(KeyTranslationPolicy::Effective),
        "raw" | "raw_keysym" | "base" => Ok(KeyTranslationPolicy::Raw),
        other => Err(anyhow!("unknown ime.key_translation {other:?}")),
    }
}

fn parse_key_route(value: &str) -> Result<KeyRoute> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "ime" | "ime_key" | "ime_first" | "rime" | "rime_first" => Ok(KeyRoute::ImeKey),
        "ime_text" | "text" | "commit_text" => Ok(KeyRoute::ImeText),
        "app" | "app_key" | "direct" | "passthrough" | "forward" => Ok(KeyRoute::AppKey),
        "ime_only" | "rime_only" | "consume" | "filter" => Ok(KeyRoute::ImeOnly),
        other => Err(anyhow!("unknown key route {other:?}")),
    }
}

impl PopupConfig {
    fn apply(&mut self, value: PopupConfigFile) -> Result<()> {
        if let Some(width) = value.width {
            self.width = width;
        }
        if let Some(max_candidates) = value.max_candidates {
            self.max_candidates = max_candidates.max(1);
        }
        if let Some(height_empty) = value.height_empty {
            self.height_empty = height_empty;
        }
        if let Some(height_candidates) = value.height_candidates {
            self.height_candidates = height_candidates;
        }
        if let Some(header_height) = value.header_height {
            self.header_height = header_height.max(1);
        }
        if let Some(padding_x) = value.padding_x {
            self.padding_x = padding_x.max(0);
        }
        if let Some(header_y) = value.header_y {
            self.header_y = header_y.max(0);
        }
        if let Some(candidate_gap) = value.candidate_gap {
            self.candidate_gap = candidate_gap.max(0);
        }
        if let Some(candidate_min_width) = value.candidate_min_width {
            self.candidate_min_width = candidate_min_width.max(1);
        }
        if let Some(candidate_max_width) = value.candidate_max_width {
            self.candidate_max_width = candidate_max_width.max(self.candidate_min_width);
        }
        if let Some(candidate_unit_width) = value.candidate_unit_width {
            self.candidate_unit_width = candidate_unit_width.max(1);
        }
        if let Some(candidate_extra_width) = value.candidate_extra_width {
            self.candidate_extra_width = candidate_extra_width.max(0);
        }
        if let Some(preedit_font_size) = value.preedit_font_size {
            self.preedit_font_size = preedit_font_size.max(1.0);
        }
        if let Some(candidate_font_size) = value.candidate_font_size {
            self.candidate_font_size = candidate_font_size.max(1.0);
        }
        if let Some(color) = value.background_color {
            self.background_color = parse_hex_color(&color, "ime.popup.background_color")?;
        }
        if let Some(color) = value.border_color {
            self.border_color = parse_hex_color(&color, "ime.popup.border_color")?;
        }
        if let Some(color) = value.separator_color {
            self.separator_color = parse_hex_color(&color, "ime.popup.separator_color")?;
        }
        if let Some(color) = value.preedit_color {
            self.preedit_color = parse_hex_color(&color, "ime.popup.preedit_color")?;
        }
        if let Some(color) = value.candidate_text_color {
            self.candidate_text_color =
                parse_hex_color(&color, "ime.popup.candidate_text_color")?;
        }
        if let Some(color) = value.highlight_background_color {
            self.highlight_background_color =
                parse_hex_color(&color, "ime.popup.highlight_background_color")?;
        }
        if let Some(color) = value.first_candidate_background_color {
            self.first_candidate_background_color =
                parse_hex_color(&color, "ime.popup.first_candidate_background_color")?;
        }
        Ok(())
    }
}

fn parse_hex_color(value: &str, name: &str) -> Result<Rgba> {
    let hex = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if hex.len() != 6 && hex.len() != 8 {
        return Err(anyhow!(
            "{name} must be #RRGGBB or #RRGGBBAA, got {value:?}"
        ));
    }
    let r = parse_hex_byte(hex, 0, name)?;
    let g = parse_hex_byte(hex, 2, name)?;
    let b = parse_hex_byte(hex, 4, name)?;
    let a = if hex.len() == 8 {
        parse_hex_byte(hex, 6, name)?
    } else {
        0xff
    };
    Ok(Rgba::new(r, g, b, a))
}

fn parse_hex_byte(hex: &str, start: usize, name: &str) -> Result<u8> {
    u8::from_str_radix(&hex[start..start + 2], 16)
        .with_context(|| format!("parse {name} color component"))
}

fn default_rime_shared_data_dir() -> PathBuf {
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

fn handle_client(mut stream: UnixStream, tx: Sender<TouchDeckRequest>) -> Result<()> {
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

unsafe fn context_candidates(context: &RimeContext) -> Vec<ImeCandidate> {
    let count = context.menu.num_candidates.max(0) as usize;
    if count == 0 || context.menu.candidates.is_null() {
        return Vec::new();
    }

    let select_keys = c_string_lossy(context.menu.select_keys);
    let select_key_chars = select_keys.chars().collect::<Vec<_>>();
    let has_select_labels = !context.select_labels.is_null();

    let mut candidates = Vec::with_capacity(count);
    for index in 0..count {
        let candidate = &*context.menu.candidates.add(index);
        let label = if has_select_labels && index < context.menu.page_size.max(0) as usize {
            c_string_lossy(*context.select_labels.add(index))
        } else if let Some(ch) = select_key_chars.get(index) {
            ch.to_string()
        } else {
            ((index + 1) % 10).to_string()
        };

        candidates.push(ImeCandidate {
            label,
            text: c_string_lossy(candidate.text),
            comment: c_string_lossy(candidate.comment),
        });
    }

    candidates
}

unsafe fn c_string_lossy(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
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

fn x_keycode_to_keysym(keycode: u8) -> Option<u32> {
    let evdev_key = u32::from(keycode).checked_sub(8)?;
    evdev_key_to_keysym(evdev_key)
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

fn rime_effective_keysym(keysym: u32, rime_mask: u32) -> u32 {
    keysym_to_text(keysym, rime_mask)
        .and_then(|text| {
            let mut chars = text.chars();
            let ch = chars.next()?;
            chars.next().is_none().then_some(ch as u32)
        })
        .unwrap_or(keysym)
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

fn is_empty_state_passthrough_key(keysym: u32) -> bool {
    matches!(keysym, XK_BACKSPACE | XK_DELETE)
}

fn draw_popup_status(
    renderer: &mut TextRenderer,
    buf: &mut [u8],
    width: u32,
    height: u32,
    status: &ImeStatus,
    popup: &PopupConfig,
) {
    let panel = RectPx {
        x: 0,
        y: 0,
        w: width as i32,
        h: height as i32,
    };
    fill_rect(buf, width, height, panel, popup.background_color.bgra());
    draw_rect_frame(buf, width, height, panel, popup.border_color.bgra());

    let header_h = if status.candidates.is_empty() {
        height as i32
    } else {
        popup.header_height.min(height as i32)
    };
    let mut header = String::new();
    if !status.preedit.is_empty() {
        header.push_str(&status.preedit);
    }
    if !status.commit_preview.is_empty() {
        if !header.is_empty() {
            header.push_str(" > ");
        }
        header.push_str(&status.commit_preview);
    }
    if header.is_empty() {
        header.push_str("IME");
    }

    renderer.draw_text(
        buf,
        width,
        height,
        RectPx {
            x: popup.padding_x + 2,
            y: popup.header_y,
            w: width as i32 - (popup.padding_x + 2) * 2,
            h: header_h - popup.header_y,
        },
        &header,
        popup.preedit_font_size,
        popup.preedit_color.rgba(),
    );

    let visible = status
        .candidates
        .iter()
        .take(popup.max_candidates)
        .collect::<Vec<_>>();
    if visible.is_empty() {
        return;
    }

    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: popup.padding_x,
            y: header_h,
            w: width as i32 - popup.padding_x * 2,
            h: 1,
        },
        popup.separator_color.bgra(),
    );

    let gap = popup.candidate_gap;
    let row_y = header_h + 7;
    let row_h = height as i32 - row_y - 8;
    let mut x = popup.padding_x;
    for (index, candidate) in visible.into_iter().enumerate() {
        let mut label = candidate.label.clone();
        if label.trim().is_empty() {
            label = format!("{}", index + 1);
        }
        if !candidate.text.is_empty() {
            label.push(' ');
            label.push_str(&candidate.text);
        }

        let text_units = label
            .chars()
            .map(|ch| if ch.is_ascii() { 1 } else { 2 })
            .sum::<i32>();
        let rect_w = (text_units * popup.candidate_unit_width + popup.candidate_extra_width)
            .clamp(popup.candidate_min_width, popup.candidate_max_width);
        if x + rect_w > width as i32 - popup.padding_x {
            break;
        }

        let rect = RectPx {
            x,
            y: row_y,
            w: rect_w,
            h: row_h,
        };
        let highlighted = status.highlighted_candidate_index == Some(index);
        if highlighted {
            fill_rect(
                buf,
                width,
                height,
                rect,
                popup.highlight_background_color.bgra(),
            );
        } else if index == 0 {
            fill_rect(
                buf,
                width,
                height,
                rect,
                popup.first_candidate_background_color.bgra(),
            );
        }

        renderer.draw_text(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + 8,
                y: rect.y + 4,
                w: rect.w - 16,
                h: rect.h - 8,
            },
            &label,
            popup.candidate_font_size,
            popup.candidate_text_color.rgba(),
        );
        x += rect.w + gap;
    }
}

fn draw_rect_frame(buf: &mut [u8], width: u32, height: u32, rect: RectPx, color: [u8; 4]) {
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: 2,
        },
        color,
    );
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x,
            y: rect.y + rect.h - 2,
            w: rect.w,
            h: 2,
        },
        color,
    );
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x,
            y: rect.y,
            w: 2,
            h: rect.h,
        },
        color,
    );
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x + rect.w - 2,
            y: rect.y,
            w: 2,
            h: rect.h,
        },
        color,
    );
}

fn fill_rect(buf: &mut [u8], width: u32, height: u32, rect: RectPx, color: [u8; 4]) {
    let x0 = rect.x.max(0).min(width as i32) as u32;
    let y0 = rect.y.max(0).min(height as i32) as u32;
    let x1 = (rect.x + rect.w).max(0).min(width as i32) as u32;
    let y1 = (rect.y + rect.h).max(0).min(height as i32) as u32;

    for y in y0..y1 {
        for x in x0..x1 {
            let index = ((y * width + x) * 4) as usize;
            buf[index..index + 4].copy_from_slice(&color);
        }
    }
}

fn blend_text_rect(buf: &mut [u8], width: u32, height: u32, rect: RectPx, color: Color) {
    let x0 = rect.x.max(0).min(width as i32) as u32;
    let y0 = rect.y.max(0).min(height as i32) as u32;
    let x1 = (rect.x + rect.w).max(0).min(width as i32) as u32;
    let y1 = (rect.y + rect.h).max(0).min(height as i32) as u32;
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    let [src_r, src_g, src_b, src_a] = color.as_rgba();
    if src_a == 0 {
        return;
    }

    for y in y0..y1 {
        for x in x0..x1 {
            let index = ((y * width + x) * 4) as usize;
            let dst_b = buf[index];
            let dst_g = buf[index + 1];
            let dst_r = buf[index + 2];
            let dst_a = buf[index + 3];
            let out_a = src_a as u16 + ((dst_a as u16 * (255 - src_a as u16)) / 255);

            buf[index] = alpha_over(src_b, src_a, dst_b);
            buf[index + 1] = alpha_over(src_g, src_a, dst_g);
            buf[index + 2] = alpha_over(src_r, src_a, dst_r);
            buf[index + 3] = out_a.min(255) as u8;
        }
    }
}

fn alpha_over(src: u8, src_a: u8, dst: u8) -> u8 {
    let src = src as u16;
    let src_a = src_a as u16;
    let dst = dst as u16;
    ((src * src_a + dst * (255 - src_a)) / 255).min(255) as u8
}

fn env_u32(name: &str, default: u32) -> u32 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
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

