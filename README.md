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

The checked-in phone portrait layout is not a physical-keyboard clone. It keeps
the central screen relatively open and places key slots in lower left/right
thumb clusters, with a larger bottom space/control row.

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

Text mode uses key slots defined by the active SVG layout. Config files map
slot IDs to key behavior with `[[keyboard.layers]]`; geometry stays in SVG.
`keyboard.layers` supports direct per-slot gestures: tap, hold, repeat, and four swipe
directions. Text mode draws keycaps even when debug draw is disabled, and labels
are resolved from behavior bindings rather than from static SVG metadata.

Text keycap labels are laid out as gesture hints:

- Center: tap binding
- Top edge: swipe up binding
- Bottom edge: swipe down binding
- Left edge: swipe left binding
- Right edge: swipe right binding
- Small lower-left hint: hold binding, when present

The built-in text keyboard is phone-first:

- tap: alphabetic QWERTY keys plus `ESC`, `SPC`, `BSPC`, `RET`
- hold: upper modifier keys `SFT`, `CTL`, `ALT`, `SUP` stay pressed until touch release
- swipe up: numbers and common symbols, optimized for quick thumb flicks
- directional swipes on selected home/special keys: arrows, word movement, backspace, enter, escape, and tab

The Charybdis/ZMK config is used as a reference for behavior composition,
high-frequency actions, and naming conventions, not as the visual or ergonomic
layout template.

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
trigger = { type = "swipe", target = "bottom_edge", direction = "up" }
behavior = { type = "mode", mode = "text" }
```

Keyboard layers generate tap/hold/repeat/swipe bindings from existing SVG slots.
They do not define geometry. Binding values use ZMK-style behavior invocation:

```toml
[keyboard]

[[keyboard.layers]]
mode = "text"
layer = "base"

[keyboard.layers.tap]
key_q = "&kp Q"
key_w = "&kp W"
key_spc = "&kp SPC"
key_del = "&kp BSPC"

[keyboard.layers.hold]
key_shift = "&hold LSHIFT"
key_ctrl = "&hold LCTRL"
key_alt = "&hold LALT"
key_super = "&hold LGUI"

[keyboard.layers.swipe_up]
key_q = "&kp N1"
key_w = "&kp N2"
key_a = "&kp EXCLAMATION"
key_s = "&kp AT_SIGN"
key_spc = "&kp TAB"

[keyboard.layers.swipe_left]
key_h = "&hold_repeat LEFT"
key_spc = "&hold_repeat BSPC"

[keyboard.layers.swipe_down]
key_j = "&hold_repeat DOWN"
key_spc = "&kp ESC"

[keyboard.layers.swipe_right]
key_l = "&hold_repeat RIGHT"
key_spc = "&kp RET"
```

Map fields:

- `mode`: defaults to `text`
- `layer`: defaults to `base`
- `tap`: optional table mapping slot IDs to behavior bindings
- `hold`: optional table mapping slot IDs to behavior bindings evaluated at the hold threshold
- `repeat`: optional table mapping slot IDs to fixed hold-repeat behavior bindings
- `swipe_up`: optional table mapping slot IDs to behavior bindings
- `swipe_down`: optional table mapping slot IDs to behavior bindings
- `swipe_left`: optional table mapping slot IDs to behavior bindings
- `swipe_right`: optional table mapping slot IDs to behavior bindings
- `fingers`: defaults to `1`
- `max_ms`: optional tap/swipe time limit
- `hold_ms`: optional hold threshold
- `min_px`: optional swipe distance threshold
- `priority`: defaults to `0`
- `consume`: defaults to `true`

For example, this gives vim-style arrows without a nav layer:

```toml
[keyboard.layers.swipe_left]
key_h = "&kp LEFT"

[keyboard.layers.swipe_down]
key_j = "&kp DOWN"

[keyboard.layers.swipe_up]
key_k = "&kp UP"

[keyboard.layers.swipe_right]
key_l = "&kp RIGHT"
```

Upper modifier slots are intended to be held with one thumb/finger while another key is tapped or flicked.

The same slot can be bound to multiple gestures. The main keycap label remains
the active tap binding; directional gesture bindings are rendered as edge hints.
Swipe bindings that use `&hold KEY` or `&hold_repeat KEY` activate as soon as the
finger crosses the swipe threshold and stay active until release. Plain `&kp KEY`
swipes still resolve once on release.

Supported trigger types:

- `tap`
- `double_tap`
- `hold`
- `swipe`

Trigger fields:

- `target`: one of the slot IDs declared by the configured layout
- `fingers`: defaults to `1`
- `direction`: required for `swipe`; one of `left`, `right`, `up`, `down`
- `min_ms`: supported by `hold`
- `max_ms`: supported by `tap`, `double_tap`, and `swipe`
- `min_px`: supported by `swipe`

Supported behavior types:

- `key` with `key = "LC(C)"` or `key = "SPC"`
- `key_sequence` with `keys = "LC(X) LC(S)"`
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

Key syntax for `key`, `key_sequence`, and `key_sequence` macro steps follows a ZMK-style subset:

- Examples: `A`, `N1`, `RET`, `LEFT`, `LC(C)`, `LC(X) LC(S)`, `LA(RET)`, `LG(LEFT)`
- Common key tokens: `RET`, `SPC`, `TAB`, `ESC`, `BSPC`, `DEL`, `DELETE`, `LEFT`, `RIGHT`, `UP`, `DOWN`
- Letter and number tokens: `A` through `Z`, `N1` through `N0`
- US punctuation tokens: `MINUS`, `EQUAL`, `LEFT_BRACKET`, `RIGHT_BRACKET`, `SEMICOLON`, `SINGLE_QUOTE`, `GRAVE`, `BACKSLASH`, `COMMA`, `PERIOD`, `SLASH`
- Shifted punctuation tokens: `EXCLAMATION`, `AT_SIGN`, `HASH`, `DOLLAR`, `PERCENT`, `CARET`, `AMPERSAND`, `ASTERISK`, `UNDERSCORE`, `QUESTION`
- Modifier wrappers: `LC(...)`, `LS(...)`, `LA(...)`, `LG(...)`, `RC(...)`, `RS(...)`, `RA(...)`, `RG(...)`
