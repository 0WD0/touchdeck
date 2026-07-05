use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImeCandidate {
    pub label: String,
    pub text: String,
    pub comment: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImeCursorRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    #[serde(default = "default_cursor_scale")]
    pub scale: f64,
    #[serde(default = "default_cursor_space")]
    pub space: String,
    #[serde(default)]
    pub window_x: Option<i32>,
    #[serde(default)]
    pub window_y: Option<i32>,
    #[serde(default)]
    pub window_w: Option<i32>,
    #[serde(default)]
    pub window_h: Option<i32>,
    #[serde(default)]
    pub root_w: Option<i32>,
    #[serde(default)]
    pub root_h: Option<i32>,
}

impl Default for ImeCursorRect {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            scale: 1.0,
            space: "surface".to_string(),
            window_x: None,
            window_y: None,
            window_w: None,
            window_h: None,
            root_w: None,
            root_h: None,
        }
    }
}

fn default_cursor_scale() -> f64 {
    1.0
}

fn default_cursor_space() -> String {
    "surface".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImeStatus {
    pub protocol: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub display_kind: String,
    #[serde(default)]
    pub ui_owner: String,
    pub active: bool,
    #[serde(default)]
    pub client_side_input_panel: bool,
    #[serde(default)]
    pub cursor_rect: Option<ImeCursorRect>,
    pub preedit: String,
    pub commit_preview: String,
    pub candidates: Vec<ImeCandidate>,
    pub highlighted_candidate_index: Option<usize>,
    pub page_no: i32,
    pub is_last_page: bool,
}

impl Default for ImeStatus {
    fn default() -> Self {
        Self {
            protocol: "touchdeck-ime-v1".to_string(),
            kind: "status".to_string(),
            source: "unknown".to_string(),
            display_kind: "unknown".to_string(),
            ui_owner: "none".to_string(),
            active: false,
            client_side_input_panel: false,
            cursor_rect: None,
            preedit: String::new(),
            commit_preview: String::new(),
            candidates: Vec::new(),
            highlighted_candidate_index: None,
            page_no: 0,
            is_last_page: true,
        }
    }
}
