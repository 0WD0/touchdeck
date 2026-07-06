# Touch input routing design

TouchDeck is not a normal on-screen keyboard. It is an input ownership layer.
This document describes how touch input is routed when TouchDeck is used with
the Sunshine fork.

## Problem

The old evdev backend tried to switch between these states:

```text
owned by TouchDeck:
  EVIOCGRAB=true

passthrough:
  EVIOCGRAB=false
  Wayland layer-shell input region = control zones
```

That model is fragile for two reasons.

First, `EVIOCGRAB` is a device-level exclusive grab. When it is enabled,
Sunshine's virtual touchscreen events never reach niri/libinput, so app
passthrough is impossible.

Second, Wayland `wl_surface.set_input_region` is geometry-based, not
device-type-based. A layer-shell surface cannot say "receive touchscreen events
but let mouse buttons and scroll wheels pass through". If the overlay is hit by
pointer/touch input, it owns that hit-test result.

This means a regular Wayland client cannot implement the desired policy:

```text
passthrough mode:
  app owns most of the screen
  TouchDeck owns wake/control zones
  mouse, touchpad buttons, and scroll wheels continue to work normally
```

## Current design

The preferred backend is `sunshine-router`.

```toml
[input]
backend = "sunshine-router"
```

The key architectural change is that routing happens before Sunshine injects
touch input into the compositor.

```text
Moonlight native touch
  -> Sunshine
  -> TouchDeck router
      -> decision: touchdeck
           TouchDeck consumes the contact
           Sunshine does not inject it
      -> decision: app
           Sunshine injects it normally
           TouchDeck ignores that contact
  -> niri/libinput/app
```

The Wayland overlay becomes display-only in this mode:

```text
Wayland layer-shell overlay:
  draw keycaps, hints, and IME UI
  input_region = empty
```

This keeps pointer motion, mouse buttons, scroll wheels, and touchpad events out
of TouchDeck's Wayland surface entirely.

## Contact ownership

Ownership is decided on `down` and then remains stable for the lifetime of the
contact.

```rust
enum TouchOwner {
    TouchDeck,
    App,
}
```

Routing policy is derived from the current TouchDeck mode:

```text
base/text/niri:
  CapturePolicy::Fullscreen
  -> all new contacts are TouchDeck-owned

passthrough:
  CapturePolicy::Zones(...)
  -> contacts starting inside active zones are TouchDeck-owned
  -> contacts starting outside active zones are App-owned

none:
  CapturePolicy::None
  -> all new contacts are App-owned
```

For a TouchDeck-owned contact:

```text
down/motion/up
  -> Engine
  -> gesture/keymap
  -> action/IME/niri
```

For an App-owned contact:

```text
down/motion/up
  -> Sunshine injects to the platform backend
  -> niri/app
```

TouchDeck stores ownership per contact id, so `motion` and `up` do not
re-evaluate geometry. This avoids changing owners mid-gesture.

## Unix socket protocol

Sunshine sends one datagram per touch event to:

```text
${XDG_RUNTIME_DIR}/touchdeck/sunshine.sock
```

Both sides can override the socket path:

```sh
TOUCHDECK_SUNSHINE_SOCKET=/run/user/1000/touchdeck/sunshine.sock
SUNSHINE_TOUCHDECK_SOCKET=/run/user/1000/touchdeck/sunshine.sock
```

Request format:

```text
touchdeck-route-v1 seq=42 output=DP-2 event=down id=7 x=0.123 y=0.456 width=2160 height=3840 time=123456
```

Response format:

```text
touchdeck-route-v1 seq=42 touchdeck
```

or:

```text
touchdeck-route-v1 seq=42 app
```

The `seq` field prevents stale replies from being applied to later requests.

Coordinates are normalized to the streamed output. TouchDeck maps them to the
current overlay surface size for hit-testing.

## Multi-session model

Sunshine passes the target output as `output=...`.

TouchDeck creates one session per output:

```text
output DP-2
  -> TouchSession A
  -> overlay bound to DP-2
  -> independent mode/layer/contact state

output HDMI-A-1
  -> TouchSession B
  -> overlay bound to HDMI-A-1
  -> independent mode/layer/contact state
```

Multiple Moonlight clients on different outputs are supported by this model.

Multiple clients on the same output are intentionally not solved yet; they share
one TouchDeck session and may compete for contact ids.

## Failure behavior

If TouchDeck is not running or does not reply quickly, Sunshine falls back to:

```text
decision = app
```

This fallback exists only to avoid making Sunshine input unusable when the
router is unavailable. It does not change TouchDeck's routing semantics when
TouchDeck is running.

If TouchDeck receives a request before the corresponding overlay is configured,
it also returns `app`. A half-initialized overlay should never create an input
black hole.

## Legacy backends

`wayland` is useful when running as a pure Wayland layer-shell client without
Sunshine integration.

`evdev` is useful for debugging raw touchscreen events and Sunshine's generated
uinput device names.

Neither backend can fully express app passthrough with TouchDeck-owned control
zones. The `sunshine-router` backend is the intended path for daily use with
Moonlight + Sunshine + niri.

## Future work

- Route pen events through the same protocol.
- Replace the text datagram protocol with a compact binary protocol if latency
  or allocation overhead becomes measurable.
- Add explicit session ids if multiple Moonlight clients on the same output need
  separate state.
- Move from synchronous ask/answer to an async queue only if the synchronous
  timeout becomes observable.
- Eventually remove the evdev grab/ungrab path once the Sunshine router backend
  is stable.
