use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use tempfile::tempfile;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_output, wl_region, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface, wl_touch,
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
mod evdev_touch;
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
use config::{Config, InputConfig, TouchInputBackend};
use engine::{CapturePolicy, Engine, EngineEffect, TouchSample, TraceEvent};
use evdev_touch::{discover_touch_device_infos, EvdevTouchBackend, RawTouchEvent};
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
const RAW_TOUCH_RETRY_MS: u64 = 500;

struct App {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    virtual_keyboard_manager: Option<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1>,
    seat: Option<wl_seat::WlSeat>,
    touch: Option<wl_touch::WlTouch>,
    outputs: Vec<OutputInfo>,
    sessions: Vec<TouchSession>,
    next_session_id: usize,
    routed_wayland_touches: HashMap<i32, usize>,
    session_scan_retry_at_ms: Option<u64>,
    session_scan_last_error: Option<String>,
    config: Config,
    trace: Option<TraceRecorder>,
    started_at: Option<Instant>,
    ime_status: ImeStatus,
    ime_status_dirty: bool,
    ime_status_rx: Option<Receiver<ImeStatus>>,
    executor: ActionExecutor,
    running: bool,
}

#[derive(Clone)]
struct OutputInfo {
    global_name: u32,
    output: wl_output::WlOutput,
    name: Option<String>,
}

struct TouchSession {
    id: usize,
    touch_device: Option<PathBuf>,
    sunshine_output: Option<String>,
    overlay_output_global: Option<u32>,
    overlay_output_name: Option<String>,
    overlay_wait_reason: Option<String>,
    raw_touch: Option<EvdevTouchBackend>,
    raw_touch_retry_at_ms: Option<u64>,
    raw_touch_last_error: Option<String>,
    overlay: Overlay,
    engine: Engine,
    capture_policy: CapturePolicy,
    mode_hint: Option<ModeHint>,
    last_presented_mode: Mode,
    text_renderer: TextRenderer,
}

impl TouchSession {
    fn wayland(id: usize, config: &Config) -> Self {
        let engine = Engine::default();
        let capture_policy = engine.capture_policy(config);
        Self {
            id,
            touch_device: None,
            sunshine_output: config.input.sunshine_output.clone(),
            overlay_output_global: None,
            overlay_output_name: None,
            overlay_wait_reason: None,
            raw_touch: None,
            raw_touch_retry_at_ms: None,
            raw_touch_last_error: None,
            overlay: Overlay::default(),
            engine,
            capture_policy,
            mode_hint: None,
            last_presented_mode: Mode::Base,
            text_renderer: TextRenderer::new(),
        }
    }

    fn evdev(
        id: usize,
        touch_device: PathBuf,
        sunshine_output: Option<String>,
        raw_touch: EvdevTouchBackend,
        config: &Config,
    ) -> Self {
        let engine = Engine::default();
        let capture_policy = engine.capture_policy(config);
        Self {
            id,
            touch_device: Some(touch_device),
            sunshine_output,
            overlay_output_global: None,
            overlay_output_name: None,
            overlay_wait_reason: None,
            raw_touch: Some(raw_touch),
            raw_touch_retry_at_ms: None,
            raw_touch_last_error: None,
            overlay: Overlay::default(),
            engine,
            capture_policy,
            mode_hint: None,
            last_presented_mode: Mode::Base,
            text_renderer: TextRenderer::new(),
        }
    }

    fn output_name<'a>(&'a self, config: &'a Config) -> Option<&'a str> {
        self.sunshine_output
            .as_deref()
            .or(config.input.sunshine_output.as_deref())
    }
}

impl Default for App {
    fn default() -> Self {
        let config = Config::default();
        Self {
            compositor: None,
            shm: None,
            layer_shell: None,
            virtual_keyboard_manager: None,
            seat: None,
            touch: None,
            outputs: Vec::new(),
            sessions: Vec::new(),
            next_session_id: 0,
            routed_wayland_touches: HashMap::new(),
            session_scan_retry_at_ms: None,
            session_scan_last_error: None,
            config,
            trace: None,
            started_at: None,
            ime_status: ImeStatus::default(),
            ime_status_dirty: false,
            ime_status_rx: None,
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
        let (status_tx, status_rx) = mpsc::channel();
        let event_tx = touchdeck::ime::spawn_embedded(status_tx);
        app.executor.set_ime_event_sender(event_tx);
        app.ime_status_rx = Some(status_rx);
    }

    display.get_registry(&qh, ());
    event_queue
        .roundtrip(&mut app)
        .context("collect Wayland globals")?;
    if app.config.input.sunshine_output.is_some() {
        event_queue
            .roundtrip(&mut app)
            .context("collect Wayland output metadata")?;
    }

    app.init_overlay(&qh)?;
    eprintln!(
        "touchdeck: overlay initialized; touch backend={}; Wayland input region {}",
        app.config.input.touch_backend.as_str(),
        if app.config.input.touch_backend == TouchInputBackend::Evdev {
            "display-only except passthrough zones"
        } else {
            "follows capture policy"
        }
    );

    while app.running {
        event_queue
            .dispatch_pending(&mut app)
            .context("dispatch pending Wayland events")?;
        app.drain_ime_status(&qh)
            .context("drain touchdeck-ime status")?;

        let now_ms = app.now_ms();
        let config = app.config.clone();
        for index in 0..app.sessions.len() {
            let size = app.sessions[index].overlay.surface_size();
            let effects = app.sessions[index]
                .engine
                .process_timers(now_ms, &config, size);
            app.apply_effects_or_stop(&qh, index, effects);
            if !app.running {
                break;
            }
        }
        app.expire_mode_hints(&qh)
            .context("expire mode hint overlays")?;
        app.maybe_connect_raw_touch(&qh);

        if !app.running {
            break;
        }

        event_queue.flush().context("flush Wayland requests")?;
        let timeout = app.poll_timeout();
        let wayland_fd = event_queue.as_fd().as_raw_fd();
        let raw_touch_fds = app.raw_touch_fds();

        let Some(guard) = event_queue.prepare_read() else {
            continue;
        };

        event_queue.flush().context("flush Wayland requests")?;
        let poll_result =
            poll_fds(wayland_fd, &raw_touch_fds, timeout).context("poll input fds")?;
        if poll_result.wayland {
            guard.read().context("read Wayland events")?;
        } else {
            drop(guard);
        }
        for index in poll_result.raw_touch {
            app.drain_raw_touch(&qh, index)
                .context("drain raw touch events")?;
        }
    }

    Ok(())
}

impl App {
    fn init_overlay(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        self.init_virtual_keyboard(qh)?;

        match self.config.input.touch_backend {
            TouchInputBackend::Wayland => {
                self.touch
                    .as_ref()
                    .ok_or_else(|| anyhow!("wl_touch is unavailable on this Wayland seat"))?;
                self.ensure_wayland_session();
            }
            TouchInputBackend::Evdev => {
                self.session_scan_retry_at_ms = Some(self.now_ms());
                self.maybe_connect_raw_touch(qh);
            }
        }

        self.try_init_all_session_overlays(qh)?;
        Ok(())
    }

    fn ensure_wayland_session(&mut self) {
        if self.config.input.touch_backend != TouchInputBackend::Wayland || !self.sessions.is_empty()
        {
            return;
        }
        let id = self.allocate_session_id();
        self.sessions.push(TouchSession::wayland(id, &self.config));
    }

    fn allocate_session_id(&mut self) -> usize {
        let id = self.next_session_id;
        self.next_session_id += 1;
        id
    }

    fn try_init_all_session_overlays(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        for index in 0..self.sessions.len() {
            self.try_init_session_overlay(qh, index)?;
        }
        Ok(())
    }

    fn try_init_session_overlay(&mut self, qh: &QueueHandle<Self>, index: usize) -> Result<()> {
        if self.sessions[index].overlay.is_initialized() {
            return Ok(());
        }

        let compositor = self
            .compositor
            .clone()
            .ok_or_else(|| anyhow!("Wayland compositor global is unavailable"))?;
        let layer_shell = self
            .layer_shell
            .clone()
            .ok_or_else(|| anyhow!("zwlr_layer_shell_v1 global is unavailable"))?;

        let output = match self.target_output(index) {
            Ok(output) => output,
            Err(err) => {
                self.set_overlay_wait_reason(index, err.to_string());
                return Ok(());
            }
        };
        let output_ref = output.as_ref().map(|output| &output.output);
        let session = &mut self.sessions[index];

        session
            .overlay
            .init(&compositor, &layer_shell, output_ref, qh, NAMESPACE);
        session.overlay_output_global = output.as_ref().map(|output| output.global_name);
        session.overlay_output_name = output.as_ref().and_then(|output| output.name.clone());
        session.overlay_wait_reason = None;
        if let Some(name) = session.overlay_output_name.as_deref() {
            eprintln!(
                "touchdeck: binding session {} to sunshine_output={name}",
                session.id
            );
        } else {
            eprintln!("touchdeck: binding session {} without explicit output", session.id);
        }

        let capture_policy = session.capture_policy.clone();
        self.apply_input_region(qh, index, &capture_policy)?;
        self.sync_raw_touch_grab(index, &capture_policy)?;

        Ok(())
    }

    fn set_overlay_wait_reason(&mut self, index: usize, reason: String) {
        let session = &mut self.sessions[index];
        if session.overlay_wait_reason.as_deref() != Some(reason.as_str()) {
            eprintln!(
                "touchdeck: waiting to initialize session {} overlay: {reason}",
                session.id
            );
            session.overlay_wait_reason = Some(reason);
        }
    }

    fn reset_overlay_binding(&mut self, qh: &QueueHandle<Self>, index: usize, reason: &str) {
        if self.sessions[index].overlay.is_initialized() {
            let session_id = self.sessions[index].id;
            eprintln!("touchdeck: resetting session {session_id} overlay binding: {reason}");
            self.sessions[index].overlay.reset();
            self.sessions[index].overlay_output_global = None;
            self.sessions[index].overlay_output_name = None;
            let capture_policy = self.sessions[index].capture_policy.clone();
            if let Err(err) = self.sync_raw_touch_grab(index, &capture_policy) {
                eprintln!("touchdeck: failed to sync raw touch grab after overlay reset: {err:?}");
                self.running = false;
                return;
            }
        }
        if let Err(err) = self.try_init_session_overlay(qh, index) {
            eprintln!("touchdeck: failed to initialize overlay after reset: {err:?}");
            self.running = false;
        }
    }

    fn known_output_names(&self) -> String {
        self.outputs
            .iter()
            .filter_map(|output| output.name.as_deref())
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn target_output(&self, index: usize) -> Result<Option<OutputInfo>> {
        let Some(name) = self.sessions[index].output_name(&self.config) else {
            return Ok(None);
        };

        let Some(output) = self
            .outputs
            .iter()
            .find(|output| output.name.as_deref() == Some(name))
        else {
            return Err(anyhow!(
                "no Wayland output named {name:?} for sunshine-output; known outputs: {}",
                self.known_output_names()
            ));
        };

        Ok(Some(output.clone()))
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

    fn removed_output(&mut self, qh: &QueueHandle<Self>, name: u32) {
        let Some(index) = self
            .outputs
            .iter()
            .position(|output| output.global_name == name)
        else {
            return;
        };
        let removed = self.outputs.remove(index);
        let reason = format!(
            "bound output {} was removed",
            removed.name.as_deref().unwrap_or("<unknown>")
        );
        let affected = self
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(index, session)| (session.overlay_output_global == Some(name)).then_some(index))
            .collect::<Vec<_>>();
        for index in affected {
            self.reset_overlay_binding(qh, index, &reason);
        }
    }

    fn now_ms(&self) -> u64 {
        self.started_at
            .map(|started_at| started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0)
    }

    fn poll_timeout(&self) -> Option<Duration> {
        let mut deadline = None;
        for session in &self.sessions {
            if let Some(next) = session.engine.next_timer_deadline_ms() {
                deadline = Some(deadline.map_or(next, |deadline: u64| deadline.min(next)));
            }
            if let Some(hint) = session.mode_hint {
                deadline = Some(deadline.map_or(hint.until_ms, |deadline: u64| deadline.min(hint.until_ms)));
            }
            if let Some(retry_at) = session.raw_touch_retry_at_ms {
                deadline = Some(deadline.map_or(retry_at, |deadline: u64| deadline.min(retry_at)));
            }
        }
        if self.ime_status_rx.is_some() {
            let refresh_deadline = self.now_ms().saturating_add(33);
            deadline = Some(deadline.map_or(refresh_deadline, |deadline: u64| {
                deadline.min(refresh_deadline)
            }));
        }
        if let Some(retry_at) = self.session_scan_retry_at_ms {
            deadline = Some(deadline.map_or(retry_at, |deadline: u64| deadline.min(retry_at)));
        }

        deadline.map(|deadline_ms| {
            let now_ms = self.now_ms();
            Duration::from_millis(deadline_ms.saturating_sub(now_ms))
        })
    }

    fn expire_mode_hints(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let now_ms = self.now_ms();
        for index in 0..self.sessions.len() {
            let expired = self.sessions[index]
                .mode_hint
                .is_some_and(|hint| now_ms >= hint.until_ms);
            if !expired {
                continue;
            }
            self.sessions[index].mode_hint = None;
            if let Some((width, height)) = self.sessions[index].overlay.dimensions() {
                self.attach_overlay_buffer(qh, index, width, height)?;
            }
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
        self.redraw_all_configured_overlays(qh)
    }

    fn attach_overlay_buffer(
        &mut self,
        qh: &QueueHandle<Self>,
        index: usize,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let shm = self
            .shm
            .as_ref()
            .ok_or_else(|| anyhow!("wl_shm global is unavailable"))?
            .clone();
        let config = self.config.clone();
        let ime_status = self.ime_status.clone();
        let now_ms = self.now_ms();
        let mut session = self.sessions.remove(index);
        let mut overlay = std::mem::take(&mut session.overlay);
        let result = overlay.attach_buffer(&shm, qh, width, height, |mmap, width, height| {
            render_session_overlay(&config, &ime_status, now_ms, &mut session, mmap, width, height);
        });
        session.overlay = overlay;
        self.sessions.insert(index, session);
        result
    }

    fn redraw_all_configured_overlays(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        for index in 0..self.sessions.len() {
            if let Some((width, height)) = self.sessions[index].overlay.dimensions() {
                self.attach_overlay_buffer(qh, index, width, height)?;
            }
        }
        Ok(())
    }

    fn apply_input_region(
        &self,
        qh: &QueueHandle<Self>,
        index: usize,
        policy: &CapturePolicy,
    ) -> Result<()> {
        let session = &self.sessions[index];
        if !session.overlay.is_initialized() {
            return Ok(());
        }
        let compositor = self
            .compositor
            .as_ref()
            .ok_or_else(|| anyhow!("Wayland compositor global is unavailable"))?;
        let effective_policy = match self.config.input.touch_backend {
            TouchInputBackend::Wayland => policy.clone(),
            TouchInputBackend::Evdev => match policy {
                CapturePolicy::Zones(_) => policy.clone(),
                CapturePolicy::Fullscreen | CapturePolicy::None => CapturePolicy::None,
            },
        };
        session
            .overlay
            .apply_input_region(compositor, qh, &effective_policy)
    }

    fn apply_capture_policy(
        &mut self,
        qh: &QueueHandle<Self>,
        index: usize,
        policy: &CapturePolicy,
    ) -> Result<()> {
        if self.raw_touch_should_grab(index, policy) {
            self.sync_raw_touch_grab(index, policy)?;
            self.apply_input_region(qh, index, policy)
        } else {
            self.apply_input_region(qh, index, policy)?;
            self.sync_raw_touch_grab(index, policy)
        }
    }

    fn raw_touch_should_grab(&self, index: usize, policy: &CapturePolicy) -> bool {
        self.config.input.touch_backend == TouchInputBackend::Evdev
            && self.config.input.evdev_grab
            && self.sessions[index].overlay.is_initialized()
            && matches!(policy, CapturePolicy::Fullscreen)
    }

    fn raw_touch_passthrough(&self, index: usize) -> bool {
        self.config.input.touch_backend == TouchInputBackend::Evdev
            && matches!(self.sessions[index].capture_policy, CapturePolicy::Zones(_))
    }

    fn sync_raw_touch_grab(&mut self, index: usize, policy: &CapturePolicy) -> Result<()> {
        let should_grab = self.raw_touch_should_grab(index, policy);
        if let Some(raw_touch) = self.sessions[index].raw_touch.as_mut() {
            raw_touch.set_grab(should_grab)?;
        }
        Ok(())
    }

    fn apply_effects_or_stop(
        &mut self,
        qh: &QueueHandle<Self>,
        index: usize,
        effects: Vec<EngineEffect>,
    ) {
        for effect in effects {
            let result = match effect {
                EngineEffect::SetCapture(policy) => {
                    self.sessions[index].capture_policy = policy.clone();
                    self.present_mode_hint_if_changed(index);
                    self.apply_capture_policy(qh, index, &policy)
                }
                EngineEffect::Dispatch(action) => {
                    let outcome = self
                        .executor
                        .dispatch_action(action, self.now_ms(), &self.config);
                    self.apply_executor_outcome(index, outcome);
                    self.redraw_ime_if_dirty(qh)
                }
                EngineEffect::Press { hold_id, action } => {
                    let outcome = self
                        .executor
                        .press_action(hold_id, action, self.now_ms(), &self.config);
                    self.apply_executor_outcome(index, outcome);
                    self.redraw_ime_if_dirty(qh)
                }
                EngineEffect::Release { hold_id } => {
                    let outcome = self
                        .executor
                        .release_action(hold_id, self.now_ms(), &self.config);
                    self.apply_executor_outcome(index, outcome);
                    self.redraw_ime_if_dirty(qh)
                }
                EngineEffect::Redraw => {
                    if let Some((width, height)) = self.sessions[index].overlay.dimensions() {
                        self.attach_overlay_buffer(qh, index, width, height)
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

    fn apply_executor_outcome(&mut self, index: usize, outcome: ExecutorOutcome) {
        if let Some(last_action) = outcome.last_action {
            self.sessions[index].engine.last_action = Some(last_action);
        }
        if outcome.exit {
            self.running = false;
        }
    }

    fn redraw_ime_if_dirty(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        if self.ime_status_dirty {
            self.ime_status_dirty = false;
            self.redraw_all_configured_overlays(qh)
        } else {
            Ok(())
        }
    }

    fn present_mode_hint_if_changed(&mut self, index: usize) {
        let until_ms = self.now_ms() + u64::from(self.config.mode_hint_ms);
        let session = &mut self.sessions[index];
        let mode = session.engine.mode;
        if session.last_presented_mode == mode {
            return;
        }

        session.last_presented_mode = mode;
        if self.config.mode_hint_ms == 0 {
            session.mode_hint = None;
            return;
        }

        session.mode_hint = Some(ModeHint {
            mode,
            until_ms,
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

    fn touch_down(
        &mut self,
        qh: &QueueHandle<Self>,
        index: usize,
        id: i32,
        time: u32,
        x: f64,
        y: f64,
    ) {
        let now_ms = self.now_ms();
        self.record_trace(TraceEvent::Down {
            t: now_ms,
            wl_time: time,
            id,
            x,
            y,
        });
        if self.config.log_touch {
            eprintln!(
                "touchdeck: session={} touch down id={id} time={time} x={x:.1} y={y:.1}",
                self.sessions[index].id
            );
        }

        let config = self.config.clone();
        let size = self.sessions[index].overlay.surface_size();
        let effects = self.sessions[index].engine.handle_down(
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
        self.apply_effects_or_stop(qh, index, effects);
    }

    fn touch_motion(
        &mut self,
        qh: &QueueHandle<Self>,
        index: usize,
        id: i32,
        time: u32,
        x: f64,
        y: f64,
    ) {
        let now_ms = self.now_ms();
        self.record_trace(TraceEvent::Motion {
            t: now_ms,
            wl_time: time,
            id,
            x,
            y,
        });
        if self.config.log_touch {
            eprintln!(
                "touchdeck: session={} touch motion id={id} time={time} x={x:.1} y={y:.1}",
                self.sessions[index].id
            );
        }

        let config = self.config.clone();
        let size = self.sessions[index].overlay.surface_size();
        let effects = self.sessions[index].engine.handle_motion(
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
        self.apply_effects_or_stop(qh, index, effects);
    }

    fn touch_up(&mut self, qh: &QueueHandle<Self>, index: usize, id: i32, time: u32) {
        let now_ms = self.now_ms();
        self.record_trace(TraceEvent::Up {
            t: now_ms,
            wl_time: time,
            id,
        });
        if self.config.log_touch {
            eprintln!(
                "touchdeck: session={} touch up id={id} time={time}",
                self.sessions[index].id
            );
        }

        let config = self.config.clone();
        let size = self.sessions[index].overlay.surface_size();
        let effects = self.sessions[index]
            .engine
            .handle_up(now_ms, time, id, &config, size);
        self.apply_effects_or_stop(qh, index, effects);
    }

    fn touch_cancel(&mut self, qh: &QueueHandle<Self>, index: usize) {
        let now_ms = self.now_ms();
        self.record_trace(TraceEvent::Cancel { t: now_ms });
        if self.config.log_touch {
            eprintln!("touchdeck: session={} touch cancel", self.sessions[index].id);
        }

        let config = self.config.clone();
        let effects = self.sessions[index].engine.handle_cancel(&config);
        self.apply_effects_or_stop(qh, index, effects);
    }

    fn touch_cancel_all(&mut self, qh: &QueueHandle<Self>) {
        self.routed_wayland_touches.clear();
        for index in 0..self.sessions.len() {
            self.touch_cancel(qh, index);
        }
    }

    fn drain_raw_touch(&mut self, qh: &QueueHandle<Self>, index: usize) -> Result<()> {
        let size = self.sessions[index].overlay.surface_size();
        let passthrough = self.raw_touch_passthrough(index);
        let events = {
            let Some(raw_touch) = self.sessions[index].raw_touch.as_mut() else {
                return Ok(());
            };
            match raw_touch.drain_events(size) {
                Ok(events) => events,
                Err(err) => {
                    self.disconnect_raw_touch(qh, index, format!("read failed: {err:#}"));
                    return Ok(());
                }
            }
        };

        if passthrough {
            return Ok(());
        }

        for event in events {
            match event {
                RawTouchEvent::Down { id, time, x, y } => {
                    self.touch_down(qh, index, id, time, x, y);
                }
                RawTouchEvent::Motion { id, time, x, y } => {
                    self.touch_motion(qh, index, id, time, x, y);
                }
                RawTouchEvent::Up { id, time } => {
                    self.touch_up(qh, index, id, time);
                }
            }
        }

        Ok(())
    }

    fn raw_touch_fds(&self) -> Vec<(usize, RawFd)> {
        self.sessions
            .iter()
            .enumerate()
            .filter_map(|(index, session)| session.raw_touch.as_ref().map(|touch| (index, touch.fd())))
            .collect()
    }

    fn maybe_connect_raw_touch(&mut self, qh: &QueueHandle<Self>) {
        if self.config.input.touch_backend != TouchInputBackend::Evdev {
            return;
        }

        let now_ms = self.now_ms();
        self.reconnect_missing_raw_touches(qh, now_ms);
        if self
            .session_scan_retry_at_ms
            .is_some_and(|retry_at| now_ms < retry_at)
        {
            return;
        }
        self.session_scan_retry_at_ms = Some(now_ms + RAW_TOUCH_RETRY_MS);

        let devices = match discover_touch_device_infos(&self.config.input) {
            Ok(devices) => {
                self.session_scan_last_error = None;
                devices
            }
            Err(err) => {
                let message = format!("{err:#}");
                if self.session_scan_last_error.as_deref() != Some(message.as_str()) {
                    eprintln!("touchdeck: waiting for evdev touch devices: {message}");
                    self.session_scan_last_error = Some(message);
                }
                return;
            }
        };

        if devices.is_empty() && self.sessions.is_empty() {
            let message = "no matching evdev touchscreen found".to_string();
            if self.session_scan_last_error.as_deref() != Some(message.as_str()) {
                eprintln!("touchdeck: waiting for evdev touch devices: {message}");
                self.session_scan_last_error = Some(message);
            }
            return;
        }

        let mut claimed_outputs = self
            .sessions
            .iter()
            .filter_map(|session| session.output_name(&self.config).map(str::to_string))
            .collect::<HashSet<_>>();
        let mut claimed_devices = self
            .sessions
            .iter()
            .filter_map(|session| session.touch_device.clone())
            .collect::<HashSet<_>>();

        for device in devices {
            if claimed_devices.contains(&device.path) {
                continue;
            }
            let sunshine_output = self
                .config
                .input
                .sunshine_output
                .clone()
                .or(device.sunshine_output.clone());
            if let Some(output) = &sunshine_output {
                if claimed_outputs.contains(output) {
                    continue;
                }
            }

            match self.open_raw_touch_for_device(&device.path, sunshine_output.as_deref()) {
                Ok(raw_touch) => {
                    let id = self.allocate_session_id();
                    let path = raw_touch.path().to_path_buf();
                    let mut session = TouchSession::evdev(
                        id,
                        path.clone(),
                        sunshine_output.clone(),
                        raw_touch,
                        &self.config,
                    );
                    let index = self.sessions.len();
                    if let Some(raw_touch) = session.raw_touch.as_mut() {
                        let should_grab = self.config.input.evdev_grab
                            && matches!(session.capture_policy, CapturePolicy::Fullscreen);
                        if let Err(err) = raw_touch.set_grab(should_grab) {
                            let message = format!("{err:#}");
                            eprintln!("touchdeck: waiting for evdev touch device: {message}");
                            continue;
                        }
                    }
                    eprintln!(
                        "touchdeck: created session {id} for device={} name={} sunshine_output={}",
                        path.display(),
                        device.name.as_deref().unwrap_or("<unknown>"),
                        sunshine_output.as_deref().unwrap_or("<none>")
                    );
                    self.sessions.push(session);
                    claimed_devices.insert(path);
                    if let Some(output) = sunshine_output {
                        claimed_outputs.insert(output);
                    }
                    if let Err(err) = self.try_init_session_overlay(qh, index) {
                        eprintln!("touchdeck: failed to initialize session overlay: {err:?}");
                        self.running = false;
                        return;
                    }
                }
                Err(err) => {
                    let message = format!("{err:#}");
                    if self.session_scan_last_error.as_deref() != Some(message.as_str()) {
                        eprintln!("touchdeck: waiting for evdev touch device: {message}");
                        self.session_scan_last_error = Some(message);
                    }
                }
            }
        }
    }

    fn reconnect_missing_raw_touches(&mut self, qh: &QueueHandle<Self>, now_ms: u64) {
        for index in 0..self.sessions.len() {
            if self.sessions[index].raw_touch.is_some() || self.sessions[index].touch_device.is_none()
            {
                continue;
            }
            if self.sessions[index]
                .raw_touch_retry_at_ms
                .is_some_and(|retry_at| now_ms < retry_at)
            {
                continue;
            }
            let Some(path) = self.sessions[index].touch_device.clone() else {
                continue;
            };
            let output = self.sessions[index].sunshine_output.clone();
            match self.open_raw_touch_for_device(&path, output.as_deref()) {
                Ok(mut raw_touch) => {
                    let policy = self.sessions[index].capture_policy.clone();
                    let should_grab = self.raw_touch_should_grab(index, &policy);
                    if let Err(err) = raw_touch.set_grab(should_grab) {
                        let message = format!("{err:#}");
                        if self.sessions[index].raw_touch_last_error.as_deref()
                            != Some(message.as_str())
                        {
                            eprintln!("touchdeck: waiting for evdev touch device: {message}");
                            self.sessions[index].raw_touch_last_error = Some(message);
                        }
                        self.sessions[index].raw_touch_retry_at_ms = Some(now_ms + RAW_TOUCH_RETRY_MS);
                        continue;
                    }
                    self.sessions[index].raw_touch = Some(raw_touch);
                    self.sessions[index].raw_touch_last_error = None;
                    self.sessions[index].raw_touch_retry_at_ms = None;
                    if let Err(err) = self.try_init_session_overlay(qh, index) {
                        eprintln!("touchdeck: failed to initialize overlay after evdev reconnect: {err:?}");
                        self.running = false;
                        return;
                    }
                }
                Err(err) => {
                    let message = format!("{err:#}");
                    if self.sessions[index].raw_touch_last_error.as_deref() != Some(message.as_str()) {
                        eprintln!("touchdeck: waiting for evdev touch device: {message}");
                        self.sessions[index].raw_touch_last_error = Some(message);
                    }
                    self.sessions[index].raw_touch_retry_at_ms = Some(now_ms + RAW_TOUCH_RETRY_MS);
                }
            }
        }
    }

    fn open_raw_touch_for_device(
        &self,
        path: &Path,
        sunshine_output: Option<&str>,
    ) -> Result<EvdevTouchBackend> {
        let mut input = InputConfig {
            evdev_touch_device: Some(path.to_path_buf()),
            sunshine_output: sunshine_output.map(str::to_string),
            ..self.config.input.clone()
        };
        input.evdev_touch_device = Some(path.to_path_buf());
        EvdevTouchBackend::open(&input)
    }

    fn disconnect_raw_touch(&mut self, qh: &QueueHandle<Self>, index: usize, reason: String) {
        eprintln!(
            "touchdeck: session {} evdev touch disconnected: {reason}",
            self.sessions[index].id
        );
        self.sessions[index].raw_touch = None;
        self.sessions[index].raw_touch_last_error = None;
        self.sessions[index].raw_touch_retry_at_ms = Some(self.now_ms() + RAW_TOUCH_RETRY_MS);
        self.touch_cancel(qh, index);
    }

    fn session_for_surface(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.sessions
            .iter()
            .position(|session| session.overlay.matches_surface(surface))
    }

    fn session_for_layer_surface(
        &self,
        layer_surface: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    ) -> Option<usize> {
        self.sessions
            .iter()
            .position(|session| session.overlay.matches_layer_surface(layer_surface))
    }
}

fn render_session_overlay(
    config: &Config,
    ime_status: &ImeStatus,
    now_ms: u64,
    session: &mut TouchSession,
    mmap: &mut [u8],
    width: u32,
    height: u32,
) {
    mmap.fill(0);

    let size = SurfaceSize { width, height };
    if config.debug_alpha != 0 && !config.debug_draw {
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
            [0x00, 0x80, 0xff, config.debug_alpha],
        );
        render_mode_hint(now_ms, session, mmap, width, height);
        return;
    }

    if !config.debug_draw {
        if session.engine.mode == Mode::Text {
            render_text_keyboard(config, session, mmap, width, height, size);
        }
        if ime_overlay::should_render_ime_status(ime_status, session.engine.mode) {
            ime_overlay::render_ime_status(
                &mut session.text_renderer,
                mmap,
                width,
                height,
                ime_status,
            );
        }
        render_mode_hint(now_ms, session, mmap, width, height);
        return;
    }

    match session.engine.mode {
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

    for slot in config.slots.slots() {
        let rect = slot.rect.to_px(size);
        let color = slot_debug_color(slot);
        if slot.capture || slot.role == SlotRole::Key {
            fill_rect(mmap, width, height, rect, color);
        } else {
            draw_rect_frame(mmap, width, height, rect, color);
        }

        let label = config
            .keymap
            .slot_label(session.engine.mode, &session.engine.layer_stack, &slot.id)
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

    for binding in config.keymap.bindings.iter().filter(|binding| {
        binding.mode == session.engine.mode && session.engine.layer_stack.contains(&binding.layer)
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

    if let Some(candidate) = &session.engine.hold_candidate {
        if let Some(contact) = session.engine.active.get(&candidate.id) {
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

    for contact in session.engine.active.values() {
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

    if ime_overlay::should_render_ime_status(ime_status, session.engine.mode) {
        ime_overlay::render_ime_status(
            &mut session.text_renderer,
            mmap,
            width,
            height,
            ime_status,
        );
    }

    render_mode_hint(now_ms, session, mmap, width, height);
}

fn render_mode_hint(
    now_ms: u64,
    session: &TouchSession,
    mmap: &mut [u8],
    width: u32,
    height: u32,
) {
    let Some(hint) = session.mode_hint else {
        return;
    };
    if now_ms >= hint.until_ms {
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

fn render_text_keyboard(
    config: &Config,
    session: &TouchSession,
    mmap: &mut [u8],
    width: u32,
    height: u32,
    size: SurfaceSize,
) {
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

    for slot in config.slots.slots() {
        if slot.role != SlotRole::Key {
            continue;
        }

        let tap_label = config.keymap.slot_gesture_label(
            Mode::Text,
            &session.engine.layer_stack,
            &slot.id,
            SlotGestureKind::Tap,
        );
        let hold_label = config.keymap.slot_gesture_label(
            Mode::Text,
            &session.engine.layer_stack,
            &slot.id,
            SlotGestureKind::Hold,
        );
        let up_label = config.keymap.slot_gesture_label(
            Mode::Text,
            &session.engine.layer_stack,
            &slot.id,
            SlotGestureKind::SwipeUp,
        );
        let down_label = config.keymap.slot_gesture_label(
            Mode::Text,
            &session.engine.layer_stack,
            &slot.id,
            SlotGestureKind::SwipeDown,
        );
        let left_label = config.keymap.slot_gesture_label(
            Mode::Text,
            &session.engine.layer_stack,
            &slot.id,
            SlotGestureKind::SwipeLeft,
        );
        let right_label = config.keymap.slot_gesture_label(
            Mode::Text,
            &session.engine.layer_stack,
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

#[derive(Clone, Debug)]
struct PollResult {
    wayland: bool,
    raw_touch: Vec<usize>,
}

fn poll_fds(
    wayland_fd: RawFd,
    raw_touch_fds: &[(usize, RawFd)],
    timeout: Option<Duration>,
) -> Result<PollResult> {
    let timeout_ms = timeout
        .map(|timeout| timeout.as_millis().min(i32::MAX as u128) as i32)
        .unwrap_or(-1);
    let mut pollfds = vec![libc::pollfd {
        fd: wayland_fd,
        events: libc::POLLIN,
        revents: 0,
    }];
    for (_, fd) in raw_touch_fds {
        pollfds.push(libc::pollfd {
            fd: *fd,
            events: libc::POLLIN,
            revents: 0,
        });
    }

    let rc =
        unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, timeout_ms) };
    if rc > 0 {
        let event_mask = libc::POLLIN | libc::POLLERR | libc::POLLHUP;
        let raw_touch = pollfds
            .iter()
            .skip(1)
            .zip(raw_touch_fds.iter())
            .filter_map(|(pollfd, (index, _))| {
                (pollfd.revents & event_mask != 0).then_some(*index)
            })
            .collect();
        return Ok(PollResult {
            wayland: pollfds[0].revents & event_mask != 0,
            raw_touch,
        });
    }
    if rc == 0 {
        return Ok(PollResult {
            wayland: false,
            raw_touch: Vec::new(),
        });
    }

    let err = std::io::Error::last_os_error();
    if err.kind() == std::io::ErrorKind::Interrupted {
        return Ok(PollResult {
            wayland: false,
            raw_touch: Vec::new(),
        });
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
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
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
                "wl_output" => {
                    let output =
                        registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, name);
                    state.outputs.push(OutputInfo {
                        global_name: name,
                        output,
                        name: None,
                    });
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
            },
            wl_registry::Event::GlobalRemove { name } => {
                state.removed_output(qh, name);
            }
            _ => {}
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

impl Dispatch<wl_output::WlOutput, u32> for App {
    fn event(
        state: &mut Self,
        _proxy: &wl_output::WlOutput,
        event: wl_output::Event,
        global_name: &u32,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            if let Some(output) = state
                .outputs
                .iter_mut()
                .find(|output| output.global_name == *global_name)
            {
                output.name = Some(name);
            }
            if let Err(err) = state.try_init_all_session_overlays(qh) {
                eprintln!("touchdeck: failed to initialize overlay after output metadata: {err:?}");
                state.running = false;
            }
        }
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
            for session in &mut state.sessions {
                session.overlay.mark_buffer_released(proxy);
            }
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
            wl_touch::Event::Down {
                time,
                id,
                x,
                y,
                surface,
                ..
            } => {
                if let Some(index) = state.session_for_surface(&surface) {
                    state.routed_wayland_touches.insert(id, index);
                    state.touch_down(qh, index, id, time, x, y);
                }
            }
            wl_touch::Event::Motion { time, id, x, y } => {
                if let Some(index) = state.routed_wayland_touches.get(&id).copied() {
                    state.touch_motion(qh, index, id, time, x, y);
                }
            }
            wl_touch::Event::Up { time, id, .. } => {
                if let Some(index) = state.routed_wayland_touches.remove(&id) {
                    state.touch_up(qh, index, id, time);
                }
            }
            wl_touch::Event::Cancel => {
                state.touch_cancel_all(qh);
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
                let Some(index) = state.session_for_layer_surface(layer_surface) else {
                    return;
                };
                state.sessions[index]
                    .overlay
                    .ack_configure(layer_surface, serial, width, height);
                state.sessions[index].capture_policy =
                    state.sessions[index].engine.capture_policy(&state.config);
                if let Err(err) = state.attach_overlay_buffer(qh, index, width, height) {
                    eprintln!("touchdeck: failed to attach overlay buffer: {err:?}");
                    state.running = false;
                    return;
                }
                let capture_policy = state.sessions[index].capture_policy.clone();
                if let Err(err) = state.apply_input_region(qh, index, &capture_policy) {
                    eprintln!("touchdeck: failed to apply input region: {err:?}");
                    state.running = false;
                    return;
                }
                if let Err(err) = state.sync_raw_touch_grab(index, &capture_policy) {
                    eprintln!("touchdeck: failed to sync raw touch grab: {err:?}");
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
