#![allow(clippy::too_many_arguments)]

use std::sync::{
    atomic::{AtomicU32, Ordering},
    mpsc::{self, Receiver, Sender},
    Arc,
};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use touchdeck::protocol::ImeStatus;
use touchdeck::x11_geometry::X11WindowGeometry;
use zbus::names::OwnedBusName;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Structure, Value};
use zbus::{interface, message::Header, object_server::SignalEmitter, ObjectServer};

const FCITX_INPUT_CONTEXT_INTERFACE: &str = "org.fcitx.Fcitx.InputContext1";
const FCITX_BATCHED_COMMIT_STRING: u32 = 0;
const FCITX_BATCHED_PREEDIT: u32 = 1;

pub(super) const FCITX_CAPABILITY_CLIENT_SIDE_INPUT_PANEL: u64 = 1 << 39;

pub(super) enum FcitxDbusRequest {
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
pub(super) struct FcitxDbusTarget {
    pub(super) path: OwnedObjectPath,
    pub(super) client: OwnedBusName,
    pub(super) display: String,
}

impl FcitxDbusTarget {
    pub(super) fn matches(&self, other: &Self) -> bool {
        self.path.as_str() == other.path.as_str() && self.client.as_str() == other.client.as_str()
    }
}

#[derive(Debug)]
pub(super) struct FcitxDbusOutput {
    pub(super) target: FcitxDbusTarget,
    pub(super) preedit: Option<String>,
    pub(super) commit: Option<String>,
    pub(super) status: ImeStatus,
    pub(super) cursor_rect: Option<FcitxCursorRect>,
}

#[derive(Clone, Debug)]
pub(super) struct FcitxCursorRect {
    pub(super) target: FcitxDbusTarget,
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) w: i32,
    pub(super) h: i32,
    pub(super) scale: f64,
    pub(super) space: String,
    pub(super) x11_window: Option<X11WindowGeometry>,
}

#[derive(Debug, Default)]
pub(super) struct FcitxDbusKeyResponse {
    pub(super) handled: bool,
    pub(super) preedit: String,
    pub(super) commit: Option<String>,
    pub(super) status: ImeStatus,
}

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

#[allow(clippy::too_many_arguments)]
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
    fn set_cursor_rect(&self, x: i32, y: i32, w: i32, h: i32, #[zbus(header)] header: Header<'_>) {
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
    async fn commit_string_signal(emitter: &SignalEmitter<'_>, str: &str) -> zbus::Result<()>;

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

pub(super) fn spawn_fcitx_dbus_server(
    tx: Sender<FcitxDbusRequest>,
) -> Sender<FcitxDbusOutput> {
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
        .serve_at("/org/freedesktop/portal/inputmethod", input_method.clone())
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
