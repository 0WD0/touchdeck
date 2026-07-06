use std::fs::File;
use std::io::Read;
use std::os::fd::OwnedFd;

use anyhow::{Context, Result};
use xkbcommon::xkb;

const EVDEV_XKB_OFFSET: u32 = 8;

pub(super) struct PhysicalKeyboard {
    _context: xkb::Context,
    _keymap: xkb::Keymap,
    state: xkb::State,
}

impl PhysicalKeyboard {
    pub(super) fn from_keymap_fd(fd: OwnedFd, size: u32) -> Result<Self> {
        let mut bytes = Vec::with_capacity(size as usize);
        File::from(fd)
            .take(u64::from(size))
            .read_to_end(&mut bytes)
            .context("read input-method keyboard keymap")?;
        let keymap = String::from_utf8(bytes).context("input-method keymap is not UTF-8")?;
        Self::from_keymap_string(keymap)
    }

    fn from_keymap_string(keymap: String) -> Result<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_string(
            &context,
            keymap,
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .context("compile input-method keyboard keymap")?;
        let state = xkb::State::new(&keymap);
        Ok(Self {
            _context: context,
            _keymap: keymap,
            state,
        })
    }

    pub(super) fn update_modifiers(
        &mut self,
        mods_depressed: u32,
        mods_latched: u32,
        mods_locked: u32,
        group: u32,
    ) {
        self.state
            .update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
    }

    pub(super) fn keysym_for_evdev_key(&self, key: u32) -> Option<u32> {
        let keycode = xkb::Keycode::new(key.checked_add(EVDEV_XKB_OFFSET)?);
        let keysym = self.state.key_get_one_sym(keycode).raw();
        (keysym != 0).then_some(keysym)
    }
}
