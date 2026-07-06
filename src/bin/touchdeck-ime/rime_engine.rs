use std::env;
use std::ffi::{CStr, CString};
use std::fs;
use std::os::raw::{c_char, c_int};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};

use anyhow::{anyhow, Context, Result};
use touchdeck::protocol::{ImeCandidate, ImeStatus};
use touchdeck::rime::*;

use super::config::KeyTranslationPolicy;
use super::key::{
    rime_effective_keysym, rime_modifier_mask, KeyState, RIME_RELEASE_MASK,
};

const RIME_FALSE: c_int = 0;
const RIME_MODULE_DEFAULT: &[u8] = b"default\0";
const RIME_MODULE_PLUGINS: &[u8] = b"plugins\0";

#[derive(Debug, Default)]
pub(super) struct RimeOutput {
    pub(super) handled: bool,
    pub(super) commit: Option<String>,
    pub(super) status: ImeStatus,
}

pub(super) struct RimeEngine {
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
    pub(super) fn new(key_translation: KeyTranslationPolicy) -> Result<Self> {
        let shared_data_dir_path = default_rime_shared_data_dir();
        let user_data_dir_path =
            env_path("TOUCHDECK_RIME_USER_DATA_DIR").unwrap_or_else(default_rime_user_data_dir);
        let prebuilt_data_dir_path = shared_data_dir_path.join("build");
        let staging_dir_path = user_data_dir_path.join("build");

        if !shared_data_dir_path.join("default.yaml").exists() {
            return Err(anyhow!(
                "Rime shared data dir {} does not contain default.yaml",
                shared_data_dir_path.display()
            ));
        }

        fs::create_dir_all(&user_data_dir_path).with_context(|| {
            format!("create Rime user data dir {}", user_data_dir_path.display())
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

    pub(super) fn process_key(
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

    pub(super) fn clear(&mut self) {
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
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn path_to_cstring(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("path contains NUL: {}", path.display()))
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
