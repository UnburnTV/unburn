# Display Compensation Overlay

## 1. Purpose

A Linux desktop application for compensating for spatial brightness defects in a monitor or TV, especially smooth circular or elliptical dark/bright regions.

The application displays a persistent, transparent, non-interactive overlay over a selected monitor. The overlay attenuates selected portions of the image so that the display appears more uniform.

Primary requirements:

- Written in Rust.
- GUI-based calibration.
- Supports X11 and Wayland.
- Supports multiple monitors.
- Negligible CPU usage when idle.
- Does not capture, copy, or re-render the desktop.
- Does not reduce spatial resolution.
- Normal operation must not interfere with mouse or keyboard input.
- Compensation profiles persist across restarts.
- Designed so automated camera-based calibration can be added later.

## 2. Important architectural decision

### Do not capture and post-process the desktop

The application should **not** work like:

```text
desktop
  ↓
screen capture
  ↓
shader
  ↓
display corrected image
```

That introduces latency, synchronization problems, protected-content problems, unnecessary GPU bandwidth, and substantial Wayland complexity.

Instead:

```text
desktop
  +
transparent compensation layer
  ↓
compositor
  ↓
monitor
```

The compensation layer is effectively a black image whose alpha varies spatially.

For a pixel with attenuation factor:

\[
C(x,y) \in [0,1]
\]

the overlay alpha is approximately:

\[
A(x,y)=1-C(x,y)
\]

Conceptually:

```text
final_pixel = desktop_pixel * C(x, y)
```

This means the program can only **remove light**, not create additional brightness.

That is desirable for damaged dark regions: find the darkest usable region and reduce the rest of the panel to approximately the same effective brightness.

No access to the underlying desktop pixels is required.

---

# 3. Compensation model

The application maintains a model of the panel's relative brightness response:

\[
D(x,y)
\]

where:

```text
1.0 = normal brightness
0.9 = 10% dimmer
0.8 = 20% dimmer
```

A user does not edit `D(x,y)` directly. Instead, they create **defects**.

## 3.1 Radial defect

The basic defect is an elliptical Gaussian:

\[
d_i(x,y)=a_i
\exp\left(
-\frac12
\left[
\frac{x_i'^2}{\sigma_{x,i}^2}
+
\frac{y_i'^2}{\sigma_{y,i}^2}
\right]
\right)
\]

where:

- `center_x`: horizontal center
- `center_y`: vertical center
- `radius_x`: horizontal extent
- `radius_y`: vertical extent
- `rotation`: ellipse rotation
- `strength`: peak brightness defect
- `falloff`: controls edge softness

Coordinates should internally be normalized to the monitor:

```text
x ∈ [0, 1]
y ∈ [0, 1]
```

so profiles survive resolution changes.

For an ordinary dark spot:

\[
D_i(x,y)=1-d_i(x,y)
\]

Example:

```text
center:    (0.63, 0.41)
radius_x:  0.08
radius_y:  0.09
strength:  0.12
```

means roughly a 12% darkening at the center.

## 3.2 Multiple defects

Defects should compose multiplicatively:

\[
D(x,y)=\prod_i D_i(x,y)
\]

rather than simply drawing independent black circles.

This handles overlapping defects naturally.

The implementation should nevertheless allow alternative composition algorithms later.

---

# 4. Deriving the correction field

Suppose the panel has response:

\[
D(x,y)
\]

and the desired uniform brightness is:

\[
T
\]

Choose:

\[
T \le \min D(x,y)
\]

Then the required attenuation is:

\[
C(x,y)=\frac{T}{D(x,y)}
\]

Because:

\[
C(x,y)\le1
\]

the entire correction can be implemented through attenuation.

Example:

```text
healthy region: D = 1.00
dark spot:      D = 0.85

target:         T = 0.85
```

Therefore:

```text
healthy region: C = 0.85
dark spot:      C = 1.00
```

The defective region remains untouched while the healthy portion is reduced.

The GUI should expose this primarily as:

```text
Global compensation: 0% ───────── 100%
```

rather than asking the user to understand `T`.

At 100%, the target is the darkest modeled point.

At lower settings, the program applies only a fraction of the calculated correction.

---

# 5. Gamma

A compositor overlay multiplies encoded RGB values; that is not necessarily the same thing as multiplying physical luminance.

Therefore expose:

```text
Compensation gamma
```

with a default around the display's expected transfer response and allow manual adjustment.

Approximate conversion:

\[
C_\text{encoded}=C_\text{luminance}^{1/\gamma}
\]

Then:

\[
A=1-C_\text{encoded}
\]

The exact display response is not known, so this should be treated as a calibration parameter rather than as an assumption about the TV.

MVP can use a single gamma value for the whole display.

Future versions can use a measured response curve.

---

# 6. Rendering

## Normal mode

For every connected corrected monitor, create one fullscreen transparent surface.

Render:

```text
RGBA = (0, 0, 0, alpha(x,y))
```

The RGB components are always black.

Only alpha changes.

The mask is spatially smooth and normally static, so it only needs to be regenerated when:

- a defect changes
- global compensation changes
- gamma changes
- monitor resolution changes
- profile changes

There is no reason to continuously calculate Gaussian functions at 60 FPS.

Generate the mask into a texture when configuration changes and then present that texture.

`wgpu` is a sensible renderer because it provides a portable Rust GPU API and supports native Vulkan/OpenGL-class backends.

However, the renderer should be abstracted:

```rust
trait MaskRenderer {
    fn resize(&mut self, width: u32, height: u32);
    fn upload_mask(&mut self, mask: &Mask);
    fn render(&mut self);
}
```

This leaves open a future CPU/shared-memory backend because the overlay itself is computationally trivial.

---

# 7. Wayland backend

Wayland does not give an ordinary application arbitrary control over the compositor's completed framebuffer. The overlay should therefore exist as its own Wayland surface.

When available, use `wlr-layer-shell-v1`, which explicitly provides desktop-layer surfaces with defined z-order, anchoring and input semantics. Rust bindings are available through `wayland-protocols-wlr`.

For every selected output:

```text
layer: overlay
anchor: top + bottom + left + right
exclusive zone: -1 / none
keyboard input: none
pointer input: none in normal mode
size: entire output
```

### Wayland requirements

Normal overlay mode MUST:

- cover the complete output
- receive no keyboard focus
- receive no pointer input
- not reserve desktop space
- remain above ordinary application windows
- not appear in task switchers where avoidable

### Compatibility

The program must detect whether the compositor exposes the required layer-shell protocol.

If available:

```text
Wayland support: Full
```

If unavailable:

```text
Wayland support: Limited
Your compositor does not provide the layer-shell protocol required
for a guaranteed always-on-top compensation layer.
```

A regular transparent window may be offered as a fallback, but the software must **not pretend that fallback is equivalent**.

This is important because Wayland compositor capabilities differ by protocol support.

---

# 8. X11 backend

Use an ARGB transparent fullscreen window.

`x11rb` provides Rust access to the X11 protocol and its extensions and is suitable for the platform-specific implementation.

Requirements:

- one overlay window per corrected monitor
- borderless
- transparent
- topmost
- excluded from normal task/window management where practical
- empty input region so mouse events pass through
- no keyboard focus
- move/recreate overlay when monitor geometry changes

The program should detect whether transparent compositing is available.

Without an X11 compositor, normal alpha-transparent operation may not be possible; report that explicitly rather than silently displaying an opaque surface.

---

# 9. Windowing architecture

Use two conceptually different types of windows.

## Controller window

Normal desktop application window containing the GUI.

A conventional Rust windowing stack can support both X11 and Wayland; current `winit` supports both Linux backends.

Suggested GUI:

```text
egui
```

because this application mainly needs sliders, lists, numerical parameters and a custom 2D editor. `egui` is a native pure-Rust immediate-mode GUI and is well suited to this type of tool.

## Overlay windows

Do **not** force the overlay implementation through the same abstraction as the GUI.

Use:

```text
platform/
    wayland.rs
    x11.rs
```

because the required behavior is inherently platform-specific.

Interface:

```rust
trait OverlayBackend {
    fn outputs(&self) -> Vec<OutputInfo>;

    fn create_overlay(
        &mut self,
        output: OutputId,
    ) -> Result<OverlayId>;

    fn destroy_overlay(
        &mut self,
        overlay: OverlayId,
    );

    fn set_interactive(
        &mut self,
        overlay: OverlayId,
        interactive: bool,
    );

    fn set_visible(
        &mut self,
        overlay: OverlayId,
        visible: bool,
    );

    fn update_mask(
        &mut self,
        overlay: OverlayId,
        mask: &Mask,
    );
}
```

---

# 10. GUI

Main window:

```text
┌───────────────────────────────────────────────────┐
│ Display Compensation                             │
├───────────────────────────────────────────────────┤
│ Display: [ HDMI-A-1 — Samsung TV            ▼ ] │
│                                                   │
│ Compensation       [██████████████------] 72%    │
│ Gamma              [██████████----------] 2.20   │
│                                                   │
│ Defects                                           │
│ ┌───────────────────────────────────────────────┐ │
│ │ ● Spot 1         dark       enabled          │ │
│ │ ● Spot 2         dark       enabled          │ │
│ │ ● Spot 3         dark       enabled          │ │
│ └───────────────────────────────────────────────┘ │
│                                                   │
│ [+ Add spot]  [Edit on screen]  [Delete]         │
│                                                   │
│ Selected spot                                     │
│ X             [-----------●-------]               │
│ Y             [------●------------]               │
│ Width         [---------●---------]               │
│ Height        [---------●---------]               │
│ Strength      [-----●-------------] 8.5%          │
│ Falloff       [----------●--------]               │
│ Rotation      [●------------------] 0°            │
│                                                   │
│ [Test pattern ▼]     [Bypass]                     │
└───────────────────────────────────────────────────┘
```

Changes update the monitor immediately.

---

# 11. On-screen editing

The important UX feature should be:

```text
Edit on screen
```

When activated, the overlay becomes temporarily interactive.

Draw the defect model directly over the affected TV:

```text
           ┌──────────── radius
           │
       .............
     ....         ....
    ...      +      ...
     ....         ....
       .............
             ↑
           center
```

Controls:

- drag center → move defect
- drag horizontal handle → width
- drag vertical handle → height
- mouse wheel → radius
- Shift + wheel → strength
- Ctrl + wheel → falloff
- Delete → remove
- `N` → new defect
- Tab → next defect
- Escape → leave editing mode

The currently selected defect gets a visible outline.

Other defects are optionally outlined faintly.

The actual compensation remains visible while editing.

A modifier or GUI toggle should allow:

```text
Show model
Show correction
Show both
```

Normal mode returns the surface to completely click-through behavior.

---

# 12. Test patterns

The program should provide fullscreen calibration patterns:

```text
Black
5% gray
10% gray
25% gray
50% gray
75% gray
100% white
Red
Green
Blue
```

Also:

```text
Cycle grayscale
```

The test surface should be generated by the program itself.

Modes:

```text
Raw
Compensated
```

Keyboard:

```text
Space  toggle correction
← →    change test pattern
Esc    exit
```

This makes manual calibration dramatically easier because the user can rapidly compare:

```text
before → after → before → after
```

without manipulating unrelated applications.

---

# 13. Useful calibration workflow

The intended manual workflow:

1. Select the TV.
2. Display 50% gray.
3. Add a spot.
4. Move its center over the visible defect.
5. Adjust width/height.
6. Adjust falloff until the shape matches.
7. Adjust strength until the transition becomes difficult to see.
8. Repeat for every defect.
9. Switch among 25%, 50%, 75% and 100% gray.
10. Adjust compensation gamma.
11. Reduce global compensation if the brightness loss is excessive.
12. Save the profile.

Do not optimize exclusively against one grayscale level.

---

# 14. Bypass

The program MUST provide an instantaneous bypass.

GUI:

```text
[ Compensation ON ]
```

Global hotkey if available:

```text
Super + Shift + B
```

Behavior:

```text
pressed:
    overlay alpha = 0

released / pressed again:
    restore compensation
```

No recomputation should be required.

This is critical for calibration.

---

# 15. Profiles

Configuration should be human-readable.

Suggested format: TOML.

Example:

```toml
version = 1

[[display]]
name = "Living Room TV"
connector = "HDMI-A-1"
enabled = true
compensation = 0.82
gamma = 2.2

[[display.defects]]
id = "spot-1"
kind = "radial"
enabled = true
center = [0.62, 0.43]
radius = [0.075, 0.091]
rotation = 0.0
strength = 0.11
falloff = 1.0

[[display.defects]]
id = "spot-2"
kind = "radial"
enabled = true
center = [0.31, 0.68]
radius = [0.052, 0.057]
rotation = 0.0
strength = 0.065
falloff = 1.3
```

Store under:

```text
$XDG_CONFIG_HOME/display-compensator/config.toml
```

Profiles should identify monitors using as much stable information as available:

```text
connector
manufacturer
model
serial
EDID hash
```

Never identify a display solely by screen coordinates.

---

# 16. Data model

```rust
struct Profile {
    version: u32,
    displays: Vec<DisplayProfile>,
}

struct DisplayProfile {
    identity: DisplayIdentity,
    enabled: bool,
    compensation: f32,
    gamma: f32,
    defects: Vec<Defect>,
}

enum Defect {
    Radial(RadialDefect),
}

struct RadialDefect {
    id: Uuid,
    enabled: bool,

    center: Vec2,
    radius: Vec2,
    rotation: f32,

    strength: f32,
    falloff: f32,
}
```

Keep `Defect` as an enum from the beginning even though MVP supports only one kind.

That makes later additions straightforward:

```rust
enum Defect {
    Radial(...),
    Gradient(...),
    Polygon(...),
    PaintedMask(...),
    ImportedMask(...),
}
```

---

# 17. Mask generation

Mask generation is CPU-cheap and can initially occur on the CPU.

Pseudo-code:

```rust
for y in 0..height {
    for x in 0..width {
        let uv = Vec2::new(
            (x as f32 + 0.5) / width as f32,
            (y as f32 + 0.5) / height as f32,
        );

        let mut defect_gain = 1.0;

        for defect in enabled_defects {
            defect_gain *= defect.gain_at(uv);
        }

        minimum_gain =
            minimum_gain.min(defect_gain);

        panel_gain[y][x] = defect_gain;
    }
}
```

Second pass:

```rust
let target =
    lerp(1.0, minimum_gain, compensation);

for pixel in panel_gain {
    let luminance_attenuation =
        (target / pixel).min(1.0);

    let encoded_attenuation =
        luminance_attenuation.powf(1.0 / gamma);

    alpha =
        1.0 - encoded_attenuation;
}
```

The mask can be computed at reduced resolution, for example:

```text
1/4 or 1/8 native resolution
```

and bilinearly interpolated by the GPU because the defects are extremely low-frequency.

For a 3840×2160 TV, even a mask around:

```text
960 × 540
```

is probably far more spatial resolution than necessary for smooth panel defects.

The GUI should nevertheless provide:

```text
Mask quality:
    Low
    Normal
    Native
```

---

# 18. Precision

Internally calculate the mask using `f32`.

Avoid quantizing the mathematical model to 8-bit until the final display representation.

Prefer a higher precision texture if supported.

If the final compositor path ultimately quantizes the overlay, optionally apply subtle spatial dithering to alpha.

Dithering must:

- have zero mean
- be visually imperceptible
- not shimmer
- remain spatially fixed

Do not use temporal noise.

---

# 19. Resource use

Normal operation should require essentially no active computation.

Target behavior:

```text
CPU:
    approximately idle

GPU:
    one transparent fullscreen layer per corrected output

memory:
    a few MB per display

mask recalculation:
    only after configuration changes
```

No 60-Hz application-side animation loop should run unless the platform requires presentation callbacks.

---

# 20. Monitor changes

Listen for:

- monitor connected
- monitor disconnected
- resolution changed
- scaling changed
- orientation changed
- compositor restarted

If a matching monitor disappears:

```text
destroy overlay
retain profile
```

If it returns:

```text
recreate overlay
restore profile
```

Normalized coordinates mean compensation geometry should remain meaningful when resolution changes.

Rotation needs explicit handling.

---

# 21. Startup behavior

CLI:

```text
display-compensator
display-compensator --no-gui
display-compensator --profile living-room
display-compensator --disable
display-compensator --test-pattern 50
```

Normal startup:

```text
load config
↓
enumerate displays
↓
match profiles
↓
create enabled overlays
↓
start controller/tray
```

The user should be able to enable:

```text
Start automatically on login
```

using the appropriate XDG/autostart mechanism.

---

# 22. Tray menu

Optional but useful:

```text
Display Compensation
────────────────────
✓ Living Room TV
  Laptop Screen
────────────────────
Compensation enabled ✓
Bypass
Open calibration…
────────────────────
Quit
```

Quitting must remove all overlays immediately.

---

# 23. Safety behavior

There must be multiple escape paths in case a platform bug produces an opaque fullscreen overlay.

At minimum:

```text
Esc
Ctrl+Alt+Shift+Backspace
```

when the overlay is interactive.

Also implement:

```text
display-compensator --disable
```

which contacts an existing process and disables all overlays.

A crash must naturally destroy the windows and therefore remove compensation.

The program should never alter firmware settings, EDID data, XRandR gamma tables, compositor configuration, or monitor hardware settings during normal operation.

---

# 24. Suggested source layout

```text
src/
├── main.rs
├── app.rs
├── config.rs
├── display.rs
│
├── compensation/
│   ├── mod.rs
│   ├── defect.rs
│   ├── radial.rs
│   └── mask.rs
│
├── overlay/
│   ├── mod.rs
│   ├── renderer.rs
│   └── window.rs
│
├── platform/
│   ├── mod.rs
│   ├── wayland.rs
│   └── x11.rs
│
├── gui/
│   ├── mod.rs
│   ├── main_window.rs
│   ├── defect_editor.rs
│   └── test_pattern.rs
│
└── ipc.rs
```

Core compensation mathematics must have **zero dependency on Wayland/X11/GUI code**.

That enables deterministic unit testing.

---

# 25. Suggested Rust stack

Core:

```text
serde
toml
thiserror
tracing
uuid
```

GUI:

```text
egui / eframe
```

Window/event handling:

```text
winit
```

Rendering:

```text
wgpu
```

Wayland:

```text
wayland-client
wayland-protocols
wayland-protocols-wlr
```

X11:

```text
x11rb
```

As of August 2026, current documentation shows `winit` supporting both Linux X11 and Wayland, `wgpu` providing the portable graphics abstraction, and `wayland-protocols-wlr` exposing Rust bindings for the layer-shell extension.

Do not tightly couple the core to exact versions of these libraries.

---

# 26. MVP

The first usable release should contain only:

1. X11 backend.
2. Wayland layer-shell backend.
3. Monitor selection.
4. Fullscreen transparent overlay.
5. Circular/elliptical Gaussian defects.
6. Move / resize / strength adjustment.
7. Global compensation strength.
8. Gamma adjustment.
9. Gray test patterns.
10. Instant before/after bypass.
11. Persistent TOML configuration.
12. Start-at-login option.

Do **not** initially implement:

- camera calibration
- desktop capture
- content-dependent correction
- HDR
- ICC profile manipulation
- per-channel RGB correction
- arbitrary painted masks
- automatic defect detection

Those features would substantially increase scope without proving whether the basic optical compensation works.

---

# 27. Phase 2: camera-assisted calibration

The architecture should deliberately make this possible.

Display a sequence such as:

```text
10%
25%
50%
75%
100%
```

Photograph the screen from a fixed camera position.

Then:

```text
photo
↓
detect screen corners
↓
perspective rectify
↓
estimate smooth luminance field
↓
remove camera vignetting / baseline
↓
fit radial defects
↓
generate compensation profile
```

The resulting profile still uses the exact same overlay mechanism.

No runtime camera is necessary after calibration.

---

# 28. Phase 3: RGB correction

Replace scalar:

\[
D(x,y)
\]

with:

\[
D_R(x,y),D_G(x,y),D_B(x,y)
\]

and generate:

```text
overlay RGB + alpha
```

or an equivalent attenuation representation.

This could compensate defects that are, for example:

```text
slightly yellow
slightly blue
red deficient
```

rather than purely dark.

This should **not** be part of the MVP.

---

# 29. Known limitations

The software cannot restore physical capability the panel has lost.

If a damaged area reaches only:

```text
300 nits
```

while the rest reaches:

```text
400 nits
```

software can make the panel resemble a uniform ~300-nit display.

It cannot make the damaged pixels become 400-nit pixels again.

Other limitations:

- maximum brightness decreases
- HDR behavior will be problematic
- correction may vary with viewing angle
- defects may vary as the panel warms up
- correction may vary with input luminance
- compositor blending behavior affects exact calibration
- some fullscreen/exclusive applications may bypass or cover an X11 overlay
- Wayland quality depends on compositor protocol support

The program should therefore describe itself as a **display uniformity compensator**, not a burn-in repair tool.

---

# 30. Definition of success

For an LCD/TV showing a uniform 50% gray image with several smooth circular brightness defects, a user should be able to:

```text
launch program
→ select TV
→ add circles over defects
→ adjust radius and strength visually
→ press Space repeatedly for before/after
→ reach visibly improved uniformity
→ save
→ use desktop normally
```

After calibration:

- the overlay automatically appears after login
- the user can interact normally with every application underneath it
- the compensation has no perceptible latency
- CPU usage remains essentially idle
- disabling the application instantly restores the unmodified image

That is the core product.