use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
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

use touchdeck::protocol::ImeStatus;

mod action;
mod action_executor;
mod config;
mod engine;
mod geometry;
mod gesture;
mod ime_overlay;
mod key;
mod keymap;
mod layout;
mod mode;
mod niri_backend;
mod renderer;
mod trace;
mod wayland_overlay;

use action_executor::{ActionExecutor, ExecutorOutcome};
use config::Config;
use engine::{CapturePolicy, Engine, EngineEffect, TouchSample, TraceEvent};
use geometry::{RectNorm, RectPx, SurfaceSize};
use layout::{Slot, SlotRole, SlotTarget};
use mode::{mode_hint_color, mode_hint_label, Mode, SlotGestureKind};
use renderer::{
    draw_circle, draw_keycap_labels, draw_label_in_rect, draw_label_in_rect_limited,
    draw_rect_frame, fill_rect, KeycapLabels, TextRenderer,
};
use trace::TraceRecorder;
use wayland_overlay::Overlay;

const NAMESPACE: &str = "touchdeck";

struct App {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    virtual_keyboard_manager: Option<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1>,
    seat: Option<wl_seat::WlSeat>,
    touch: Option<wl_touch::WlTouch>,
    overlay: Overlay,
    config: Config,
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
    executor: ActionExecutor,
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
            seat: None,
            touch: None,
            overlay: Overlay::default(),
            config,
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
            executor: ActionExecutor::default(),
            running: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ModeHint {
    mode: Mode,
    until_ms: u64,
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

        self.overlay.init(compositor, layer_shell, qh, NAMESPACE);

        self.init_virtual_keyboard(qh)?;

        Ok(())
    }

    fn init_virtual_keyboard(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let Some(manager) = self.virtual_keyboard_manager.as_ref() else {
            self.executor.clear_virtual_keyboard();
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
        self.executor.set_virtual_keyboard(keyboard, file);
        eprintln!("touchdeck: virtual keyboard initialized");

        Ok(())
    }

    fn surface_size(&self) -> SurfaceSize {
        self.overlay.surface_size()
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
        if let Some((width, height)) = self.overlay.dimensions() {
            self.attach_overlay_buffer(qh, width, height)?;
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
        if let Some((width, height)) = self.overlay.dimensions() {
            self.attach_overlay_buffer(qh, width, height)?;
        }
        Ok(())
    }

    fn attach_overlay_buffer(
        &mut self,
        qh: &QueueHandle<Self>,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let shm = self
            .shm
            .as_ref()
            .ok_or_else(|| anyhow!("wl_shm global is unavailable"))?
            .clone();
        let mut overlay = std::mem::take(&mut self.overlay);
        let result = overlay.attach_buffer(&shm, qh, width, height, |mmap, width, height| {
            self.render_overlay(mmap, width, height);
        });
        self.overlay = overlay;
        result
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
            if ime_overlay::should_render_ime_status(&self.ime_status, self.engine.mode) {
                ime_overlay::render_ime_status(
                    &mut self.text_renderer,
                    mmap,
                    width,
                    height,
                    &self.ime_status,
                );
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

        if ime_overlay::should_render_ime_status(&self.ime_status, self.engine.mode) {
            ime_overlay::render_ime_status(
                &mut self.text_renderer,
                mmap,
                width,
                height,
                &self.ime_status,
            );
        }

        self.render_mode_hint(mmap, width, height);
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
                KeycapLabels {
                    tap: tap_label.as_deref(),
                    hold: hold_label.as_deref(),
                    up: up_label.as_deref(),
                    down: down_label.as_deref(),
                    left: left_label.as_deref(),
                    right: right_label.as_deref(),
                },
            );
        }
    }
}

impl App {
    fn apply_input_region(&self, qh: &QueueHandle<Self>, policy: &CapturePolicy) -> Result<()> {
        let compositor = self
            .compositor
            .as_ref()
            .ok_or_else(|| anyhow!("Wayland compositor global is unavailable"))?;
        self.overlay.apply_input_region(compositor, qh, policy)
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
                    let outcome = self.executor.dispatch_action(
                        action,
                        self.now_ms(),
                        &self.config,
                        &mut self.ime_status,
                        &mut self.ime_status_dirty,
                    );
                    self.apply_executor_outcome(outcome);
                    self.redraw_ime_if_dirty(qh)
                }
                EngineEffect::Press { hold_id, action } => {
                    let outcome = self.executor.press_action(
                        hold_id,
                        action,
                        self.now_ms(),
                        &self.config,
                        &mut self.ime_status,
                        &mut self.ime_status_dirty,
                    );
                    self.apply_executor_outcome(outcome);
                    self.redraw_ime_if_dirty(qh)
                }
                EngineEffect::Release { hold_id } => {
                    let outcome = self.executor.release_action(
                        hold_id,
                        self.now_ms(),
                        &self.config,
                        &mut self.ime_status,
                        &mut self.ime_status_dirty,
                    );
                    self.apply_executor_outcome(outcome);
                    self.redraw_ime_if_dirty(qh)
                }
                EngineEffect::Redraw => {
                    if let Some((width, height)) = self.overlay.dimensions() {
                        self.attach_overlay_buffer(qh, width, height)
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

    fn apply_executor_outcome(&mut self, outcome: ExecutorOutcome) {
        if let Some(last_action) = outcome.last_action {
            self.engine.last_action = Some(last_action);
        }
        if outcome.exit {
            self.running = false;
        }
    }

    fn redraw_ime_if_dirty(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        if let Some((width, height)) = self.overlay.dimensions().filter(|_| self.ime_status_dirty) {
            self.ime_status_dirty = false;
            self.attach_overlay_buffer(qh, width, height)
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
        let effects = self.engine.handle_down(
            TouchSample {
                now_ms,
                time,
                id,
                x,
                y,
            },
            &config,
            size,
        );
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
        let effects = self.engine.handle_motion(
            TouchSample {
                now_ms,
                time,
                id,
                x,
                y,
            },
            &config,
            size,
        );
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
    Err(err.into())
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
            state.overlay.mark_buffer_released(proxy);
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
                state
                    .overlay
                    .ack_configure(layer_surface, serial, width, height);
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
