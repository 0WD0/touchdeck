# touchdeck

Host-side Wayland touch input layer for niri.

It creates a transparent `wlr-layer-shell` overlay, receives `wl_touch` events,
controls its Wayland input region dynamically, resolves touch gestures through a
mode/layer keymap, and dispatches actions to either niri or a Wayland virtual
keyboard.

## Current model

The prototype separates input ownership from application passthrough:

- `Base`: fullscreen capture. This is the normal programmable touch layer.
- `Text`: fullscreen capture for virtual-keyboard text/key output.
- `NiriMomentary`: fullscreen capture while a configured momentary mode trigger is held.
- `NiriLocked`: fullscreen capture after a configured lock trigger.
- `Passthrough`: only the active binding targets for the current mode/layer are captured; the rest of the screen passes through to applications.

`Passthrough` is a mode, not the default behavior.

There is also a layer stack:

- `base`: default key bindings.
- `niri`: niri control bindings.
- Layer resolution checks the top layer first, then lower layers.
- `layer_momentary` pushes a layer and restores the previous stack on release.
- `layer_toggle` pushes an inactive layer or removes an active layer.

This is still not a full ZMK-like behavior engine. It now has the minimum shape
for configurable mode, layer stack, trigger, and behavior resolution.

## Built-in slots

The built-in layout provides these slot IDs:

- `left_bottom`: `x 0.00..0.18`, `y 0.82..1.00`
- `right_bottom`: `x 0.82..1.00`, `y 0.82..1.00`
- `bottom_edge`: `x 0.18..0.82`, `y 0.94..1.00`
- `top_left`: `x 0.00..0.12`, `y 0.00..0.10`
- `center`: `x 0.18..0.82`, `y 0.12..0.82`
- `full`: full overlay

External layouts are SVG files. TOML keymaps reference slots by ID; TOML does
not define slot geometry.

```toml
[layout]
svg = "layouts/phone-portrait.svg"
```

SVG layout v1 reads only `<rect>` elements with `data-td-slot`:

```xml
<svg viewBox="0 0 1000 2400" xmlns="http://www.w3.org/2000/svg">
  <rect
    data-td-slot="left_bottom"
    data-td-role="zone"
    data-td-capture="true"
    data-td-label="NIRI"
    x="0"
    y="1968"
    width="180"
    height="432" />
</svg>
```

Supported SVG slot attributes:

- `data-td-slot`: required slot ID
- `data-td-role`: optional `zone`, `key`, or `gesture-area`
- `data-td-capture`: optional boolean, defaults to `true`
- `data-td-label`: optional debug/display label

SVG `viewBox` is used to normalize coordinates. `path`, `circle`, transforms,
and group-level inheritance are not supported yet.

In debug draw mode, slot `role`, `capture`, and `label` metadata are used to
color slot rectangles and mark labeled slots. Current mode/layer binding targets
are highlighted on top of the base layout.

## Built-in bindings

Mode ownership controls are now represented as default bindings:

- `base/base`, left-bottom hold: `mode_momentary:niri`
- `passthrough/base`, left-bottom hold: `mode_momentary:niri`
- `base/base`, left-bottom double tap: `mode_toggle:niri_locked`
- `niri_locked/niri`, left-bottom double tap: `mode:base`
- `base/base`, bottom-edge double tap: `mode_toggle:passthrough`
- `passthrough/base`, bottom-edge double tap: `mode:base`
- `base/base`, bottom-edge swipe up: `mode_toggle:text`
- `passthrough/base`, bottom-edge swipe up: `mode:text`
- `text/base`, bottom-edge swipe down: `mode:base`
- Top-left tap in base, text, passthrough, or niri locked: `exit`

Base keyboard bindings:

- Right-bottom tap: virtual keyboard `Space`
- Right-bottom swipe left: virtual keyboard `Backspace`
- Right-bottom swipe right: virtual keyboard `Enter`

Text mode includes a small built-in QWERTY-ish row layout. Config files can
replace or extend this with `[[keyboard.rows]]`.

Default niri gestures still exist when no configured binding matches in niri modes:

- One-finger swipe left: `focus-column-left`
- One-finger swipe right: `focus-column-right`
- One-finger swipe up: `focus-workspace-up`
- One-finger swipe down: `focus-workspace-down`
- Two-finger tap: `toggle-overview`

Three-finger tap remains a hard safety exit.

## Virtual keyboard

The prototype binds `zwp_virtual_keyboard_manager_v1`, creates a virtual
keyboard for the current `wl_seat`, sends an XKB keymap, then emits key
press/release events for structured `key` and `key_sequence` behaviors.

By default it uses a built-in `us/pc105` XKB keymap. To provide your own raw XKB
keymap:

```sh
TOUCHDECK_XKB_KEYMAP=/path/to/keymap.xkb cargo run --release
```

If `zwp_virtual_keyboard_manager_v1` is unavailable, keyboard actions are ignored
and niri actions still work when `$NIRI_SOCKET` is available.

## niri IPC

niri actions are sent directly to `$NIRI_SOCKET` as JSON IPC requests. The
prototype does not run `niri msg action` and does not keep a command fallback.

Supported socket actions right now:

- `focus-column-left`
- `focus-column-right`
- `focus-workspace-up`
- `focus-workspace-down`
- `toggle-overview`

Configured niri actions are parsed into a typed action enum. Unsupported TOML
actions fail during config loading; unsupported environment override values are
logged and disabled instead of trying to guess a JSON shape or shelling out.

## Configurable bindings

If `TOUCHDECK_CONFIG` is set, that TOML file is loaded. Otherwise `./touchdeck.toml`
is loaded when it exists. If a config file contains `[[bindings]]`, those
bindings replace the built-in bindings. There is no legacy `gesture/action`
compatibility; use structured `trigger` and `behavior` tables.

Use [touchdeck.example.toml](/home/disk/Projects/touchdeck-prototype/touchdeck.example.toml) as a full working example.

Example binding:

```toml
[[bindings]]
mode = "base"
layer = "base"
trigger = { type = "swipe", target = "right_bottom", direction = "up" }
behavior = { type = "key", key = "C-c" }
```

Keyboard rows generate rectangular tap bindings automatically:

```toml
[keyboard]

[[keyboard.rows]]
mode = "text"
layer = "base"
x = [0.04, 0.96]
y = [0.54, 0.64]
keys = ["q", "w", "e", "r", "t", "y", "u", "i", "o", "p"]
gap = 0.006
```

Row fields:

- `mode`: defaults to `text`
- `layer`: defaults to `base`
- `x`: normalized horizontal range; defaults to `[0.04, 0.96]`
- `y`: required normalized vertical range
- `keys`: required Emacs-style key tokens or key sequences
- `gap`: normalized gap between generated keys; defaults to `0.0`
- `fingers`: defaults to `1`
- `max_ms`: optional tap time limit
- `priority`: defaults to `0`
- `consume`: defaults to `true`

Supported trigger types:

- `tap`
- `double_tap`
- `hold`
- `swipe`

Trigger fields:

- `target`: one of the configured/built-in slot IDs
- `fingers`: defaults to `1`
- `direction`: required for `swipe`; one of `left`, `right`, `up`, `down`
- `min_ms`: supported by `hold`
- `max_ms`: supported by `tap`, `double_tap`, and `swipe`
- `min_px`: supported by `swipe`

Supported behavior types:

- `key` with `key = "C-c"` or `key = "SPC"`
- `key_sequence` with `keys = "C-x C-s"`
- `niri` with `action = "focus-column-left"`
- `mode` / `mode_set` with `mode = "base"`
- `mode_toggle` with `mode = "passthrough"`
- `mode_momentary` with `mode = "niri"`
- `layer` / `layer_set` with `layer = "base"`
- `layer_toggle` with `layer = "niri"`
- `layer_momentary` with `layer = "niri"`
- `sequence` with inline `steps = [...]`
- `macro` with `macro = "copy"`
- `transparent`
- `noop`
- `exit`

Key syntax for `key`, `key_sequence`, and `key_sequence` macro steps follows the Emacs `kbd` / `read-kbd-macro` style subset:

- Examples: `f`, `C-c`, `C-x C-s`, `M-RET`, `C-M-<return>`, `s-<left>`
- Shorthands: `RET`, `SPC`, `TAB`, `ESC`, `DEL`
- Angle keys: `<return>`, `<space>`, `<tab>`, `<escape>`, `<backspace>`, `<delete>`, `<left>`, `<right>`, `<up>`, `<down>`
- Modifiers are parsed in Emacs order: `A-C-H-M-S-s`
- Current backend maps `C` to left Ctrl, `M`/`A` to left Alt, and `s`/`H` to left Super

Macro definitions:

```toml
[macros.copy]
steps = [
  { type = "key_sequence", keys = "C-c" },
]

[macros.ctrl_c_manual]
steps = [
  { type = "key_down", key = "<leftctrl>" },
  { type = "tap_key", key = "c" },
  { type = "key_up", key = "<leftctrl>" },
]
```

Supported macro step types:

- `key_down` with one physical key token, for example `<leftctrl>`
- `key_up` with one physical key token
- `tap_key` with one physical key token
- `key_sequence` with Emacs-style `keys`, for example `C-x C-s`
- `niri` with `action`
- `delay_ms` with `ms`

Layer fallthrough:

- `transparent` means skip this binding and continue resolving lower layers.
- `consume = false` on a binding also lets resolution continue instead of using that binding.
- Top-layer bindings normally override lower-layer bindings.

Supported mode names:

- `base`
- `text`
- `passthrough`
- `niri`
- `niri_momentary`
- `niri_locked`

Supported layer names:

- `base`
- `niri`

## Run

```sh
cd /home/disk/Projects/touchdeck-prototype
cargo run --release
```

Or use the launcher that loads `config.example.env`:

```sh
sh /home/disk/Projects/touchdeck-prototype/scripts/run.sh
```

Debug run with visible zones and touch points:

```sh
sh /home/disk/Projects/touchdeck-prototype/scripts/run-debug.sh
```

## Check

```sh
sh /home/disk/Projects/touchdeck-prototype/scripts/check.sh
```

## Trace recording

Record raw touch events as JSONL:

```sh
TOUCHDECK_RECORD_TRACE=/tmp/touchdeck-trace.jsonl sh scripts/run-debug.sh
```

Each line contains a relative timestamp, Wayland touch time, touch id, and
coordinates. The engine has replay tests built around this same format.

## Manual smoke test checklist

1. Start with `scripts/run-debug.sh`; Base mode should visibly cover the whole overlay in debug mode.
2. Right-bottom tap should send Space through the virtual keyboard.
3. Right-bottom swipe left should send Backspace.
4. Bottom-edge double tap should enter Passthrough; central app touch should pass through only in this mode.
5. Bottom-edge double tap again should return to Base fullscreen capture.
6. Hold the left-bottom zone for `TOUCHDECK_HOLD_MS`; niri mode tint should appear.
7. While holding, one-finger left/right/up/down swipes should control niri.
8. Releasing the hold finger should return to the previous mode.
9. Left-bottom double tap should lock niri mode; double tap again should unlock.
10. A top-left one-finger tap or three-finger tap should exit the prototype.
11. With `TOUCHDECK_CONFIG=touchdeck.example.toml`, right-bottom swipe up should send `Ctrl+C`.

## niri notes

The overlay does not request a specific `wl_output`; niri will place it on the
active output. For multi-output setups, map Sunshine's virtual touch device to
the streamed output in niri config if needed:

```kdl
input {
    touch {
        map-to-output "YOUR-OUTPUT-NAME"
    }
}
```
