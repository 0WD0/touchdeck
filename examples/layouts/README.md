# TouchDeck layout examples

This directory is intentionally split by responsibility:

- `keymaps/`: behavior-layer snippets. These bind slot IDs to ZMK-style behaviors, but do not define geometry.
- `svg/`: visual/touch geometry examples. These define slot positions with `data-td-slot`, but do not bind actions.
- `profiles/`: layout-only snippets. They are not complete runnable configs because TouchDeck no longer has Rust-side keymap fallback.

The default project layout is Charybdis-inspired: a 3x10 alpha grid, a separate number row above it, and a lower thumb/action row with Ctrl, Shift, Super, Enter, and Space. Geometry lives in SVG; behavior bindings live in TOML.

The examples here are not limited to that shape: they include split-thumb, staggered QWERTY, and one-hand-right geometries.

Use the full default config directly while experimenting:

```sh
TOUCHDECK_CONFIG=touchdeck.example.toml cargo run --release
```

The files in `profiles/` are layout-only snippets. Copy one `[layout]` section into a full config, then copy a keymap snippet from `keymaps/` if you want to experiment with behavior variants.

All key names use the ZMK-style token subset supported by TouchDeck, for example `Q`, `N1`, `BSPC`, `LEFT`, `QUESTION`, `LC(RET)`, and `LA(BSPC)`.
