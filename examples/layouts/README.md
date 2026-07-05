# TouchDeck layout examples

This directory is intentionally split by responsibility:

- `keymaps/`: behavior-layer snippets. These bind slot IDs to ZMK-style behaviors, but do not define geometry.
- `svg/`: visual/touch geometry examples. These define slot positions with `data-td-slot`, but do not bind actions.
- `profiles/`: tiny config snippets that select one SVG geometry while inheriting the default bindings.

The default project layout is 3x10 ortholinear because it is predictable on a phone screen. The examples here are not limited to that shape: they include split-thumb, staggered QWERTY, and one-hand-right geometries.

Use a profile directly while experimenting:

```sh
TOUCHDECK_CONFIG=examples/layouts/profiles/split-thumb.toml cargo run --release
```

Or copy a keymap snippet from `keymaps/` into your normal `touchdeck.toml`.

All key names use the ZMK-style token subset supported by TouchDeck, for example `Q`, `N1`, `BSPC`, `LEFT`, `QUESTION`, `LC(RET)`, and `LA(BSPC)`.
