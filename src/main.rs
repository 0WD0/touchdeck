use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache};
use memmap2::MmapMut;
use tempfile::tempfile;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_region, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
    wl_touch,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use touchdeck::niri;
use touchdeck::protocol::{ImeCandidate, ImeCursorRect, ImeStatus};

mod action;
mod config;
mod engine;
mod geometry;
mod gesture;
mod key;
mod keymap;
mod layout;
mod mode;
mod niri_backend;


use action::*;
use config::*;
use engine::*;
use geometry::*;
use key::*;
use keymap::*;
use layout::*;
use mode::*;
use niri_backend::*;

const NAMESPACE: &str = "touchdeck";

struct App {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    virtual_keyboard_manager: Option<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1>,
    virtual_keyboard: Option<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1>,
    virtual_keyboard_keymap: Option<File>,
    seat: Option<wl_seat::WlSeat>,
    touch: Option<wl_touch::WlTouch>,
    surface: Option<wl_surface::WlSurface>,
    layer_surface: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    buffers: VecDeque<BufferBacking>,
    config: Config,
    width: u32,
    height: u32,
    engine: Engine,
    capture_policy: CapturePolicy,
    trace: Option<TraceRecorder>,
    started_at: Option<Instant>,
    mode_hint: Option<ModeHint>,
    last_presented_mode: Mode,
    ime_status: ImeStatus,
    ime_status_dirty: bool,
    ime_status_rx: Option<Receiver<ImeStatus>>,
    text_renderer: TextRenderer,
    modifier_mask: u32,
    held_modifier_mask: u32,
    modifier_mask_stack: Vec<u32>,
    last_key_sequence: Option<LastKeySequence>,
    active_actions: HashMap<i32, PressedAction>,
    running: bool,
}

impl Default for App {
    fn default() -> Self {
        let config = Config::default();
        let engine = Engine::default();
        let capture_policy = engine.capture_policy(&config);
        Self {
            compositor: None,
            shm: None,
            layer_shell: None,
            virtual_keyboard_manager: None,
            virtual_keyboard: None,
            virtual_keyboard_keymap: None,
            seat: None,
            touch: None,
            surface: None,
            layer_surface: None,
            buffers: VecDeque::new(),
            config,
            width: 0,
            height: 0,
            capture_policy,
            engine,
            trace: None,
            started_at: None,
            mode_hint: None,
            last_presented_mode: Mode::Base,
            ime_status: ImeStatus::default(),
            ime_status_dirty: false,
            ime_status_rx: None,
            text_renderer: TextRenderer::new(),
            modifier_mask: 0,
            held_modifier_mask: 0,
            modifier_mask_stack: Vec::new(),
            last_key_sequence: None,
            active_actions: HashMap::new(),
            running: false,
        }
    }
}

struct BufferBacking {
    _file: File,
    _mmap: MmapMut,
    _pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
    released: bool,
}

#[derive(Clone, Copy, Debug)]
struct ModeHint {
    mode: Mode,
    until_ms: u64,
}

struct TextRenderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

impl TextRenderer {
    fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        }
    }

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

fn spawn_ime_status_subscriber(socket_path: PathBuf) -> Receiver<ImeStatus> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || loop {
        match UnixStream::connect(&socket_path) {
            Ok(mut stream) => {
                let message = serde_json::json!({
                    "protocol": "touchdeck-ime-v1",
                    "type": "subscribe_status",
                    "source": "touchdeck",
                });

                if let Err(err) = serde_json::to_writer(&mut stream, &message)
                    .and_then(|()| stream.write_all(b"\n").map_err(serde_json::Error::io))
                {
                    eprintln!("touchdeck: failed to subscribe touchdeck-ime status: {err}");
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }

                eprintln!(
                    "touchdeck: subscribed to touchdeck-ime status at {}",
                    socket_path.display()
                );
                let reader = BufReader::new(stream);
                for line in reader.lines() {
                    let Ok(line) = line else {
                        break;
                    };
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<ImeStatus>(line.trim()) {
                        Ok(status) => {
                            if tx.send(status).is_err() {
                                return;
                            }
                        }
                        Err(err) => {
                            eprintln!("touchdeck: failed to parse subscribed IME status: {err}");
                        }
                    }
                }
            }
            Err(err) => {
                eprintln!(
                    "touchdeck: waiting for touchdeck-ime status socket {}: {err}",
                    socket_path.display()
                );
            }
        }

        thread::sleep(Duration::from_millis(500));
    });
    rx
}

#[derive(Debug)]
enum PressedAction {
    None,
    Key(u32),
    ModMorph {
        masked_mods: u32,
        pressed: Box<PressedAction>,
    },
}

struct TraceRecorder {
    file: File,
}

impl TraceRecorder {
    fn new(path: &PathBuf) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .with_context(|| format!("open trace file {}", path.display()))?;
        Ok(Self { file })
    }

    fn record(&mut self, event: &TraceEvent) -> Result<()> {
        serde_json::to_writer(&mut self.file, event).context("write trace event")?;
        self.file.write_all(b"\n").context("write trace newline")?;
        Ok(())
    }
}

fn main() -> Result<()> {
    let config = Config::default();
    let trace = if let Some(path) = &config.record_trace_path {
        Some(TraceRecorder::new(path)?)
    } else {
        None
    };

    let conn = Connection::connect_to_env().context("connect to Wayland display")?;
    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();

    let mut app = App {
        config,
        trace,
        started_at: Some(Instant::now()),
        running: true,
        ..Default::default()
    };
    if app.config.text_output.backend.uses_ime() {
        app.ime_status_rx = Some(spawn_ime_status_subscriber(
            app.config.text_output.ime_socket.clone(),
        ));
    }

    display.get_registry(&qh, ());
    event_queue
        .roundtrip(&mut app)
        .context("collect Wayland globals")?;

    app.init_overlay(&qh)?;
    eprintln!(
        "touchdeck: overlay initialized; base mode captures fullscreen; double-tap bottom edge for passthrough"
    );

    while app.running {
        event_queue
            .dispatch_pending(&mut app)
            .context("dispatch pending Wayland events")?;
        app.drain_ime_status(&qh)
            .context("drain touchdeck-ime status")?;

        let now_ms = app.now_ms();
        let size = app.surface_size();
        let config = app.config.clone();
        let effects = app.engine.process_timers(now_ms, &config, size);
        app.apply_effects_or_stop(&qh, effects);
        app.expire_mode_hint(&qh)
            .context("expire mode hint overlay")?;

        if !app.running {
            break;
        }

        event_queue.flush().context("flush Wayland requests")?;
        let timeout = app.poll_timeout();
        let wayland_fd = event_queue.as_fd().as_raw_fd();

        let Some(guard) = event_queue.prepare_read() else {
            continue;
        };

        event_queue.flush().context("flush Wayland requests")?;
        if poll_fd(wayland_fd, timeout).context("poll Wayland fd")? {
            guard.read().context("read Wayland events")?;
        }
    }

    Ok(())
}

impl App {
    fn init_overlay(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let compositor = self
            .compositor
            .as_ref()
            .ok_or_else(|| anyhow!("Wayland compositor global is unavailable"))?;
        let layer_shell = self
            .layer_shell
            .as_ref()
            .ok_or_else(|| anyhow!("zwlr_layer_shell_v1 global is unavailable"))?;
        self.touch
            .as_ref()
            .ok_or_else(|| anyhow!("wl_touch is unavailable on this Wayland seat"))?;

        let surface = compositor.create_surface(qh, ());
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Overlay,
            String::from(NAMESPACE),
            qh,
            (),
        );

        layer_surface.set_anchor(
            zwlr_layer_surface_v1::Anchor::Top
                | zwlr_layer_surface_v1::Anchor::Bottom
                | zwlr_layer_surface_v1::Anchor::Left
                | zwlr_layer_surface_v1::Anchor::Right,
        );
        layer_surface.set_size(0, 0);
        layer_surface
            .set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);

        surface.commit();

        self.surface = Some(surface);
        self.layer_surface = Some(layer_surface);

        self.init_virtual_keyboard(qh)?;

        Ok(())
    }

    fn init_virtual_keyboard(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let Some(manager) = self.virtual_keyboard_manager.as_ref() else {
            eprintln!("touchdeck: zwp_virtual_keyboard_manager_v1 is unavailable; key actions will be ignored");
            return Ok(());
        };
        let seat = self
            .seat
            .as_ref()
            .ok_or_else(|| anyhow!("wl_seat is unavailable for virtual keyboard"))?;

        let keyboard = manager.create_virtual_keyboard(seat, qh, ());
        let keymap_bytes = load_xkb_keymap(&self.config)?;
        let mut file = tempfile().context("create virtual keyboard keymap file")?;
        file.write_all(&keymap_bytes)
            .context("write virtual keyboard keymap")?;
        file.flush().context("flush virtual keyboard keymap")?;
        keyboard.keymap(1, file.as_fd(), keymap_bytes.len() as u32);
        keyboard.modifiers(0, 0, 0, 0);
        self.modifier_mask = 0;
        self.held_modifier_mask = 0;
        self.modifier_mask_stack.clear();
        self.active_actions.clear();
        self.last_key_sequence = None;

        self.virtual_keyboard = Some(keyboard);
        self.virtual_keyboard_keymap = Some(file);
        eprintln!("touchdeck: virtual keyboard initialized");

        Ok(())
    }

    fn surface_size(&self) -> SurfaceSize {
        SurfaceSize {
            width: self.width.max(1),
            height: self.height.max(1),
        }
    }

    fn now_ms(&self) -> u64 {
        self.started_at
            .map(|started_at| started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0)
    }

    fn poll_timeout(&self) -> Option<Duration> {
        let mut deadline = self.engine.next_timer_deadline_ms();
        if let Some(hint) = self.mode_hint {
            deadline = Some(deadline.map_or(hint.until_ms, |deadline| deadline.min(hint.until_ms)));
        }
        if self.ime_status_rx.is_some() {
            let refresh_deadline = self.now_ms().saturating_add(33);
            deadline =
                Some(deadline.map_or(refresh_deadline, |deadline| deadline.min(refresh_deadline)));
        }

        deadline.map(|deadline_ms| {
            let now_ms = self.now_ms();
            Duration::from_millis(deadline_ms.saturating_sub(now_ms))
        })
    }

    fn expire_mode_hint(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let expired = self
            .mode_hint
            .is_some_and(|hint| self.now_ms() >= hint.until_ms);
        if !expired {
            return Ok(());
        }

        self.mode_hint = None;
        if self.width != 0 && self.height != 0 {
            self.attach_overlay_buffer(qh, self.width, self.height)?;
        }
        Ok(())
    }

    fn drain_ime_status(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let mut latest = None;
        if let Some(rx) = &self.ime_status_rx {
            while let Ok(status) = rx.try_recv() {
                if status.protocol == "touchdeck-ime-v1" && status.kind == "status" {
                    latest = Some(status);
                }
            }
        }

        let Some(status) = latest else {
            return Ok(());
        };

        if status == self.ime_status {
            return Ok(());
        }

        self.ime_status = status;
        if self.width != 0 && self.height != 0 {
            self.attach_overlay_buffer(qh, self.width, self.height)?;
        }
        Ok(())
    }

    fn attach_overlay_buffer(
        &mut self,
        qh: &QueueHandle<Self>,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let width = width.max(1);
        let height = height.max(1);
        let stride = width
            .checked_mul(4)
            .ok_or_else(|| anyhow!("invalid buffer stride"))?;
        let size = stride
            .checked_mul(height)
            .ok_or_else(|| anyhow!("invalid buffer size"))?;

        let file = tempfile().context("create shm backing file")?;
        file.set_len(u64::from(size))
            .context("resize shm backing file")?;

        let mut mmap = unsafe { MmapMut::map_mut(&file).context("map shm backing file")? };
        self.render_overlay(&mut mmap, width, height);

        let shm = self
            .shm
            .as_ref()
            .ok_or_else(|| anyhow!("wl_shm global is unavailable"))?;
        let surface = self
            .surface
            .as_ref()
            .ok_or_else(|| anyhow!("overlay surface is not initialized"))?;

        let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            qh,
            (),
        );

        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width as i32, height as i32);
        surface.commit();

        self.buffers.retain(|backing| !backing.released);
        self.buffers.push_back(BufferBacking {
            _file: file,
            _mmap: mmap,
            _pool: pool,
            buffer,
            released: false,
        });

        Ok(())
    }

    fn render_overlay(&mut self, mmap: &mut [u8], width: u32, height: u32) {
        mmap.fill(0);

        let size = SurfaceSize { width, height };
        if self.config.debug_alpha != 0 && !self.config.debug_draw {
            fill_rect(
                mmap,
                width,
                height,
                RectPx {
                    x: 0,
                    y: 0,
                    w: width as i32,
                    h: height as i32,
                },
                [0x00, 0x80, 0xff, self.config.debug_alpha],
            );
            self.render_mode_hint(mmap, width, height);
            return;
        }

        if !self.config.debug_draw {
            if self.engine.mode == Mode::Text {
                self.render_text_keyboard(mmap, width, height, size);
            }
            if self.should_render_ime_status() {
                self.render_ime_status(mmap, width, height);
            }
            self.render_mode_hint(mmap, width, height);
            return;
        }

        match self.engine.mode {
            Mode::Base => {}
            Mode::Text => fill_rect(
                mmap,
                width,
                height,
                RectPx {
                    x: 0,
                    y: 0,
                    w: width as i32,
                    h: height as i32,
                },
                [0x10, 0x90, 0x70, 0x30],
            ),
            Mode::Passthrough => {}
            Mode::NiriMomentary => fill_rect(
                mmap,
                width,
                height,
                RectPx {
                    x: 0,
                    y: 0,
                    w: width as i32,
                    h: height as i32,
                },
                [0x90, 0x40, 0x10, 0x30],
            ),
            Mode::NiriLocked => fill_rect(
                mmap,
                width,
                height,
                RectPx {
                    x: 0,
                    y: 0,
                    w: width as i32,
                    h: height as i32,
                },
                [0x10, 0x50, 0xe0, 0x38],
            ),
        }

        for slot in self.config.slots.slots() {
            let rect = slot.rect.to_px(size);
            let color = slot_debug_color(slot);
            if slot.capture || slot.role == SlotRole::Key {
                fill_rect(mmap, width, height, rect, color);
            } else {
                draw_rect_frame(mmap, width, height, rect, color);
            }

            let label = self
                .config
                .keymap
                .slot_label(self.engine.mode, &self.engine.layer_stack, &slot.id)
                .or_else(|| slot.label.clone());
            if let Some(label) = label {
                let mut label_mark = rect;
                label_mark.h = label_mark.h.min(8);
                if slot.capture || slot.role == SlotRole::Key {
                    fill_rect(mmap, width, height, label_mark, [0xff, 0xff, 0xff, 0x70]);
                }
                draw_label_in_rect(mmap, width, height, rect, &label, [0xff, 0xff, 0xff, 0xd0]);
            }
        }

        for binding in self.config.keymap.bindings.iter().filter(|binding| {
            binding.mode == self.engine.mode && self.engine.layer_stack.contains(&binding.layer)
        }) {
            let target = binding.trigger.target();
            let rect = binding.trigger.rect().to_px(size);
            let color = active_binding_debug_color(target);
            if target.capture {
                fill_rect(mmap, width, height, rect, color);
            } else {
                draw_rect_frame(mmap, width, height, rect, color);
            }
        }

        if let Some(candidate) = &self.engine.hold_candidate {
            if let Some(contact) = self.engine.active.get(&candidate.id) {
                draw_circle(
                    mmap,
                    width,
                    height,
                    contact.last_x.round() as i32,
                    contact.last_y.round() as i32,
                    42,
                    [0x00, 0xc0, 0xff, 0xb0],
                );
            }
        }

        for contact in self.engine.active.values() {
            draw_circle(
                mmap,
                width,
                height,
                contact.last_x.round() as i32,
                contact.last_y.round() as i32,
                24,
                [0xff, 0xff, 0xff, 0xd0],
            );
        }

        if self.should_render_ime_status() {
            self.render_ime_status(mmap, width, height);
        }

        self.render_mode_hint(mmap, width, height);
    }

    fn should_render_ime_status(&self) -> bool {
        if self.ime_status.preedit.is_empty()
            && self.ime_status.commit_preview.is_empty()
            && self.ime_status.candidates.is_empty()
        {
            return false;
        }

        (self.engine.mode == Mode::Text && self.ime_status.ui_owner == "touchdeck-overlay")
            || self.should_render_fcitx_server_popup()
    }

    fn should_render_fcitx_server_popup(&self) -> bool {
        self.ime_status.ui_owner == "touchdeck-server-popup"
            && self.ime_status.cursor_rect.is_some()
    }

    fn render_mode_hint(&self, mmap: &mut [u8], width: u32, height: u32) {
        let Some(hint) = self.mode_hint else {
            return;
        };
        if self.now_ms() >= hint.until_ms {
            return;
        }

        let max_w = (width as i32 - 32).max(80);
        let toast_w = ((width as i32 * 52) / 100).clamp(80, max_w);
        let toast_h = ((height as i32 * 45) / 1000)
            .clamp(40, 90)
            .min(height as i32);
        let x = (width as i32 - toast_w) / 2;
        let y = ((height as i32 * 70) / 1000)
            .max(16)
            .min((height as i32 - toast_h).max(0));
        let rect = RectPx {
            x,
            y,
            w: toast_w,
            h: toast_h,
        };
        let color = mode_hint_color(hint.mode);

        fill_rect(mmap, width, height, rect, [0x08, 0x12, 0x18, 0xd8]);
        draw_rect_frame(mmap, width, height, rect, color);
        fill_rect(
            mmap,
            width,
            height,
            RectPx {
                x: rect.x,
                y: rect.y + rect.h - 6,
                w: rect.w,
                h: 6,
            },
            color,
        );
        draw_label_in_rect_limited(
            mmap,
            width,
            height,
            rect,
            mode_hint_label(hint.mode),
            [0xff, 0xff, 0xff, 0xf0],
            8,
        );
    }

    fn render_text_keyboard(&self, mmap: &mut [u8], width: u32, height: u32, size: SurfaceSize) {
        fill_rect(
            mmap,
            width,
            height,
            RectPx {
                x: 0,
                y: 0,
                w: width as i32,
                h: height as i32,
            },
            [0x08, 0x18, 0x14, 0x20],
        );

        for slot in self.config.slots.slots() {
            if slot.role != SlotRole::Key {
                continue;
            }

            let tap_label = self.config.keymap.slot_gesture_label(
                Mode::Text,
                &self.engine.layer_stack,
                &slot.id,
                SlotGestureKind::Tap,
            );
            let hold_label = self.config.keymap.slot_gesture_label(
                Mode::Text,
                &self.engine.layer_stack,
                &slot.id,
                SlotGestureKind::Hold,
            );
            let up_label = self.config.keymap.slot_gesture_label(
                Mode::Text,
                &self.engine.layer_stack,
                &slot.id,
                SlotGestureKind::SwipeUp,
            );
            let down_label = self.config.keymap.slot_gesture_label(
                Mode::Text,
                &self.engine.layer_stack,
                &slot.id,
                SlotGestureKind::SwipeDown,
            );
            let left_label = self.config.keymap.slot_gesture_label(
                Mode::Text,
                &self.engine.layer_stack,
                &slot.id,
                SlotGestureKind::SwipeLeft,
            );
            let right_label = self.config.keymap.slot_gesture_label(
                Mode::Text,
                &self.engine.layer_stack,
                &slot.id,
                SlotGestureKind::SwipeRight,
            );

            if tap_label.is_none()
                && hold_label.is_none()
                && up_label.is_none()
                && down_label.is_none()
                && left_label.is_none()
                && right_label.is_none()
            {
                continue;
            }

            let rect = slot.rect.to_px(size);
            fill_rect(mmap, width, height, rect, [0x12, 0x34, 0x2a, 0xa8]);
            draw_rect_frame(mmap, width, height, rect, [0x80, 0xff, 0xc8, 0xb0]);
            draw_keycap_labels(
                mmap,
                width,
                height,
                rect,
                tap_label.as_deref(),
                hold_label.as_deref(),
                up_label.as_deref(),
                down_label.as_deref(),
                left_label.as_deref(),
                right_label.as_deref(),
            );
        }
    }

    fn render_ime_status(&mut self, mmap: &mut [u8], width: u32, height: u32) {
        if self.ime_status.preedit.is_empty()
            && self.ime_status.commit_preview.is_empty()
            && self.ime_status.candidates.is_empty()
        {
            return;
        }

        if self.should_render_fcitx_server_popup() {
            if let Some(cursor_rect) = self.ime_status.cursor_rect.clone() {
                self.render_physical_ime_status(mmap, width, height, cursor_rect);
                return;
            }
        }

        if self.ime_status.ui_owner != "touchdeck-overlay" {
            return;
        }

        let panel_x = ((width as i32 * 3) / 100).max(4);
        let panel_w = (width as i32 - panel_x * 2).max(80);
        let panel_h = ((height as i32 * 78) / 1000).clamp(54, 118);
        let panel_y = ((height as i32 * 430) / 1000)
            .max(8)
            .min((height as i32 - panel_h - 8).max(0));
        let panel = RectPx {
            x: panel_x,
            y: panel_y,
            w: panel_w,
            h: panel_h,
        };

        fill_rect(mmap, width, height, panel, [0x08, 0x14, 0x18, 0xd8]);
        draw_rect_frame(mmap, width, height, panel, [0x40, 0xff, 0xd0, 0xc8]);

        let header_h = (panel.h * 42 / 100).max(22);
        let mut header = String::new();
        if !self.ime_status.preedit.is_empty() {
            header.push_str(&self.ime_status.preedit);
        }
        if !self.ime_status.commit_preview.is_empty() {
            if !header.is_empty() {
                header.push_str(" > ");
            }
            header.push_str(&self.ime_status.commit_preview);
        }
        if header.is_empty() {
            header.push_str("IME");
        }
        self.text_renderer.draw_text(
            mmap,
            width,
            height,
            RectPx {
                x: panel.x + 8,
                y: panel.y + 4,
                w: panel.w - 16,
                h: header_h - 6,
            },
            &header,
            (header_h as f32 * 0.55).clamp(14.0, 34.0),
            [0xff, 0xff, 0xff, 0xe8],
        );

        let row_y = panel.y + header_h;
        let row_h = (panel.h - header_h - 8).max(18);
        let visible = self
            .ime_status
            .candidates
            .iter()
            .take(5)
            .collect::<Vec<_>>();
        if visible.is_empty() {
            return;
        }

        let gap = 6;
        let box_w =
            ((panel.w - 16 - gap * (visible.len() as i32 - 1)) / visible.len() as i32).max(24);
        for (index, candidate) in visible.into_iter().enumerate() {
            let rect = RectPx {
                x: panel.x + 8 + index as i32 * (box_w + gap),
                y: row_y,
                w: box_w,
                h: row_h,
            };
            let highlighted = self.ime_status.highlighted_candidate_index == Some(index);
            let fill = if highlighted {
                [0x10, 0xff, 0xb0, 0xa0]
            } else {
                [0x18, 0x38, 0x34, 0xb8]
            };
            fill_rect(mmap, width, height, rect, fill);
            draw_rect_frame(mmap, width, height, rect, [0xb0, 0xff, 0xe0, 0xb0]);

            let mut label = candidate.label.clone();
            if !candidate.text.is_empty() {
                label.push(' ');
                label.push_str(&candidate.text);
            }
            if label.trim().is_empty() {
                label = format!("{}", index + 1);
            }
            self.text_renderer.draw_text(
                mmap,
                width,
                height,
                rect,
                &label,
                (rect.h as f32 * 0.45).clamp(13.0, 28.0),
                [0xff, 0xff, 0xff, 0xf0],
            );
        }
    }

    fn render_physical_ime_status(
        &mut self,
        mmap: &mut [u8],
        width: u32,
        height: u32,
        cursor_rect: ImeCursorRect,
    ) {
        let screen_w = width as i32;
        let screen_h = height as i32;
        if screen_w <= 0 || screen_h <= 0 {
            return;
        }

        let candidate_count = self.ime_status.candidates.iter().take(6).count();
        let panel_w = 560.min(screen_w - 16).max(220);
        let panel_h = if candidate_count == 0 { 48 } else { 88 }
            .min(screen_h - 16)
            .max(44);

        let Some((cursor_x, cursor_y, cursor_h)) =
            self.physical_ime_anchor(cursor_rect, screen_w, screen_h)
        else {
            return;
        };

        let panel_x = cursor_x.clamp(0, (screen_w - panel_w).max(0));
        let below_y = cursor_y;
        let above_y = cursor_y - panel_h;
        let panel_y = if below_y + panel_h <= screen_h {
            below_y
        } else {
            above_y.max(0)
        };

        let panel = RectPx {
            x: panel_x,
            y: panel_y,
            w: panel_w,
            h: panel_h,
        };

        if std::env::var_os("TOUCHDECK_LOG_IME_GEOMETRY").is_some() {
            eprintln!(
                "touchdeck: ime geometry panel anchor=({}, {} h={}) panel=({}, {} {}x{}) screen={}x{}",
                cursor_x,
                cursor_y,
                cursor_h,
                panel.x,
                panel.y,
                panel.w,
                panel.h,
                screen_w,
                screen_h
            );
        }

        fill_rect(mmap, width, height, panel, [0x1a, 0x22, 0x26, 0xe6]);
        draw_rect_frame(mmap, width, height, panel, [0x79, 0x8b, 0x86, 0x96]);
        if std::env::var_os("TOUCHDECK_LOG_IME_GEOMETRY").is_some() {
            fill_rect(
                mmap,
                width,
                height,
                RectPx {
                    x: cursor_x - 14,
                    y: cursor_y - 1,
                    w: 29,
                    h: 3,
                },
                [0x20, 0x40, 0xff, 0xf0],
            );
            fill_rect(
                mmap,
                width,
                height,
                RectPx {
                    x: cursor_x - 1,
                    y: cursor_y - 14,
                    w: 3,
                    h: 29,
                },
                [0x20, 0x40, 0xff, 0xf0],
            );
            draw_rect_frame(
                mmap,
                width,
                height,
                RectPx {
                    x: cursor_x - 6,
                    y: cursor_y - 6,
                    w: 13,
                    h: 13,
                },
                [0xff, 0xff, 0xff, 0xf0],
            );
        }

        let header_h = if candidate_count == 0 {
            panel.h
        } else {
            (panel.h * 40 / 100).max(30)
        };
        let header = self.ime_header_text();
        self.text_renderer.draw_text(
            mmap,
            width,
            height,
            RectPx {
                x: panel.x + 12,
                y: panel.y + 4,
                w: panel.w - 24,
                h: header_h - 6,
            },
            &header,
            (header_h as f32 * 0.52).clamp(15.0, 30.0),
            [0xd8, 0xde, 0xe8, 0xee],
        );

        let visible = self
            .ime_status
            .candidates
            .iter()
            .take(6)
            .collect::<Vec<_>>();
        if visible.is_empty() {
            return;
        }

        fill_rect(
            mmap,
            width,
            height,
            RectPx {
                x: panel.x + 10,
                y: panel.y + header_h,
                w: panel.w - 20,
                h: 1,
            },
            [0x6c, 0x78, 0x72, 0x70],
        );

        let row_y = panel.y + header_h + 7;
        let row_h = panel.h - header_h - 14;
        let gap = 6;
        let mut x = panel.x + 10;
        let right = panel.x + panel.w - 10;
        for (index, candidate) in visible.into_iter().enumerate() {
            let label = Self::ime_candidate_label(index, candidate);
            let text_units = label
                .chars()
                .map(|ch| if ch.is_ascii() { 1 } else { 2 })
                .sum::<i32>();
            let rect_w = (text_units * 8 + 26).clamp(48, 154);
            if x + rect_w > right {
                break;
            }

            let rect = RectPx {
                x,
                y: row_y,
                w: rect_w,
                h: row_h,
            };
            let highlighted = self.ime_status.highlighted_candidate_index == Some(index);
            let fill = if highlighted {
                [0x3b, 0x86, 0xf2, 0xdc]
            } else if index == 0 {
                [0x2e, 0x3d, 0x44, 0x70]
            } else {
                [0x00, 0x00, 0x00, 0x00]
            };
            fill_rect(mmap, width, height, rect, fill);
            self.text_renderer.draw_text(
                mmap,
                width,
                height,
                RectPx {
                    x: rect.x + 8,
                    y: rect.y + 3,
                    w: rect.w - 16,
                    h: rect.h - 6,
                },
                &label,
                (rect.h as f32 * 0.48).clamp(14.0, 24.0),
                [0xff, 0xff, 0xff, 0xf0],
            );

            x += rect.w + gap;
        }
    }

    fn physical_ime_anchor(
        &self,
        cursor_rect: ImeCursorRect,
        screen_w: i32,
        screen_h: i32,
    ) -> Option<(i32, i32, i32)> {
        if cursor_rect.space == "x11-root" {
            return self.x11_ime_anchor(cursor_rect, screen_w, screen_h);
        }

        let scale = if cursor_rect.scale.is_finite() && cursor_rect.scale > 0.0 {
            cursor_rect.scale
        } else {
            1.0
        };
        Some((
            ((cursor_rect.x as f64) / scale)
                .round()
                .clamp(0.0, screen_w.saturating_sub(1) as f64) as i32,
            ((cursor_rect.y as f64) / scale)
                .round()
                .clamp(0.0, screen_h.saturating_sub(1) as f64) as i32,
            ((cursor_rect.h.max(0) as f64) / scale).round() as i32,
        ))
    }

    fn x11_ime_anchor(
        &self,
        cursor_rect: ImeCursorRect,
        screen_w: i32,
        screen_h: i32,
    ) -> Option<(i32, i32, i32)> {
        let (Some(window_x), Some(window_y), Some(window_w), Some(window_h)) = (
            cursor_rect.window_x,
            cursor_rect.window_y,
            cursor_rect.window_w,
            cursor_rect.window_h,
        ) else {
            if std::env::var_os("TOUCHDECK_LOG_IME_GEOMETRY").is_some() {
                eprintln!(
                    "touchdeck: ime geometry x11-root missing window geometry raw=({}, {} {}x{}) root={:?}x{:?}",
                    cursor_rect.x,
                    cursor_rect.y,
                    cursor_rect.w,
                    cursor_rect.h,
                    cursor_rect.root_w,
                    cursor_rect.root_h
                );
            }
            return None;
        };
        if window_w <= 0 || window_h <= 0 {
            if std::env::var_os("TOUCHDECK_LOG_IME_GEOMETRY").is_some() {
                eprintln!(
                    "touchdeck: ime geometry x11-root invalid window geometry window=({}, {} {}x{}) raw=({}, {} {}x{})",
                    window_x,
                    window_y,
                    window_w,
                    window_h,
                    cursor_rect.x,
                    cursor_rect.y,
                    cursor_rect.w,
                    cursor_rect.h
                );
            }
            return None;
        }

        let layout = match niri::focused_window_layout() {
            Ok(Some(layout)) => layout,
            Ok(None) => {
                if std::env::var_os("TOUCHDECK_LOG_IME_GEOMETRY").is_some() {
                    eprintln!(
                        "touchdeck: ime geometry x11-root no focused niri window window=({}, {} {}x{}) raw=({}, {} {}x{})",
                        window_x,
                        window_y,
                        window_w,
                        window_h,
                        cursor_rect.x,
                        cursor_rect.y,
                        cursor_rect.w,
                        cursor_rect.h
                    );
                }
                return None;
            }
            Err(err) => {
                eprintln!("touchdeck: failed to query niri focused window for xwayland IME popup: {err:?}");
                return None;
            }
        };

        let (window_output_x, window_output_y, output_window_w, output_window_h) =
            layout.window_rect_in_output;
        let (workarea_x, workarea_y, workarea_w, workarea_h) = layout.working_area_in_output;
        let output_layout = match niri::focused_output_layout() {
            Ok(Some(output)) => output,
            Ok(None) => {
                if std::env::var_os("TOUCHDECK_LOG_IME_GEOMETRY").is_some() {
                    eprintln!(
                        "touchdeck: ime geometry x11-root no focused niri output window=({}, {} {}x{}) raw=({}, {} {}x{})",
                        window_x,
                        window_y,
                        window_w,
                        window_h,
                        cursor_rect.x,
                        cursor_rect.y,
                        cursor_rect.w,
                        cursor_rect.h
                    );
                }
                return None;
            }
            Err(err) => {
                eprintln!("touchdeck: failed to query niri focused output for xwayland IME popup: {err:?}");
                return None;
            }
        };
        let (mut source_output_w, mut source_output_h) = transformed_source_size(output_layout);
        source_output_w = source_output_w.max(workarea_x + workarea_w);
        source_output_h = source_output_h.max(workarea_y + workarea_h);
        let (workarea_overlay_x, workarea_overlay_y, workarea_overlay_w, workarea_overlay_h) =
            transform_rect_to_overlay(
                output_layout.transform,
                workarea_x,
                workarea_y,
                workarea_w,
                workarea_h,
                source_output_w,
                source_output_h,
            );
        let origin_x = window_output_x - workarea_overlay_x;
        let origin_y = window_output_y - workarea_overlay_y;
        let window_size_w = output_window_w as f64;
        let window_size_h = output_window_h as f64;
        if window_size_w <= 0.0 || window_size_h <= 0.0 {
            return None;
        }

        let scale_x = window_w as f64 / window_size_w;
        let scale_y = window_h as f64 / window_size_h;
        if !scale_x.is_finite() || !scale_y.is_finite() || scale_x <= 0.0 || scale_y <= 0.0 {
            if std::env::var_os("TOUCHDECK_LOG_IME_GEOMETRY").is_some() {
                eprintln!(
                    "touchdeck: ime geometry x11-root invalid scale raw=({}, {} {}x{}) x11_window=({}, {} {}x{}) niri_output={}x{} {:?} niri_window_rect=({:.2}, {:.2} {}x{}) niri_workarea=({:.2}, {:.2} {:.2}x{:.2}) workarea_overlay=({:.2}, {:.2} {:.2}x{:.2}) overlay_origin=({:.2}, {:.2}) scale=({:.4}, {:.4})",
                    cursor_rect.x,
                    cursor_rect.y,
                    cursor_rect.w,
                    cursor_rect.h,
                    window_x,
                    window_y,
                    window_w,
                    window_h,
                    output_layout.width,
                    output_layout.height,
                    output_layout.transform,
                    window_output_x,
                    window_output_y,
                    output_window_w,
                    output_window_h,
                    workarea_x,
                    workarea_y,
                    workarea_w,
                    workarea_h,
                    workarea_overlay_x,
                    workarea_overlay_y,
                    workarea_overlay_w,
                    workarea_overlay_h,
                    origin_x,
                    origin_y,
                    scale_x,
                    scale_y
                );
            }
            return None;
        }

        let local_x = (cursor_rect.x - window_x) as f64;
        let local_y = (cursor_rect.y - window_y) as f64;
        let cursor_x = (origin_x + local_x / scale_x)
            .round()
            .clamp(0.0, screen_w.saturating_sub(1) as f64) as i32;
        let cursor_y = (origin_y + local_y / scale_y)
            .round()
            .clamp(0.0, screen_h.saturating_sub(1) as f64) as i32;
        let cursor_h = ((cursor_rect.h.max(0) as f64) / scale_y).round() as i32;

        if std::env::var_os("TOUCHDECK_LOG_IME_GEOMETRY").is_some() {
            eprintln!(
                "touchdeck: ime geometry x11-root raw=({}, {} {}x{}) root={:?}x{:?} x11_window=({}, {} {}x{}) niri_output={}x{} {:?} niri_window_rect=({:.2}, {:.2} {}x{}) niri_workarea=({:.2}, {:.2} {:.2}x{:.2}) workarea_overlay=({:.2}, {:.2} {:.2}x{:.2}) overlay_origin=({:.2}, {:.2}) scale=({:.4}, {:.4}) local=({:.2}, {:.2}) anchor=({}, {} h={}) screen={}x{}",
                cursor_rect.x,
                cursor_rect.y,
                cursor_rect.w,
                cursor_rect.h,
                cursor_rect.root_w,
                cursor_rect.root_h,
                window_x,
                window_y,
                window_w,
                window_h,
                output_layout.width,
                output_layout.height,
                output_layout.transform,
                window_output_x,
                window_output_y,
                output_window_w,
                output_window_h,
                workarea_x,
                workarea_y,
                workarea_w,
                workarea_h,
                workarea_overlay_x,
                workarea_overlay_y,
                workarea_overlay_w,
                workarea_overlay_h,
                origin_x,
                origin_y,
                scale_x,
                scale_y,
                local_x,
                local_y,
                cursor_x,
                cursor_y,
                cursor_h,
                screen_w,
                screen_h
            );
        }

        Some((cursor_x, cursor_y, cursor_h))
    }
}

impl App {
    fn ime_header_text(&self) -> String {
        let mut header = String::new();
        if !self.ime_status.preedit.is_empty() {
            header.push_str(&self.ime_status.preedit);
        }
        if !self.ime_status.commit_preview.is_empty() {
            if !header.is_empty() {
                header.push_str(" > ");
            }
            header.push_str(&self.ime_status.commit_preview);
        }
        if header.is_empty() {
            header.push_str("IME");
        }
        header
    }

    fn ime_candidate_label(index: usize, candidate: &ImeCandidate) -> String {
        let mut label = candidate.label.clone();
        if !candidate.text.is_empty() {
            label.push(' ');
            label.push_str(&candidate.text);
        }
        if label.trim().is_empty() {
            label = format!("{}", index + 1);
        }
        label
    }

    fn apply_input_region(&self, qh: &QueueHandle<Self>, policy: &CapturePolicy) -> Result<()> {
        let surface = self
            .surface
            .as_ref()
            .ok_or_else(|| anyhow!("overlay surface is not initialized"))?;
        let compositor = self
            .compositor
            .as_ref()
            .ok_or_else(|| anyhow!("Wayland compositor global is unavailable"))?;
        let size = self.surface_size();

        match policy {
            CapturePolicy::Fullscreen => {
                surface.set_input_region(None);
            }
            CapturePolicy::Zones(rects) => {
                let region = compositor.create_region(qh, ());
                for rect in rects {
                    let rect = rect.to_px(size);
                    if rect.w > 0 && rect.h > 0 {
                        region.add(rect.x, rect.y, rect.w, rect.h);
                    }
                }
                surface.set_input_region(Some(&region));
                region.destroy();
            }
            CapturePolicy::None => {
                let region = compositor.create_region(qh, ());
                surface.set_input_region(Some(&region));
                region.destroy();
            }
        }

        surface.commit();
        Ok(())
    }

    fn apply_effects_or_stop(&mut self, qh: &QueueHandle<Self>, effects: Vec<EngineEffect>) {
        for effect in effects {
            let result = match effect {
                EngineEffect::SetCapture(policy) => {
                    self.capture_policy = policy.clone();
                    self.present_mode_hint_if_changed();
                    self.apply_input_region(qh, &policy)
                }
                EngineEffect::Dispatch(action) => {
                    self.dispatch_action(action);
                    self.redraw_ime_if_dirty(qh)
                }
                EngineEffect::Press { hold_id, action } => {
                    let pressed = self.press_action(action);
                    self.active_actions.insert(hold_id, pressed);
                    self.redraw_ime_if_dirty(qh)
                }
                EngineEffect::Release { hold_id } => {
                    if let Some(pressed) = self.active_actions.remove(&hold_id) {
                        self.release_pressed_action(pressed);
                    }
                    self.redraw_ime_if_dirty(qh)
                }
                EngineEffect::Redraw => {
                    if self.width != 0 && self.height != 0 {
                        self.attach_overlay_buffer(qh, self.width, self.height)
                    } else {
                        Ok(())
                    }
                }
            };

            if let Err(err) = result {
                eprintln!("touchdeck: failed to apply effect: {err:?}");
                self.running = false;
                break;
            }
        }
    }

    fn redraw_ime_if_dirty(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        if self.ime_status_dirty && self.width != 0 && self.height != 0 {
            self.ime_status_dirty = false;
            self.attach_overlay_buffer(qh, self.width, self.height)
        } else {
            Ok(())
        }
    }

    fn present_mode_hint_if_changed(&mut self) {
        let mode = self.engine.mode;
        if self.last_presented_mode == mode {
            return;
        }

        self.last_presented_mode = mode;
        if self.config.mode_hint_ms == 0 {
            self.mode_hint = None;
            return;
        }

        self.mode_hint = Some(ModeHint {
            mode,
            until_ms: self.now_ms() + u64::from(self.config.mode_hint_ms),
        });
    }

    fn dispatch_action(&mut self, action: GestureAction) {
        match action {
            GestureAction::Niri(action) => {
                self.engine.last_action = Some(action.as_str().to_string());
                eprintln!("touchdeck: niri action {action}");
                spawn_niri_action(action);
            }
            GestureAction::KeySequence(sequence) => {
                self.send_key_sequence(&sequence, None, None);
            }
            GestureAction::KeySequenceWithOptions {
                sequence,
                translation,
                route,
            } => {
                self.send_key_sequence(&sequence, translation, route);
            }
            GestureAction::ModMorph {
                mods,
                keep_mods,
                normal,
                morph,
            } => {
                let pressed = self.press_action(GestureAction::ModMorph {
                    mods,
                    keep_mods,
                    normal,
                    morph,
                });
                self.release_pressed_action(pressed);
            }
            GestureAction::KeyRepeat => {
                self.repeat_last_key_sequence();
            }
            GestureAction::HoldRepeat {
                sequence,
                translation,
                route,
                ..
            } => {
                self.send_key_sequence(&sequence, translation, route);
            }
            GestureAction::KeyHold(key) => {
                self.send_key_state(key, true);
            }
            GestureAction::Sequence(steps) => {
                self.run_action_steps(&steps);
            }
            GestureAction::Exit => {
                eprintln!("touchdeck: exit gesture");
                self.running = false;
            }
            GestureAction::ModeSet(_)
            | GestureAction::ModeToggle(_)
            | GestureAction::ModeMomentary(_)
            | GestureAction::LayerSet(_)
            | GestureAction::LayerToggle(_)
            | GestureAction::LayerMomentary(_) => {}
            GestureAction::None => {}
        }
    }

    fn press_action(&mut self, action: GestureAction) -> PressedAction {
        match action {
            GestureAction::KeyHold(key) => {
                self.send_key_state(key, true);
                if let Some(mask) = modifier_mask_for_key(key) {
                    self.held_modifier_mask |= mask;
                }
                PressedAction::Key(key)
            }
            GestureAction::ModMorph {
                mods,
                keep_mods,
                normal,
                morph,
            } => {
                if self.held_modifier_mask & mods == 0 {
                    self.press_action(*normal)
                } else {
                    let masked_mods = mods & !keep_mods;
                    self.push_modifier_mask(masked_mods);
                    let pressed = self.press_action(*morph);
                    PressedAction::ModMorph {
                        masked_mods,
                        pressed: Box::new(pressed),
                    }
                }
            }
            action => {
                self.dispatch_action(action);
                PressedAction::None
            }
        }
    }

    fn release_pressed_action(&mut self, pressed: PressedAction) {
        match pressed {
            PressedAction::None => {}
            PressedAction::Key(key) => {
                self.send_key_state(key, false);
                if let Some(mask) = modifier_mask_for_key(key) {
                    self.held_modifier_mask &= !mask;
                    self.restore_held_modifiers();
                }
            }
            PressedAction::ModMorph {
                masked_mods,
                pressed,
            } => {
                self.release_pressed_action(*pressed);
                self.pop_modifier_mask(masked_mods);
            }
        }
    }

    fn send_key(&mut self, key: u32) {
        let time = self.now_ms().min(u64::from(u32::MAX)) as u32;
        let release_time = time.saturating_add(self.key_tap_gap_ms(key));
        eprintln!("touchdeck: key {key}");
        self.emit_key_output(time, key, true, None, None);
        self.emit_key_output(release_time, key, false, None, None);
        self.restore_held_modifiers();
    }

    fn send_key_sequence(
        &mut self,
        sequence: &[KeyChord],
        translation: Option<KeyTranslationPolicy>,
        route: Option<KeyRoute>,
    ) {
        let mut time = self.now_ms().min(u64::from(u32::MAX)) as u32;
        eprintln!("touchdeck: key sequence {sequence:?}");
        if !sequence.is_empty() {
            self.last_key_sequence = Some(LastKeySequence {
                sequence: sequence.to_vec(),
                translation,
                route,
            });
        }
        for chord in sequence {
            for key in &chord.keys {
                self.emit_key_output(time, *key, true, translation, route);
                time = time.saturating_add(1);
            }
            if chord.keys.len() == 1 {
                time = time.saturating_add(self.key_tap_gap_ms(chord.keys[0]));
            }
            for key in chord.keys.iter().rev() {
                self.emit_key_output(time, *key, false, translation, route);
                time = time.saturating_add(1);
            }
        }
        self.restore_held_modifiers();
    }

    fn repeat_last_key_sequence(&mut self) {
        let Some(last) = self.last_key_sequence.clone() else {
            eprintln!("touchdeck: key_repeat ignored; no previous key sequence");
            return;
        };
        eprintln!("touchdeck: repeat last key sequence {:?}", last.sequence);
        self.send_key_sequence(&last.sequence, last.translation, last.route);
    }

    fn run_action_steps(&mut self, steps: &[ActionStep]) {
        for step in steps {
            match step {
                ActionStep::KeyDown(key) => self.send_key_state(*key, true),
                ActionStep::KeyUp(key) => self.send_key_state(*key, false),
                ActionStep::TapKey(key) => self.send_key(*key),
                ActionStep::KeySequence(sequence) => self.send_key_sequence(sequence, None, None),
                ActionStep::Niri(action) => spawn_niri_action(action.clone()),
                ActionStep::DelayMs(ms) => {
                    std::thread::sleep(Duration::from_millis(u64::from(*ms)))
                }
            }
        }
    }

    fn send_key_state(&mut self, key: u32, pressed: bool) {
        let time = self.now_ms().min(u64::from(u32::MAX)) as u32;
        self.emit_key_output(time, key, pressed, None, None);
    }

    fn emit_key_output(
        &mut self,
        time: u32,
        key: u32,
        pressed: bool,
        translation: Option<KeyTranslationPolicy>,
        route: Option<KeyRoute>,
    ) {
        let keyboard = if self.config.text_output.backend.uses_virtual_keyboard() {
            self.virtual_keyboard.clone()
        } else {
            None
        };

        if self.config.text_output.backend.uses_virtual_keyboard() && keyboard.is_none() {
            eprintln!("touchdeck: virtual keyboard unavailable; ignored key state {key}");
        }

        if let Some(keyboard) = &keyboard {
            keyboard.key(time, key, if pressed { 1 } else { 0 });
        }

        let Some(mask) = modifier_mask_for_key(key) else {
            self.emit_ime_key_state(time, key, pressed, translation, route);
            return;
        };

        if pressed {
            self.set_modifier_mask(self.modifier_mask | mask);
        } else {
            self.set_modifier_mask(self.modifier_mask & !mask);
        }
        self.emit_ime_key_state(time, key, pressed, translation, route);
    }

    fn set_modifier_mask(&mut self, modifier_mask: u32) {
        self.modifier_mask = modifier_mask;
        if self.config.text_output.backend.uses_virtual_keyboard() {
            if let Some(keyboard) = &self.virtual_keyboard {
                keyboard.modifiers(self.modifier_mask, 0, 0, 0);
            }
        }
    }

    fn push_modifier_mask(&mut self, masked_mods: u32) {
        if masked_mods == 0 {
            return;
        }
        self.modifier_mask_stack.push(masked_mods);
        self.restore_held_modifiers();
    }

    fn pop_modifier_mask(&mut self, masked_mods: u32) {
        if masked_mods == 0 {
            return;
        }
        if let Some(index) = self
            .modifier_mask_stack
            .iter()
            .rposition(|value| *value == masked_mods)
        {
            self.modifier_mask_stack.remove(index);
        }
        self.restore_held_modifiers();
    }

    fn restore_held_modifiers(&mut self) {
        let masked_mods = self
            .modifier_mask_stack
            .iter()
            .copied()
            .fold(0, |mask, value| mask | value);
        self.set_modifier_mask(self.held_modifier_mask & !masked_mods);
    }

    fn emit_ime_key_state(
        &mut self,
        time: u32,
        key: u32,
        pressed: bool,
        translation: Option<KeyTranslationPolicy>,
        route: Option<KeyRoute>,
    ) {
        if !self.config.text_output.backend.uses_ime() {
            return;
        }

        let message = serde_json::json!({
            "protocol": "touchdeck-ime-v1",
            "type": "key",
            "source": "touchdeck",
            "time": time,
            "key": key,
            "state": if pressed { "pressed" } else { "released" },
            "modifiers": self.modifier_mask,
            "translation": translation.map(KeyTranslationPolicy::as_str),
            "route": route.map(KeyRoute::as_str),
        });

        let mut stream = match UnixStream::connect(&self.config.text_output.ime_socket) {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!(
                    "touchdeck: failed to connect touchdeck-ime socket {}: {err}",
                    self.config.text_output.ime_socket.display()
                );
                return;
            }
        };

        if let Err(err) = serde_json::to_writer(&mut stream, &message)
            .and_then(|()| stream.write_all(b"\n").map_err(serde_json::Error::io))
        {
            eprintln!("touchdeck: failed to write touchdeck-ime event: {err}");
            return;
        }

        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {}
            Ok(_) => match serde_json::from_str::<ImeStatus>(line.trim()) {
                Ok(status) if status.protocol == "touchdeck-ime-v1" && status.kind == "status" => {
                    if status != self.ime_status {
                        self.ime_status = status;
                        self.ime_status_dirty = true;
                    }
                }
                Ok(status) => {
                    eprintln!("touchdeck: ignored unsupported touchdeck-ime status {status:?}");
                }
                Err(err) => {
                    eprintln!("touchdeck: failed to parse touchdeck-ime status: {err}");
                }
            },
            Err(err) => {
                eprintln!("touchdeck: failed to read touchdeck-ime status: {err}");
            }
        }
    }

    fn key_tap_gap_ms(&self, key: u32) -> u32 {
        if modifier_mask_for_key(key).is_some() {
            self.config.modifier_tap_ms.max(1)
        } else {
            1
        }
    }

    fn record_trace(&mut self, event: TraceEvent) {
        if let Some(trace) = self.trace.as_mut() {
            if let Err(err) = trace.record(&event) {
                eprintln!("touchdeck: failed to record trace: {err:?}");
                self.trace = None;
            }
        }
    }

    fn touch_down(&mut self, qh: &QueueHandle<Self>, id: i32, time: u32, x: f64, y: f64) {
        let now_ms = self.now_ms();
        self.record_trace(TraceEvent::Down {
            t: now_ms,
            wl_time: time,
            id,
            x,
            y,
        });
        if self.config.log_touch {
            eprintln!("touchdeck: touch down id={id} time={time} x={x:.1} y={y:.1}");
        }

        let config = self.config.clone();
        let size = self.surface_size();
        let effects = self
            .engine
            .handle_down(now_ms, time, id, x, y, &config, size);
        self.apply_effects_or_stop(qh, effects);
    }

    fn touch_motion(&mut self, qh: &QueueHandle<Self>, id: i32, time: u32, x: f64, y: f64) {
        let now_ms = self.now_ms();
        self.record_trace(TraceEvent::Motion {
            t: now_ms,
            wl_time: time,
            id,
            x,
            y,
        });
        if self.config.log_touch {
            eprintln!("touchdeck: touch motion id={id} time={time} x={x:.1} y={y:.1}");
        }

        let config = self.config.clone();
        let size = self.surface_size();
        let effects = self
            .engine
            .handle_motion(now_ms, id, time, x, y, &config, size);
        self.apply_effects_or_stop(qh, effects);
    }

    fn touch_up(&mut self, qh: &QueueHandle<Self>, id: i32, time: u32) {
        let now_ms = self.now_ms();
        self.record_trace(TraceEvent::Up {
            t: now_ms,
            wl_time: time,
            id,
        });
        if self.config.log_touch {
            eprintln!("touchdeck: touch up id={id} time={time}");
        }

        let config = self.config.clone();
        let size = self.surface_size();
        let effects = self.engine.handle_up(now_ms, time, id, &config, size);
        self.apply_effects_or_stop(qh, effects);
    }

    fn touch_cancel(&mut self, qh: &QueueHandle<Self>) {
        let now_ms = self.now_ms();
        self.record_trace(TraceEvent::Cancel { t: now_ms });
        if self.config.log_touch {
            eprintln!("touchdeck: touch cancel");
        }

        let config = self.config.clone();
        let effects = self.engine.handle_cancel(&config);
        self.apply_effects_or_stop(qh, effects);
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
        return Err(err.into());
    }
}

fn validate_rect(rect: RectNorm, context: &str) -> Result<RectNorm> {
    if !(rect.x0.is_finite()
        && rect.x1.is_finite()
        && rect.y0.is_finite()
        && rect.y1.is_finite()
        && rect.x0 >= 0.0
        && rect.y0 >= 0.0
        && rect.x1 <= 1.0
        && rect.y1 <= 1.0
        && rect.x0 < rect.x1
        && rect.y0 < rect.y1)
    {
        return Err(anyhow!(
            "{context} coordinates must be finite normalized ranges with x0 < x1 and y0 < y1"
        ));
    }

    Ok(rect)
}

fn slot_debug_color(slot: &Slot) -> [u8; 4] {
    match (slot.capture, slot.role) {
        (true, SlotRole::Key) => [0x10, 0xff, 0xb0, 0x38],
        (true, SlotRole::Zone) => [0x20, 0xff, 0x80, 0x50],
        (true, SlotRole::GestureArea) => [0xff, 0x90, 0x20, 0x44],
        (false, SlotRole::Key) => [0x80, 0x80, 0x80, 0x24],
        (false, SlotRole::Zone) => [0x80, 0x80, 0x80, 0x20],
        (false, SlotRole::GestureArea) => [0x60, 0x60, 0x60, 0x18],
    }
}

fn active_binding_debug_color(target: &SlotTarget) -> [u8; 4] {
    match (target.capture, target.role) {
        (true, SlotRole::Key) => [0x10, 0xff, 0xb0, 0x70],
        (true, SlotRole::Zone) => [0xe0, 0xff, 0x50, 0x70],
        (true, SlotRole::GestureArea) => [0xff, 0xb0, 0x20, 0x70],
        (false, _) => [0xff, 0xff, 0xff, 0x36],
    }
}

fn draw_rect_frame(buf: &mut [u8], width: u32, height: u32, rect: RectPx, color: [u8; 4]) {
    let thickness = 2.max((rect.w.min(rect.h) / 36).max(1));
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: thickness,
        },
        color,
    );
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x,
            y: rect.y + rect.h - thickness,
            w: rect.w,
            h: thickness,
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
            w: thickness,
            h: rect.h,
        },
        color,
    );
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x + rect.w - thickness,
            y: rect.y,
            w: thickness,
            h: rect.h,
        },
        color,
    );
}

fn draw_keycap_labels(
    buf: &mut [u8],
    width: u32,
    height: u32,
    rect: RectPx,
    tap: Option<&str>,
    hold: Option<&str>,
    up: Option<&str>,
    down: Option<&str>,
    left: Option<&str>,
    right: Option<&str>,
) {
    let hint_color = [0xa8, 0xff, 0xd8, 0xc8];
    let hold_color = [0xff, 0xe0, 0x90, 0xc8];
    let center_color = [0xff, 0xff, 0xff, 0xf0];
    let margin = (rect.w.min(rect.h) / 12).clamp(2, 10);
    let hint_h = (rect.h / 4).max(10);
    let side_w = (rect.w / 3).max(12);

    if let Some(label) = up {
        draw_label_in_rect_limited(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + margin,
                y: rect.y + margin,
                w: rect.w - margin * 2,
                h: hint_h,
            },
            label,
            hint_color,
            3,
        );
    }

    if let Some(label) = down {
        draw_label_in_rect_limited(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + margin,
                y: rect.y + rect.h - hint_h - margin,
                w: rect.w - margin * 2,
                h: hint_h,
            },
            label,
            hint_color,
            3,
        );
    }

    if let Some(label) = left {
        draw_label_in_rect_limited(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + margin,
                y: rect.y + rect.h / 3,
                w: side_w,
                h: rect.h / 3,
            },
            label,
            hint_color,
            3,
        );
    }

    if let Some(label) = right {
        draw_label_in_rect_limited(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + rect.w - side_w - margin,
                y: rect.y + rect.h / 3,
                w: side_w,
                h: rect.h / 3,
            },
            label,
            hint_color,
            3,
        );
    }

    if tap.is_some() {
        if let Some(label) = hold {
            draw_label_in_rect_limited(
                buf,
                width,
                height,
                RectPx {
                    x: rect.x + margin,
                    y: rect.y + rect.h - hint_h - margin,
                    w: side_w,
                    h: hint_h,
                },
                label,
                hold_color,
                2,
            );
        }
    }

    let center_label = tap.or(hold);
    let center_color = if tap.is_some() {
        center_color
    } else {
        hold_color
    };

    if let Some(label) = center_label {
        draw_label_in_rect_limited(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + rect.w / 4,
                y: rect.y + rect.h / 3,
                w: rect.w / 2,
                h: rect.h / 3,
            },
            label,
            center_color,
            8,
        );
    }
}

fn draw_label_in_rect(
    buf: &mut [u8],
    width: u32,
    height: u32,
    rect: RectPx,
    label: &str,
    color: [u8; 4],
) {
    draw_label_in_rect_limited(buf, width, height, rect, label, color, 8);
}

fn draw_label_in_rect_limited(
    buf: &mut [u8],
    width: u32,
    height: u32,
    rect: RectPx,
    label: &str,
    color: [u8; 4],
    max_scale: i32,
) {
    let text = label
        .chars()
        .filter(|ch| ch.is_ascii_graphic() || *ch == ' ')
        .take(8)
        .map(|ch| ch.to_ascii_uppercase())
        .collect::<Vec<_>>();
    if text.is_empty() {
        return;
    }

    let glyph_w = 3;
    let glyph_h = 5;
    let spacing = 1;
    let total_units = text.len() as i32 * glyph_w + (text.len() as i32 - 1) * spacing;
    let scale_x = (rect.w / (total_units + 2)).max(1);
    let scale_y = (rect.h / (glyph_h + 2)).max(1);
    let scale = scale_x.min(scale_y).clamp(1, max_scale.max(1));
    let total_w = total_units * scale;
    let total_h = glyph_h * scale;
    let mut x = rect.x + (rect.w - total_w) / 2;
    let y = rect.y + (rect.h - total_h) / 2;

    for ch in text {
        draw_glyph(buf, width, height, x, y, scale, ch, color);
        x += (glyph_w + spacing) * scale;
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

fn draw_glyph(
    buf: &mut [u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    scale: i32,
    ch: char,
    color: [u8; 4],
) {
    let pattern = glyph_3x5(ch);
    for (row, bits) in pattern.iter().enumerate() {
        for col in 0usize..3 {
            if *bits & (1u8 << (2 - col)) == 0 {
                continue;
            }

            fill_rect(
                buf,
                width,
                height,
                RectPx {
                    x: x + col as i32 * scale,
                    y: y + row as i32 * scale,
                    w: scale,
                    h: scale,
                },
                color,
            );
        }
    }
}

fn glyph_3x5(ch: char) -> [u8; 5] {
    match ch {
        'A' => [0b010, 0b101, 0b111, 0b101, 0b101],
        'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        'C' => [0b111, 0b100, 0b100, 0b100, 0b111],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
        'F' => [0b111, 0b100, 0b110, 0b100, 0b100],
        'G' => [0b111, 0b100, 0b101, 0b101, 0b111],
        'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        'J' => [0b001, 0b001, 0b001, 0b101, 0b111],
        'K' => [0b101, 0b101, 0b110, 0b101, 0b101],
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'N' => [0b101, 0b111, 0b111, 0b111, 0b101],
        'O' => [0b111, 0b101, 0b101, 0b101, 0b111],
        'P' => [0b111, 0b101, 0b111, 0b100, 0b100],
        'Q' => [0b111, 0b101, 0b101, 0b111, 0b001],
        'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        'S' => [0b111, 0b100, 0b111, 0b001, 0b111],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
        'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
        'W' => [0b101, 0b101, 0b111, 0b111, 0b101],
        'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        '-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        '_' => [0b000, 0b000, 0b000, 0b000, 0b111],
        '+' => [0b000, 0b010, 0b111, 0b010, 0b000],
        '=' => [0b000, 0b111, 0b000, 0b111, 0b000],
        '!' => [0b010, 0b010, 0b010, 0b000, 0b010],
        '?' => [0b111, 0b001, 0b010, 0b000, 0b010],
        '@' => [0b111, 0b101, 0b111, 0b100, 0b111],
        '#' => [0b101, 0b111, 0b101, 0b111, 0b101],
        '$' => [0b011, 0b110, 0b010, 0b011, 0b110],
        '%' => [0b101, 0b001, 0b010, 0b100, 0b101],
        '^' => [0b010, 0b101, 0b000, 0b000, 0b000],
        '&' => [0b010, 0b101, 0b010, 0b101, 0b011],
        '*' => [0b101, 0b010, 0b111, 0b010, 0b101],
        '(' => [0b001, 0b010, 0b010, 0b010, 0b001],
        ')' => [0b100, 0b010, 0b010, 0b010, 0b100],
        '[' => [0b111, 0b100, 0b100, 0b100, 0b111],
        ']' => [0b111, 0b001, 0b001, 0b001, 0b111],
        '{' => [0b011, 0b010, 0b110, 0b010, 0b011],
        '}' => [0b110, 0b010, 0b011, 0b010, 0b110],
        ';' => [0b000, 0b010, 0b000, 0b010, 0b100],
        ':' => [0b000, 0b010, 0b000, 0b010, 0b000],
        '\'' => [0b010, 0b010, 0b000, 0b000, 0b000],
        '"' => [0b101, 0b101, 0b000, 0b000, 0b000],
        '`' => [0b100, 0b010, 0b000, 0b000, 0b000],
        '~' => [0b000, 0b011, 0b110, 0b000, 0b000],
        '\\' => [0b100, 0b100, 0b010, 0b001, 0b001],
        '|' => [0b010, 0b010, 0b010, 0b010, 0b010],
        ',' => [0b000, 0b000, 0b000, 0b010, 0b100],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        '/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        '<' => [0b001, 0b010, 0b100, 0b010, 0b001],
        '>' => [0b100, 0b010, 0b001, 0b010, 0b100],
        ' ' => [0b000, 0b000, 0b000, 0b000, 0b000],
        _ => [0b111, 0b001, 0b010, 0b000, 0b010],
    }
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

fn draw_circle(
    buf: &mut [u8],
    width: u32,
    height: u32,
    cx: i32,
    cy: i32,
    radius: i32,
    color: [u8; 4],
) {
    let r2 = radius * radius;
    let x0 = (cx - radius).max(0);
    let y0 = (cy - radius).max(0);
    let x1 = (cx + radius).min(width as i32 - 1);
    let y1 = (cy + radius).min(height as i32 - 1);

    for y in y0..=y1 {
        for x in x0..=x1 {
            let dx = x - cx;
            let dy = y - cy;
            if dx * dx + dy * dy <= r2 {
                let index = (((y as u32) * width + x as u32) * 4) as usize;
                buf[index..index + 4].copy_from_slice(&color);
            }
        }
    }
}

fn load_xkb_keymap(config: &Config) -> Result<Vec<u8>> {
    let mut bytes = if let Some(path) = &config.xkb_keymap_path {
        fs::read(path).with_context(|| format!("read XKB keymap {}", path.display()))?
    } else {
        DEFAULT_XKB_KEYMAP.as_bytes().to_vec()
    };

    if !bytes.ends_with(&[0]) {
        bytes.push(0);
    }

    Ok(bytes)
}

const DEFAULT_XKB_KEYMAP: &str = r#"xkb_keymap {
xkb_keycodes "evdev+aliases(qwerty)" {
    include "evdev+aliases(qwerty)"
};
xkb_types "complete" {
    include "complete"
};
xkb_compatibility "complete" {
    include "complete"
};
xkb_symbols "pc+us+inet(evdev)" {
    include "pc+us+inet(evdev)"
};
xkb_geometry "pc(pc105)" {
    include "pc(pc105)"
};
};
"#;

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind::<wl_compositor::WlCompositor, _, _>(
                        name,
                        version.min(6),
                        qh,
                        (),
                    ));
                }
                "wl_shm" => {
                    state.shm =
                        Some(registry.bind::<wl_shm::WlShm, _, _>(name, version.min(1), qh, ()));
                }
                "wl_seat" => {
                    let seat = registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(8), qh, ());
                    let touch = seat.get_touch(qh, ());
                    state.touch = Some(touch);
                    state.seat = Some(seat);
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(
                        registry.bind::<zwlr_layer_shell_v1::ZwlrLayerShellV1, _, _>(
                            name,
                            version.min(4),
                            qh,
                            (),
                        ),
                    );
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.virtual_keyboard_manager = Some(registry.bind::<
                        zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
                        _,
                        _,
                    >(
                        name,
                        version.min(1),
                        qh,
                        (),
                    ));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_compositor::WlCompositor,
        _event: wl_compositor::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_region::WlRegion, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_region::WlRegion,
        _event: wl_region::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm::WlShm, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_shm::WlShm,
        _event: wl_shm::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_shm_pool::WlShmPool,
        _event: wl_shm_pool::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for App {
    fn event(
        state: &mut Self,
        proxy: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_buffer::Event::Release) {
            for backing in &mut state.buffers {
                if backing.buffer == proxy.clone() {
                    backing.released = true;
                    break;
                }
            }
            state.buffers.retain(|backing| !backing.released);
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_seat::WlSeat,
        _event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_surface::WlSurface,
        _event: wl_surface::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        _event: zwp_virtual_keyboard_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
        _event: zwp_virtual_keyboard_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_touch::WlTouch, ()> for App {
    fn event(
        state: &mut Self,
        _proxy: &wl_touch::WlTouch,
        event: wl_touch::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_touch::Event::Down { time, id, x, y, .. } => {
                state.touch_down(qh, id, time, x, y);
            }
            wl_touch::Event::Motion { time, id, x, y } => {
                state.touch_motion(qh, id, time, x, y);
            }
            wl_touch::Event::Up { time, id, .. } => {
                state.touch_up(qh, id, time);
            }
            wl_touch::Event::Cancel => {
                state.touch_cancel(qh);
            }
            _ => {}
        }
    }
}

impl Dispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        _event: zwlr_layer_shell_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        state: &mut Self,
        layer_surface: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer_surface.ack_configure(serial);
                state.width = width;
                state.height = height;
                state.capture_policy = state.engine.capture_policy(&state.config);
                if let Err(err) = state.attach_overlay_buffer(qh, width, height) {
                    eprintln!("touchdeck: failed to attach overlay buffer: {err:?}");
                    state.running = false;
                    return;
                }
                if let Err(err) = state.apply_input_region(qh, &state.capture_policy) {
                    eprintln!("touchdeck: failed to apply input region: {err:?}");
                    state.running = false;
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.running = false;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    fn test_config() -> Config {
        let mut config = Config {
            action_swipe_left: Some(NiriAction::FocusWorkspaceDown),
            action_swipe_right: Some(NiriAction::FocusWorkspaceUp),
            action_swipe_up: Some(NiriAction::FocusColumnRight),
            action_swipe_down: Some(NiriAction::FocusColumnLeft),
            action_two_finger_tap: Some(NiriAction::ToggleOverview),
            tap_radius: 48.0,
            two_finger_tap_ms: 350,
            exit_tap_ms: 450,
            hold_ms: 180,
            repeat_start_ms: 360,
            repeat_interval_ms: 45,
            double_tap_ms: 280,
            swipe_threshold_ratio: 0.08,
            swipe_threshold_min: 64.0,
            swipe_threshold_max: 140.0,
            debug_alpha: 0,
            debug_draw: false,
            mode_hint_ms: 700,
            modifier_tap_ms: 40,
            log_touch: false,
            record_trace_path: None,
            xkb_keymap_path: None,
            text_output: TextOutputConfig {
                backend: TextOutputBackend::VirtualKeyboard,
                ime_socket: default_ime_socket_path(),
            },
            slots: test_slots(),
            keymap: Keymap::default(),
            macros: MacroRegistry::default(),
            exit_corner_enabled: true,
            exit_corner_ratio: 0.12,
            exit_corner_tap_ms: 350,
        };
        apply_example_keymap(&mut config);
        config
    }

    fn test_slots() -> SlotRegistry {
        SlotRegistry::from_svg_str(include_str!("../layouts/phone-portrait.svg")).unwrap()
    }

    fn apply_example_keymap(config: &mut Config) {
        let mut file_config: FileConfig =
            toml::from_str(include_str!("../touchdeck.example.toml")).unwrap();

        if let Some(macros) = file_config.macros.take() {
            config.macros.clear();
            for (name, macro_config) in macros {
                config
                    .macros
                    .insert(&name, parse_action_steps(macro_config.steps).unwrap());
            }
        }

        let mut behavior_registry = BehaviorRegistry::default();
        if let Some(behaviors) = file_config.behaviors.take() {
            behavior_registry.extend(behaviors);
        }
        if let Some(keyboard) = &file_config.keyboard {
            if let Some(behaviors) = &keyboard.behaviors {
                behavior_registry.extend(behaviors.clone());
            }
        }

        config.keymap.bindings.clear();
        if let Some(bindings) = file_config.bindings.take() {
            for binding in bindings {
                config
                    .keymap
                    .bindings
                    .push(
                        Binding::from_file_config(
                            binding,
                            &config.slots,
                            &config.macros,
                            &behavior_registry,
                        )
                        .unwrap(),
                    );
            }
        }

        if let Some(keyboard) = file_config.keyboard {
            if let Some(maps) = keyboard.layers {
                config
                    .keymap
                    .bindings
                    .extend(
                        expand_keyboard_maps(
                            maps,
                            &config.slots,
                            &config.macros,
                            &behavior_registry,
                        )
                        .unwrap(),
                    );
            }
        }
    }

    #[test]
    fn mod_morph_hold_keeps_selected_binding_until_release() {
        let mut app = App::default();
        app.config = test_config();

        let shift = app.press_action(GestureAction::KeyHold(KEY_LEFTSHIFT));
        assert_eq!(app.held_modifier_mask & XKB_MOD_SHIFT, XKB_MOD_SHIFT);
        assert_eq!(app.modifier_mask & XKB_MOD_SHIFT, XKB_MOD_SHIFT);

        let morph = app.press_action(GestureAction::ModMorph {
            mods: XKB_MOD_SHIFT,
            keep_mods: 0,
            normal: Box::new(GestureAction::KeyHold(KEY_A)),
            morph: Box::new(GestureAction::KeyHold(KEY_B)),
        });

        let PressedAction::ModMorph {
            masked_mods,
            pressed,
        } = &morph
        else {
            panic!("expected active mod-morph state");
        };
        assert_eq!(*masked_mods, XKB_MOD_SHIFT);
        assert!(matches!(pressed.as_ref(), PressedAction::Key(KEY_B)));
        assert_eq!(app.modifier_mask & XKB_MOD_SHIFT, 0);

        app.release_pressed_action(shift);
        assert_eq!(app.held_modifier_mask & XKB_MOD_SHIFT, 0);

        app.release_pressed_action(morph);
        assert_eq!(app.modifier_mask & XKB_MOD_SHIFT, 0);
        assert!(app.modifier_mask_stack.is_empty());
    }

    #[test]
    fn one_shot_mod_morph_restores_held_modifiers_after_shifted_morph() {
        let mut app = App::default();
        app.config = test_config();

        let shift = app.press_action(GestureAction::KeyHold(KEY_LEFTSHIFT));
        app.dispatch_action(GestureAction::ModMorph {
            mods: XKB_MOD_SHIFT,
            keep_mods: 0,
            normal: Box::new(GestureAction::KeySequence(vec![KeyChord {
                keys: vec![KEY_SLASH],
            }])),
            morph: Box::new(GestureAction::KeySequence(vec![KeyChord {
                keys: vec![KEY_LEFTSHIFT, KEY_SLASH],
            }])),
        });

        assert_eq!(app.held_modifier_mask & XKB_MOD_SHIFT, XKB_MOD_SHIFT);
        assert_eq!(app.modifier_mask & XKB_MOD_SHIFT, XKB_MOD_SHIFT);
        assert!(app.modifier_mask_stack.is_empty());

        app.release_pressed_action(shift);
        assert_eq!(app.modifier_mask & XKB_MOD_SHIFT, 0);
    }
}
