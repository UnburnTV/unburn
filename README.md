# [unburn.tv](https://unburn.tv)

Corrects OLED and LCD mura, also known as "backlight bleed", that appear due to aging or applied pressure.

![Typical defects examples](docs/defects.jpg)

The app draws an overlay over other windows that compensates for the increased brightness. The position, size and color parameters of the defects are configurable via a visual editor.

![UI](docs/edit-mode.png)

## Ubuntu package

Build an Ubuntu-compatible Debian package for a release tag:

```sh
./scripts/build-deb.sh v0.1.0
```

The build requires the Rust nightly toolchain, Python 3, and Debian package
tools. The tag version must match the version in `Cargo.toml`. The package is
written to `dist/`. GitHub Actions builds, installs, verifies, and uploads the
package when a `vX.Y.Z` tag is pushed. Local package builds do not install the
package.
