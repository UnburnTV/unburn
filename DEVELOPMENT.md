# Development

```bash
cargo build --release
cargo test
```

The build needs the X11 and Wayland client libraries with their development
headers: `libxcb`, `libxkbcommon` and `libwayland-client`.

## Ubuntu package

Build an Ubuntu-compatible Debian package for a release tag:

```sh
./scripts/build-deb.sh v0.1.0
```

The build requires Python 3 and Debian package tools. The repository's
`rust-toolchain.toml` selects the Rust nightly toolchain. The tag version must
match the version in `Cargo.toml`, and the package is written to `dist/`.

GitHub Actions builds native `amd64` and `arm64` packages for `vX.Y.Z` tags and
for every push to the `test-release` branch. Test-branch packages are workflow
artifacts only; they are not published as GitHub releases. Local package builds
do not install the package.

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
