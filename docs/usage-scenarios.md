# TouchDeck usage scenarios

This document describes which TouchDeck path to use for common app/runtime
combinations. The main decision is not "Chinese input or not"; it is which input
frontend the focused application can actually talk to.

## Components

TouchDeck currently has three relevant input paths:

- `touchdeck` overlay: captures touch, resolves slots/gestures/modes, sends keys/actions.
- embedded touchdeck-ime runtime: librime-backed IME handling physical keyboard, TouchDeck keys, XIM, and fcitx D-Bus compatibility.
- focused app frontend: Wayland text-input/input-method, XIM, fcitx D-Bus module, or raw key input.

The usual Rime setup runs one process:

```sh
cd /home/disk/Projects/touchdeck

TOUCHDECK_CONFIG=$PWD/touchdeck.example.toml \
cargo run --release --bin touchdeck
```

Do not run another service that owns the same IM frontend name at the same time.
In particular, TouchDeck's fcitx compatibility layer owns
`org.fcitx.Fcitx5` and `org.freedesktop.portal.Fcitx`.

## Decision table

| Scenario | App frontend | TouchDeck config | Candidate/preedit UI |
| --- | --- | --- | --- |
| Phone portrait streaming, TouchDeck keyboard | in-process channel to embedded IME | `[keyboard] output = "ime"` | TouchDeck overlay |
| Native Wayland app with physical keyboard | Wayland text-input/input-method | run `touchdeck` with `output = "ime"` | native `input_popup_surface_v2` |
| Raw key passthrough only | focused app key events | `TOUCHDECK_TEXT_OUTPUT=virtual-keyboard` | app/toolkit only |
| XWayland app with XIM support | XIM | `TOUCHDECK_IME_XIM=1`, `XMODIFIERS=@im=touchdeck` | TouchDeck server-side popup |
| Qt/fcitx-compatible app such as some WeChat surfaces | fcitx D-Bus frontend | `TOUCHDECK_IME_FCITX_DBUS=1`, app uses `fcitx` IM module | client-side if app supports it, otherwise TouchDeck popup |
| PGTK Emacs with IM context disabled | no usable IM frontend | raw key fallback only | no Rime preedit/commit path |

## Scenario: phone portrait streaming

Use this for Moonlight/Sunshine phone streaming where TouchDeck owns the touch
keyboard and niri gestures.

```sh
TOUCHDECK_CONFIG=$PWD/touchdeck.example.toml \
TOUCHDECK_TEXT_OUTPUT=ime \
cargo run --release --bin touchdeck
```

Expected behavior:

- touch keys go to Rime through the embedded IME runtime;
- niri actions use niri IPC directly;
- candidate UI for touch input is drawn by the TouchDeck overlay;
- `passthrough` mode is explicit, not assumed for every mode.

Use `route = "ime-key"` for normal text keys and cursor/candidate keys. Use
`route = "app-key"` only for keys that must bypass Rime.

## Scenario: native Wayland apps and physical keyboard

Use this when you want TouchDeck's embedded IME to replace fcitx5 for physical keyboard
input in apps that support Wayland text-input/input-method.

```sh
TOUCHDECK_CONFIG=$PWD/touchdeck.example.toml \
cargo run --release --bin touchdeck
```

Expected behavior:

- physical keyboard events enter the embedded IME through input-method-v2 keyboard grab;
- Rime preedit/candidates use native `input_popup_surface_v2`;
- commit/preedit are delivered through Wayland input-method protocol.

Avoid forcing XIM or fcitx modules for native Wayland apps unless the app's
native Wayland IM path is known broken. If a toolkit or app disables its IM
context, TouchDeck cannot deliver Rime preedit/commit through that native path.

Known case:

- Emacs PGTK with `(setopt pgtk-use-im-context-on-new-connection nil)` disables
  the GTK IM context. In that configuration TouchDeck can still send raw keys,
  but the standard Wayland IM path is unavailable.

## Scenario: XWayland/XIM apps

Use this for XWayland clients that use XIM directly.

Start TouchDeck with XIM enabled:

```sh
TOUCHDECK_IME_XIM=1 \
TOUCHDECK_CONFIG=$PWD/touchdeck.example.toml \
cargo run --release --bin touchdeck
```

Start the target app with:

```sh
XMODIFIERS=@im=touchdeck app-command
```

Expected behavior:

- key events arrive through XIM `ForwardEvent`;
- TouchDeck sends preedit/commit through XIM;
- candidate status is published to `touchdeck`;
- the main TouchDeck overlay draws the server-side candidate popup near the X11
  cursor rect mapped into niri output coordinates.

Debug with:

```sh
TOUCHDECK_LOG_XIM=1
TOUCHDECK_LOG_IME_GEOMETRY=1
```

If preedit appears but positioning is wrong, inspect the `xim surface geometry`
and `touchdeck: ime geometry` logs. If commit works but preedit does not appear,
check which XIM style the app selected.

## Scenario: Qt/fcitx-compatible apps

Some Qt/XWayland surfaces do not use XIM for all text fields but do use the
fcitx D-Bus frontend. This is common in mixed or embedded UI surfaces.

Start TouchDeck with fcitx compatibility enabled:

```sh
TOUCHDECK_IME_FCITX_DBUS=1 \
TOUCHDECK_CONFIG=$PWD/touchdeck.example.toml \
cargo run --release --bin touchdeck
```

Start the app with an fcitx IM module, for example:

```sh
QT_IM_MODULE=fcitx \
QT_IM_MODULES=fcitx \
app-command
```

Expected behavior:

- the app connects to TouchDeck's fcitx-compatible D-Bus service;
- client-side input panels are respected when the app advertises support;
- if the app does not provide a client-side input panel, TouchDeck draws a
  server-side candidate popup.

Do not run fcitx5 itself at the same time on the same session bus. This mode is a
compatibility layer for apps expecting fcitx, not a bridge to a separate fcitx5
daemon.

## Scenario: raw key passthrough

Use this when you want only key events and no Rime integration.

```sh
TOUCHDECK_CONFIG=$PWD/touchdeck.example.toml \
TOUCHDECK_TEXT_OUTPUT=virtual-keyboard \
cargo run --release --bin touchdeck
```

Expected behavior:

- the embedded IME runtime is not started;
- keys are sent through Wayland virtual-keyboard;
- input methods may be bypassed depending on compositor/client routing;
- Shift or other IM trigger keys are just key events, not Rime control.

This is useful for app shortcuts, games, and simple English-only input. It is not
the preferred path for Chinese input.

## Rime data

Default directories:

- shared data: `/usr/share/rime-data`
- user data: `$XDG_DATA_HOME/touchdeck/rime`
- fallback user data: `~/.local/share/touchdeck/rime`

To reuse an existing fcitx5-rime user config:

```sh
TOUCHDECK_RIME_USER_DATA_DIR=/home/_WD_/.local/share/fcitx5/rime \
TOUCHDECK_CONFIG=$PWD/touchdeck.example.toml \
cargo run --release --bin touchdeck
```

TouchDeck intentionally uses `/usr/share/rime-data` as shared data. Do not copy
the whole shared data tree into the user directory unless you know why.

If a schema uses Lua processors/translators/filters, the linked librime must have
the Lua plugin stack. Logs like `error creating processor: 'lua_processor'` mean
the Rime plugin build is incomplete.

## Debug flags

TouchDeck overlay:

```sh
TOUCHDECK_DEBUG_DRAW=1
TOUCHDECK_DEBUG_ALPHA=64
TOUCHDECK_LOG_TOUCH=1
TOUCHDECK_LOG_IME_GEOMETRY=1
```

TouchDeck IME:

```sh
TOUCHDECK_LOG_XIM=1
TOUCHDECK_RIME_LOG_LEVEL=0
TOUCHDECK_RIME_LOG_DIR=/tmp/touchdeck-rime-log
```

Use `TOUCHDECK_LOG_XIM=1` only while debugging. It logs key/preedit contents and
is too noisy for daily use.

## Common failure modes

- App receives English letters instead of Rime preedit: app is probably not using
  a supported IM frontend, or TouchDeck is running with `virtual-keyboard`.
- XWayland commit works but candidates are missing: check whether the app uses
  XIM or fcitx D-Bus; configure the matching frontend.
- Candidate popup is duplicated: app is drawing a client-side panel while
  TouchDeck also thinks it owns server-side UI. Check `client_side_input_panel`
  logs and display kind.
- Cursor/candidate position is wrong only when panels/bars move: check layer-shell
  exclusive-zone interactions and `TOUCHDECK_LOG_IME_GEOMETRY=1`.
- Shift punctuation is wrong in Rime: choose `key_translation = "effective"` or
  override a binding with `&kpe` / `&kpr`.
- Deletion/cursor keys edit the app instead of preedit: keep route as `ime-key`
  or `ime-only`; do not use `app-key` for candidate editing keys.
