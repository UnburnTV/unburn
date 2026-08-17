# unburn

A transparent compensation layer that evens out spatial brightness defects in a
monitor or TV. `README.md` is the design specification; this file is what you
need to build and use the program.

unburn never captures or re-renders the desktop. It puts a mostly black, fully
click-through surface on top of the screen and varies its alpha across the
panel, so the patches that emit too much light are attenuated down to match
their surroundings.

## Building

Requires a Rust toolchain (2021 edition, 1.85 or newer):

```bash
cargo build --release
./target/release/unburn
```

The build needs the usual X11 and Wayland client libraries plus their
development headers: `libxcb`, `libxkbcommon`, `libwayland-client`.

## Checking your session

```bash
unburn check          # which overlay backends this session supports
unburn list-displays  # monitors as unburn sees them
```

`check` reports one of three levels per backend:

- **Full** — the overlay is guaranteed to sit above everything and take no
  input. On Wayland this needs `wlr-layer-shell-v1` (wlroots, Sway, Hyprland,
  KDE); on X11 it needs a compositing manager.
- **Limited** — unburn can only ask for an ordinary window, which the
  compositor may stack below others. GNOME's Wayland session lands here, but
  its XWayland server gives the X11 backend full support, so unburn uses that
  instead.
- **Unsupported** — no display server was found at all.

Pick a backend explicitly with `--backend x11` or `--backend wayland` if the
automatic choice is wrong.

## Calibrating

Run `unburn` with no arguments to get the calibration window.

1. Choose the monitor from the **Display** menu.
2. Open a grey test pattern (25% or 50% works best) from the **Test pattern**
   menu. Blemishes are easiest to see on mid grey; look at the screen from your
   normal viewing position and off-axis.
3. Press **Edit on screen**. Spots are placed on the monitor itself — there is
   no preview and no numeric position or size fields, because a blemish is
   something you match by eye against the real panel. The overlay becomes
   interactive:
   - Drag a spot onto the blemish. It follows the pointer exactly, from
     wherever you took hold of it.
   - Drag one of the four square handles to stretch the ellipse along that
     axis, or use the wheel to resize both axes at once.
   - `n` puts a new spot under the pointer.
   - `Shift`+wheel changes strength, `Ctrl`+wheel changes falloff, `Tab` walks
     between spots, `Delete` removes the selected one.
   - Clicking anywhere that is not a spot leaves editing mode, as does `Esc`.

   While this mode is on, the overlay takes every click and keystroke on that
   monitor, so leave it when you want the desktop back. If it ever gets stuck,
   `Ctrl`+`Alt`+`Shift`+`Backspace` tears every overlay down.
4. Raise **Strength** until the spot stops standing out. Strength is how much
   *brighter* than the rest of the panel the patch is, so the overlay darkens
   the patch itself. It goes up to 100%, meaning a patch modelled as twice as
   bright as its surroundings — which costs half the light wherever the spot
   reaches, so it is a setting to arrive at, not to start from.
5. If the patch is off-colour rather than just too bright, tick **Separate
   colour channels** and set red, green and blue independently. A patch that
   looks warm needs more taken out of red than out of blue.
6. For a panel with several similar blemishes, select one and press **Clone
   spot**. The copy keeps the shape, strength and colour, and lands beside the
   original so both stay reachable; drag it onto the next blemish.
7. Switch the on-screen display between *Correction*, *Model* and *Outline* to
   check that the modelled defect matches the real one before you trust the
   correction.
8. Press `Space` at any time for an instant before/after. The overlay is
   removed and restored without recomputing anything.
9. Repeat across several grey levels. If the correction is right at 50% but
   overshoots at 10%, adjust **Gamma** rather than the spot strength.
10. **Save profile** writes the settings to disk.

The **Compensation** slider scales the whole correction. At 100% the panel is
brought all the way down to its dimmest modelled point, which is the most
uniform and the dimmest result; lower values trade uniformity for brightness.
The **Summary** section reports how much light the current settings remove.

### Dim patches

A spot with a *negative* strength describes a patch that is too dim rather than
too bright. Nothing can add light, so unburn matches it the only way available:
by dimming the whole rest of the panel down to it. That is expensive in
brightness, which is why the strength sliders only offer the cheap direction;
write a negative number into the profile by hand if you need it.

### Per-channel correction and its cost

A single alpha-blended surface can only scale every colour channel by the same
factor: the compositor computes `out = colour + desktop × (1 - alpha)`, and
there is one alpha for all three channels. unburn therefore sets `alpha` from
the channel that needs the *most* attenuation and uses the surface's own colour
to hand back the light the other channels should have kept.

That reconstruction is exact at one desktop level and drifts either side of it.
The level is the **Reference level** setting under *Advanced*, 50% by default;
set it to whatever you actually look at. The visible symptom away from it is a
faint constant glow on black, which the window reports as **Black lifted by**.
Neutral spots are unaffected: with equal channel strengths the surface colour is
zero and the overlay is exactly black-with-alpha again.

## Running in the background

```bash
unburn start                     # overlays only, no calibration window
unburn start --profile bedroom   # a named profile
```

Tick **Start automatically on login** in the window, or run
`unburn start` from your compositor's autostart. A single instance holds a
control socket; further invocations talk to it instead of starting a second
copy:

```bash
unburn hide      # hide compensation
unburn show      # put it back
unburn status    # what the running instance is doing
unburn quit
```

## Profiles

Settings live in `$XDG_CONFIG_HOME/unburn/config.toml` (by default
`~/.config/unburn/config.toml`); `--profile NAME` uses
`~/.config/unburn/profiles/NAME.toml`. The format is meant to be hand-edited:

```toml
version = 1

[[display]]
name = "Living Room TV"
connector = "HDMI-A-1"
model = "QN90A"
serial = "0x01010101"
enabled = true
compensation = 0.82
gamma = 2.2
reference = 0.5

[[display.defects]]
kind = "radial"
center = [0.62, 0.43]   # fraction of the panel, origin top-left
radius = [0.075, 0.091] # 1-sigma radii, same units
rotation = 0.0          # radians, counter-clockwise
strength = 0.11         # peak brightness excess, -1..1
falloff = 1.0           # >1 sharpens the edge, <1 softens it

[[display.defects]]
kind = "radial"
center = [0.31, 0.68]
radius = [0.052, 0.057]
strength = [0.09, 0.05, 0.04]  # per channel: this patch runs warm
```

A bare `strength` applies to all three channels; a list is red, green and blue
separately. Positive means the patch emits too much light and gets darkened,
negative means it is too dim and everything else comes down to meet it.

Coordinates are always in the panel's own unrotated frame, so a profile keeps
working if you rotate the display. Monitors are matched by serial, model and
EDID hash before connector name, so a profile survives being plugged into a
different port.

## What it costs

The mask is computed once per change on the CPU and uploaded as a static
buffer. Nothing redraws while the settings are unchanged, so an idle instance
uses no measurable CPU. The overlay does add one full-screen alpha-blended
surface for the compositor to composite, and it costs brightness by design: the
figure in the summary is how much light the correction removes at its strongest
point.

## Limitations

- Compensation can only remove light, never add it. A patch that is too bright
  is cheap to fix; one that is too dim costs the brightness of the whole panel.
- Correction is content-independent: it is right for mid greys and approximate
  elsewhere, which is what the gamma control exists to trim.
- Per-channel correction is exact only at the reference level, and lifts black
  slightly everywhere else. See above.
- Screenshots and screen recordings do not include the overlay, because it is a
  separate surface and not part of the desktop image.
- On a compositor without layer-shell the overlay is an ordinary window and may
  be covered by fullscreen applications.
