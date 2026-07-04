#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

debug_alpha_was_set=${TOUCHDECK_DEBUG_ALPHA+x}
debug_draw_was_set=${TOUCHDECK_DEBUG_DRAW+x}
tap_radius_was_set=${TOUCHDECK_TAP_RADIUS+x}

if [ -f ./config.example.env ]; then
    set -a
    . ./config.example.env
    set +a
fi

if [ -z "$debug_alpha_was_set" ]; then
    TOUCHDECK_DEBUG_ALPHA=0
fi

if [ -z "$debug_draw_was_set" ]; then
    TOUCHDECK_DEBUG_DRAW=true
fi

if [ -z "$tap_radius_was_set" ]; then
    TOUCHDECK_TAP_RADIUS=56
fi

export TOUCHDECK_DEBUG_ALPHA
export TOUCHDECK_DEBUG_DRAW
export TOUCHDECK_TAP_RADIUS

cargo run --release
