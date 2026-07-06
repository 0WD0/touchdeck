# touchdeck

Host-side Wayland touch input layer for niri, optimized for phone portrait streaming.

TouchDeck is not a traditional on-screen keyboard window. It is a programmable
Wayland input layer:

```text
wl_touch events
  -> slot hit testing from SVG layout
  -> gesture recognition
  -> mode/layer keymap resolution
  -> behavior dispatch
  -> niri IPC / virtual keyboard / embedded touchdeck-ime runtime
```

The normal runtime is a single binary:

- `touchdeck`: the fullscreen layer-shell overlay, gesture engine, keymap engine, niri dispatcher, key sender, and embedded librime IME runtime.

For application-specific setup, see [Usage scenarios](docs/usage-scenarios.md).

## Quick start

Run TouchDeck:

```sh
cd /home/disk/Projects/touchdeck
TOUCHDECK_CONFIG=$PWD/touchdeck.example.toml cargo run --release --bin touchdeck
```

Useful debug run:

```sh
cd /home/disk/Projects/touchdeck
TOUCHDECK_CONFIG=$PWD/touchdeck.example.toml \
TOUCHDECK_DEBUG_DRAW=1 \
TOUCHDECK_LOG_TOUCH=1 \
cargo run --release --bin touchdeck
```

If you only want raw Wayland virtual-keyboard output and do not want Rime:

```sh
TOUCHDECK_CONFIG=$PWD/touchdeck.example.toml \
TOUCHDECK_TEXT_OUTPUT=virtual-keyboard \
cargo run --release --bin touchdeck
```

## Current design rules

TouchDeck is in rapid iteration. The runtime intentionally does not keep legacy
fallback layouts or fallback keymaps in `main.rs`.

The default usable setup lives in files:

- `touchdeck.example.toml`: default profile, keymap, IME settings, popup theme.
- `layouts/phone-portrait.svg`: default phone portrait slot layout.
- `examples/layouts/keymaps/*.toml`: alternative keymap examples.

A config file should describe behavior. SVG should describe geometry. Source code
should not contain named slot/keymap defaults.

## Runtime configuration lookup

TouchDeck loads config in this order:

```text
TOUCHDECK_CONFIG
  -> ./touchdeck.toml if present
  -> empty runtime config
```

For real use, set `TOUCHDECK_CONFIG` explicitly.

Important environment overrides:

- `TOUCHDECK_CONFIG`: path to TOML config.
- `TOUCHDECK_TOUCH_BACKEND`: `wayland` or `evdev`.
- `TOUCHDECK_TOUCH_DEVICE`: `/dev/input/event*` or `/dev/input/by-id/...` path for `evdev`.
- `TOUCHDECK_TOUCH_DEVICE_NAME`: device-name substring for evdev auto-discovery.
- `TOUCHDECK_SUNSHINE_OUTPUT`: match Sunshine's `[sunshine-output=...]` device tag.
- `TOUCHDECK_TOUCH_GRAB=0|1`: whether the evdev backend grabs the touch device.
- `TOUCHDECK_TEXT_OUTPUT`: `virtual-keyboard`, `ime`, or `both`.
- `TOUCHDECK_XKB_KEYMAP`: raw XKB keymap file for virtual keyboard output.
- `TOUCHDECK_DEBUG_DRAW=1`: draw overlay debug/keycap UI.
- `TOUCHDECK_DEBUG_ALPHA=0..255`: overlay background alpha.
- `TOUCHDECK_LOG_TOUCH=1`: log raw touch events.
- `TOUCHDECK_REPEAT_START_MS`: global first-repeat delay fallback.
- `TOUCHDECK_REPEAT_INTERVAL_MS`: global repeat interval fallback.

## Modes and layers

Modes decide how touch input is interpreted. Layers decide which bindings are
active within a mode.

Current modes:

- `base`: normal programmable touch layer.
- `text`: text keyboard / key output mode.
- `niri_momentary`: niri control while a momentary trigger is held.
- `niri_locked`: niri control until explicitly unlocked.
- `passthrough`: only active binding targets are captured; other areas pass through to apps.

Current layers:

- `base`
- `niri`

Layer resolution checks the top layer first, then lower layers. `layer_momentary`
pushes a layer and restores the previous stack on release. `layer_toggle` pushes
an inactive layer or removes an active layer.

`passthrough` is explicit. Central passthrough is not assumed in every mode.

## Layout SVG

Layout geometry is described by SVG. TOML references slots by ID.

```toml
[layout]
svg = "layouts/phone-portrait.svg"
```

SVG v1 supports `<rect>` slots with these attributes:

- `data-td-slot`: required stable slot ID.
- `data-td-role`: optional `zone`, `key`, `gesture-area`, or decoration role.
- `data-td-capture`: optional boolean, defaults to `true`.
- `data-td-label`: optional display/debug label.

Example:

```xml
<svg viewBox="0 0 1000 2400" xmlns="http://www.w3.org/2000/svg">
  <rect
    data-td-slot="thumb_spc"
    data-td-role="key"
    data-td-capture="true"
    data-td-label="SPC"
    x="300"
    y="2200"
    width="360"
    height="150" />
</svg>
```

The SVG `viewBox` is normalized to the current surface size. Currently supported
hit/capture geometry is rectangular. `path`, `circle`, transforms, and group-level
inheritance are not supported yet.

## Keymap structure

Keyboard maps are generated from existing SVG slot IDs:

```toml
[[keyboard.layers]]
mode = "text"
layer = "base"
repeat_start_ms = 520
repeat_interval_ms = 55

[keyboard.layers.tap]
key_q = "&kp Q"
thumb_spc = "&kp SPC"

[keyboard.layers.swipe_left]
key_h = "&hold_repeat LEFT"
thumb_spc = "&hold_repeat BSPC"
```

`[[keyboard.layers]]` fields:

- `mode`: defaults to `text`.
- `layer`: defaults to `base`.
- `tap`: slot ID to behavior table.
- `hold`: slot ID to behavior table evaluated after hold threshold.
- `repeat`: slot ID to fixed hold-repeat table.
- `swipe_up`, `swipe_down`, `swipe_left`, `swipe_right`: slot ID to behavior table.
- `fingers`: defaults to `1`.
- `max_ms`: optional tap/swipe time limit.
- `hold_ms`: optional hold threshold.
- `repeat_start_ms`: first repeat delay for `&hold_repeat` in this layer.
- `repeat_interval_ms`: later repeat interval for `&hold_repeat` in this layer.
- `min_px`: optional swipe distance threshold.
- `priority`: defaults to `0`.
- `consume`: defaults to `true`.

The same slot may have tap, hold, and directional swipe bindings. Text keycaps
render gesture hints:

- center: tap
- top edge: swipe up
- bottom edge: swipe down
- left edge: swipe left
- right edge: swipe right
- small lower-left hint: hold

## Behavior invocation syntax

TouchDeck uses a ZMK-inspired behavior invocation syntax.

Common builtins:

- `&kp KEY`: tap a key or key chord.
- `&kpe KEY`: tap with effective Rime keysym translation.
- `&kpr KEY`: tap with raw Rime keysym translation.
- `&ik KEY`: route as `ime-key`.
- `&ak KEY`: route as `app-key`.
- `&it KEY`: route as `ime-text`.
- `&io KEY`: route as `ime-only`.
- `&hold KEY`: press key on hold, release when touch is released.
- `&key_repeat`: repeat the previous key sequence.
- `&hold_repeat KEY`: immediately send key once, then repeat while held.
- `&trans`: fall through to lower active layer.
- `&none`: consume without action.

Examples:

```toml
key_a = "&kp A"
key_h = "&hold_repeat LEFT"
key_slash = "&kpe QUESTION"
thumb_ret = "&kp LC(RET)"
```

Named behaviors can define richer options:

```toml
[behaviors.cursor_left]
type = "hold_repeat"
keys = "LEFT"
start_ms = 650
interval_ms = 70
route = "ime-key"

[keyboard.layers.swipe_down]
key_h = "&cursor_left"
```

## Key syntax

Key syntax supports a ZMK-style subset:

- Letters: `A` through `Z`.
- Numbers: `N1` through `N0`.
- Common keys: `RET`, `SPC`, `TAB`, `ESC`, `BSPC`, `DEL`, `LEFT`, `RIGHT`, `UP`, `DOWN`, `HOME`, `END`, `PAGE_UP`, `PAGE_DOWN`.
- Modifiers: `LCTRL`, `LSHIFT`, `LALT`, `LGUI`, `RCTRL`, `RSHIFT`, `RALT`, `RGUI`.
- Modifier wrappers: `LC(...)`, `LS(...)`, `LA(...)`, `LG(...)`, `RC(...)`, `RS(...)`, `RA(...)`, `RG(...)`.
- US punctuation: `MINUS`, `EQUAL`, `LEFT_BRACKET`, `RIGHT_BRACKET`, `SEMICOLON`, `SINGLE_QUOTE`, `GRAVE`, `BACKSLASH`, `COMMA`, `PERIOD`, `SLASH`.
- Shifted punctuation: `EXCLAMATION`, `AT_SIGN`, `HASH`, `DOLLAR`, `PERCENT`, `CARET`, `AMPERSAND`, `ASTERISK`, `UNDERSCORE`, `QUESTION`.

Examples:

```text
LC(C)
LC(X) LC(S)
LS(SLASH)
LC(LEFT)
```

## Hold repeat timing

`&hold_repeat KEY` has two intervals:

```text
cross threshold / hold trigger
  -> send KEY once immediately
  -> wait start_ms
  -> repeat every interval_ms until release
```

Defaults come from:

```text
behavior start_ms / interval_ms
  -> keyboard layer repeat_start_ms / repeat_interval_ms
  -> TOUCHDECK_REPEAT_START_MS / TOUCHDECK_REPEAT_INTERVAL_MS
  -> built-in fallback values
```

Default profile values are intentionally conservative for phone swipes:

```toml
repeat_start_ms = 520
repeat_interval_ms = 55
```

This makes a short swipe produce one movement, while a deliberate hold starts
continuous movement later.

## Touch input backends

The default backend is `wayland`, which receives `wl_touch` through the
fullscreen layer-shell overlay. In this mode the overlay input region follows
the current capture policy, so pointer buttons and scroll wheels inside that
region also hit TouchDeck first.

For daily use with a separate mouse or touchpad, use the `evdev` backend:

```toml
[input]
backend = "evdev"
sunshine_output = "DP-2"
grab = true
```

In `evdev` mode the Wayland overlay is display-only and always uses an empty
input region. TouchDeck reads and optionally grabs only the configured
touchscreen event node; mouse buttons, pointer motion, and scroll wheels remain
routed by the compositor.

With the Sunshine fork, TouchDeck can auto-discover the per-client virtual
touchscreen created by inputtino. Sunshine names it like:

```text
Touch passthrough [sunshine-output=DP-2]
```

This is the same output tag that the niri fork uses to map virtual absolute
devices to the correct output and to avoid applying output rotation twice.
If there is only one matching Sunshine touch device, `touch_device` can be
omitted. If there are several, set `sunshine_output` or `touch_device`.

The evdev backend supports hotplug. If TouchDeck starts before Sunshine creates
the per-client touchscreen, it will keep running and retry discovery until the
device appears. If the streaming session ends and the event node disappears,
TouchDeck releases its current touch state and waits for the next matching
device.

## Text output backends

Configured in `[keyboard]` or `[ime]`:

```toml
[keyboard]
output = "ime"
```

Supported values:

- `virtual-keyboard`: send `zwp_virtual_keyboard_v1` events directly to the focused app.
- `ime`: send key events to the embedded touchdeck-ime runtime.
- `both`: send to both, useful only while debugging.

`virtual-keyboard` is simple but may bypass input methods depending on compositor
routing. `ime` is the preferred path for Rime input.

## Embedded IME and Rime

The embedded touchdeck-ime runtime is a minimal Wayland input-method-v2 frontend
backed by librime.
It handles two input sources:

- Physical keyboard events from input-method keyboard grab.
- TouchDeck key events sent over an in-process channel from the overlay.

For native Wayland input-method usage, it can show preedit/candidates through
`input_popup_surface_v2`. For fcitx-compatible Xwayland clients, it publishes
cursor/candidate status and lets the main TouchDeck overlay render the
server-side candidate popup.

Rime directories:

- Shared data dir is currently `/usr/share/rime-data`.
- User data dir defaults to `$XDG_DATA_HOME/touchdeck/rime`.
- If `XDG_DATA_HOME` is unset, user data defaults to `~/.local/share/touchdeck/rime`.
- Override user data with `TOUCHDECK_RIME_USER_DATA_DIR`.

Example using an existing fcitx5-rime user directory:

```sh
TOUCHDECK_RIME_USER_DATA_DIR=/home/_WD_/.local/share/fcitx5/rime \
TOUCHDECK_CONFIG=$PWD/touchdeck.example.toml \
cargo run --release --bin touchdeck
```

Useful Rime environment variables:

- `TOUCHDECK_RIME_USER_DATA_DIR`: user data directory.
- `TOUCHDECK_RIME_DEPLOY=0`: skip deployment on startup.
- `TOUCHDECK_RIME_LOG_DIR`: librime log directory.
- `TOUCHDECK_RIME_LOG_LEVEL`: librime min log level.

If your schema uses Lua processors/translators/filters, librime must be built
with the corresponding plugin support. Errors such as `error creating processor:
'lua_processor'` mean the Rime plugin stack is missing, not that the keymap is
wrong.

## IME key route model

Each key behavior can choose how it interacts with Rime and the focused app.
Default route is `ime-key`.

Routes:

- `ime-key`: send to Rime first. If Rime handles the key, consume it. If Rime does not handle it, forward the original key to the app.
- `ime-only`: send to Rime first. If Rime does not handle it, consume it anyway.
- `app-key`: bypass Rime and forward directly to the focused app.
- `ime-text`: commit printable text through text-input; Ctrl/Alt/Super combinations are forwarded as keys.

Examples:

```toml
[behaviors.cursor_left]
type = "hold_repeat"
keys = "LEFT"
route = "ime-key"

[behaviors.raw_enter]
type = "key"
keys = "RET"
route = "app-key"

[behaviors.literal_a]
type = "key"
keys = "A"
route = "ime-text"
```

Inline aliases:

```toml
key_h = "&ik LEFT"  # Rime first, app fallback
key_l = "&ak RIGHT" # app direct
key_a = "&it A"     # text commit
key_x = "&io ESC"   # Rime only
```

Use `ime-key` for normal text keyboard keys and cursor keys. It matches the
fcitx model: `process_key` decides whether Rime consumed the key; unhandled keys
are forwarded rather than guessed from preedit state.

## Rime key translation

`translation` controls what keysym is sent to Rime when modifiers are active.

Configured globally:

```toml
[ime]
key_translation = "effective"
```

Per behavior:

```toml
key_slash = "&kpr SLASH"      # raw / with Shift modifier if held
key_question = "&kpe QUESTION" # effective ?
```

Policies:

- `effective`: translate shifted printable keys before calling Rime. `Shift+/` can become `?`.
- `raw`: send the raw key symbol plus modifier mask. `Shift+/` stays `/` with Shift.

## niri IPC

niri actions are sent directly to `$NIRI_SOCKET` as JSON IPC. TouchDeck does not
run `niri msg` and does not keep a command fallback.

Supported configured actions currently include:

- `focus-column-left`
- `focus-column-right`
- `focus-workspace-up`
- `focus-workspace-down`
- `toggle-overview`

The default phone portrait config maps niri portrait movement ergonomically, not
as a literal desktop left/right mapping.

## Debug and troubleshooting

Useful flags:

```sh
TOUCHDECK_DEBUG_DRAW=1
TOUCHDECK_DEBUG_ALPHA=64
TOUCHDECK_LOG_TOUCH=1
```

When diagnosing key behavior, prefer `TOUCHDECK_LOG_TOUCH=1` plus
IME logs from the same `touchdeck` process. The IME log line includes key,
state, modifiers, route, handled, and current preedit.

Common issues:

- Key reaches app but not Rime: use `output = "ime"` and keep route as `ime-key`.
- Emacs PGTK with `(setopt pgtk-use-im-context-on-new-connection nil)`: Emacs disables `GtkIMContext`, so it will not activate Wayland text-input/input-method. TouchDeck keys fall back to virtual-keyboard passthrough, but Rime commit/preedit cannot be delivered to Emacs through the standard Wayland IM path.
- Cursor key edits app instead of candidate/preedit: keep route as `ime-key`; if Rime handles it, it will be consumed.
- Cursor key must always bypass Rime: use `route = "app-key"` or `&ak KEY`.
- Swipe jumps too far: increase `repeat_start_ms` or `start_ms`; increase `repeat_interval_ms` if continuous repeat is too fast.
- Rime falls back to an unexpected schema: check your Rime user data directory and `default.custom.yaml` / `default.yaml`.
- Lua schema fails: your librime/plugin build is missing Lua support.

## Current limitations

- SVG layout supports rect slots only.
- Runtime has no built-in fallback slot/keymap defaults.
- Modes and layers are intentionally small: `base`, `text`, `niri_momentary`, `niri_locked`, `passthrough`; layers `base`, `niri`.
- The embedded touchdeck-ime runtime is a focused Rime frontend, not a full fcitx replacement yet.
- No graphical layout editor yet.
