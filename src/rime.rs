use std::os::raw::{c_char, c_int, c_void};

use anyhow::{anyhow, Result};

pub type Bool = c_int;
pub type RimeSessionId = usize;

#[repr(C)]
pub struct RimeTraits {
    pub data_size: c_int,
    pub shared_data_dir: *const c_char,
    pub user_data_dir: *const c_char,
    pub distribution_name: *const c_char,
    pub distribution_code_name: *const c_char,
    pub distribution_version: *const c_char,
    pub app_name: *const c_char,
    pub modules: *const *const c_char,
    min_log_level: c_int,
    log_dir: *const c_char,
    prebuilt_data_dir: *const c_char,
    staging_dir: *const c_char,
}

#[repr(C)]
pub struct RimeComposition {
    pub length: c_int,
    pub cursor_pos: c_int,
    pub sel_start: c_int,
    pub sel_end: c_int,
    pub preedit: *mut c_char,
}

#[repr(C)]
pub struct RimeCandidate {
    pub text: *mut c_char,
    pub comment: *mut c_char,
    reserved: *mut c_void,
}

#[repr(C)]
pub struct RimeMenu {
    pub page_size: c_int,
    pub page_no: c_int,
    pub is_last_page: Bool,
    pub highlighted_candidate_index: c_int,
    pub num_candidates: c_int,
    pub candidates: *mut RimeCandidate,
    select_keys: *mut c_char,
}

#[repr(C)]
pub struct RimeCommit {
    pub data_size: c_int,
    pub text: *mut c_char,
}

#[repr(C)]
pub struct RimeContext {
    pub data_size: c_int,
    pub composition: RimeComposition,
    pub menu: RimeMenu,
    pub commit_text_preview: *mut c_char,
    select_labels: *mut *mut c_char,
}

#[repr(C)]
pub struct RimeApi {
    pub data_size: c_int,
    pub setup: Option<unsafe extern "C" fn(*mut RimeTraits)>,
    set_notification_handler: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    pub initialize: Option<unsafe extern "C" fn(*mut RimeTraits)>,
    pub finalize: Option<unsafe extern "C" fn()>,
    pub start_maintenance: Option<unsafe extern "C" fn(Bool) -> Bool>,
    is_maintenance_mode: Option<unsafe extern "C" fn() -> Bool>,
    pub join_maintenance_thread: Option<unsafe extern "C" fn()>,
    deployer_initialize: Option<unsafe extern "C" fn(*mut RimeTraits)>,
    prebuild: Option<unsafe extern "C" fn() -> Bool>,
    pub deploy: Option<unsafe extern "C" fn() -> Bool>,
    deploy_schema: Option<unsafe extern "C" fn(*const c_char) -> Bool>,
    deploy_config_file: Option<unsafe extern "C" fn(*const c_char, *const c_char) -> Bool>,
    sync_user_data: Option<unsafe extern "C" fn() -> Bool>,
    pub create_session: Option<unsafe extern "C" fn() -> RimeSessionId>,
    find_session: Option<unsafe extern "C" fn(RimeSessionId) -> Bool>,
    pub destroy_session: Option<unsafe extern "C" fn(RimeSessionId) -> Bool>,
    cleanup_stale_sessions: Option<unsafe extern "C" fn()>,
    cleanup_all_sessions: Option<unsafe extern "C" fn()>,
    pub process_key: Option<unsafe extern "C" fn(RimeSessionId, c_int, c_int) -> Bool>,
    commit_composition: Option<unsafe extern "C" fn(RimeSessionId) -> Bool>,
    clear_composition: Option<unsafe extern "C" fn(RimeSessionId)>,
    pub get_commit: Option<unsafe extern "C" fn(RimeSessionId, *mut RimeCommit) -> Bool>,
    pub free_commit: Option<unsafe extern "C" fn(*mut RimeCommit) -> Bool>,
    pub get_context: Option<unsafe extern "C" fn(RimeSessionId, *mut RimeContext) -> Bool>,
    pub free_context: Option<unsafe extern "C" fn(*mut RimeContext) -> Bool>,
    get_status: Option<unsafe extern "C" fn(RimeSessionId, *mut c_void) -> Bool>,
    free_status: Option<unsafe extern "C" fn(*mut c_void) -> Bool>,
    pub set_option: Option<unsafe extern "C" fn(RimeSessionId, *const c_char, Bool)>,
    get_option: Option<unsafe extern "C" fn(RimeSessionId, *const c_char) -> Bool>,
}

#[link(name = "rime")]
unsafe extern "C" {
    fn rime_get_api() -> *mut RimeApi;
}

pub fn rime_traits_data_size() -> c_int {
    (std::mem::size_of::<RimeTraits>() - std::mem::size_of::<c_int>()) as c_int
}

pub fn rime_commit_data_size() -> c_int {
    (std::mem::size_of::<RimeCommit>() - std::mem::size_of::<c_int>()) as c_int
}

pub fn empty_rime_context() -> RimeContext {
    RimeContext {
        pub data_size: (std::mem::size_of::<RimeContext>() - std::mem::size_of::<c_int>()) as c_int,
        pub composition: RimeComposition {
            pub length: 0,
            pub cursor_pos: 0,
            pub sel_start: 0,
            pub sel_end: 0,
            pub preedit: ptr::null_mut(),
        },
        pub menu: RimeMenu {
            pub page_size: 0,
            pub page_no: 0,
            pub is_last_page: RIME_FALSE,
            pub highlighted_candidate_index: 0,
            pub num_candidates: 0,
            pub candidates: ptr::null_mut(),
            select_keys: ptr::null_mut(),
        },
        pub commit_text_preview: ptr::null_mut(),
        select_labels: ptr::null_mut(),
    }
}

pub fn call_void<T>(func: Option<T>, name: &'static str) -> Result<T> {
    func.ok_or_else(|| anyhow!("{name} unavailable"))
}

pub fn call_ret<T>(func: Option<T>, name: &'static str) -> Result<T> {
    func.ok_or_else(|| anyhow!("{name} unavailable"))
}
