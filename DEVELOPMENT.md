# Development

```bash
cargo build --release
cargo test
```

The build needs the X11 and Wayland client libraries with their development
headers: `libxcb`, `libxkbcommon` and `libwayland-client`.

## Updating the screenshot

The picture in `README.md` is generated, not captured by hand. `tools/screenshot.rs`
drives the real calibration window through `gui::main_window::draw` and saves
the frame, so the screenshot cannot quietly drift away from the program.

Regenerate it whenever the window's layout or wording changes:

```bash
cargo run --features screenshot --bin unburn-screenshot
```

It needs a display server to render on, because egui draws through OpenGL. A
window appears for about a second while it works.
