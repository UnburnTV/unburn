//! X11 overlay windows.
//!
//! Each corrected monitor gets a borderless, override-redirect ARGB window with
//! an empty input region, so it is topmost, invisible to window management and
//! completely transparent to the mouse. Alpha only means anything if a
//! compositing manager is running; when there is none the program says so
//! instead of quietly painting an opaque rectangle over the desktop.

use std::{
    collections::HashMap,
    os::fd::{AsFd, BorrowedFd},
    time::Duration,
};

use tracing::{debug, warn};
use x11rb::{
    connection::{Connection as _, RequestConnection as _},
    protocol::{
        randr::{self, ConnectionExt as _},
        shape::{self, ConnectionExt as _},
        xfixes::ConnectionExt as _,
        xproto::{self, ConnectionExt as _},
        Event,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
    CURRENT_TIME, NONE,
};

use crate::{
    compensation::{Mask, Vec2},
    display::{identity_from_edid, DisplayIdentity, OutputId, OutputInfo, OverlayId, Transform},
    overlay::{renderer::fill_opaque, EditorView, OverlaySurface, TestPatternState},
};

use super::{
    interaction::{Button, EditorInteraction, EditorKey, Modifiers},
    BackendError, BackendEvent, BackendKind, BackendReport, OverlayBackend, PatternAction, Result,
    Support,
};

const NO_COMPOSITOR: &str = "No compositing manager is running on this X11 display, so a \
transparent overlay cannot be blended with the desktop. Start a compositor (picom, xcompmgr, or \
your desktop's own) before enabling compensation.";

const NO_ARGB_VISUAL: &str =
    "This X11 screen has no 32-bit ARGB visual, so a transparent overlay is not possible.";

// Keysyms the overlay reacts to. X11 gives us keycodes, so these come from the
// server's own keyboard mapping.
const XK_BACKSPACE: u32 = 0xff08;
const XK_TAB: u32 = 0xff09;
const XK_ESCAPE: u32 = 0xff1b;
const XK_DELETE: u32 = 0xffff;
const XK_LEFT: u32 = 0xff51;
const XK_RIGHT: u32 = 0xff53;
const XK_SPACE: u32 = 0x0020;
const XK_B: u32 = 0x0062;
const XK_E: u32 = 0x0065;
const XK_M: u32 = 0x006d;
const XK_N: u32 = 0x006e;

const SHIFT_MASK: u16 = 1;
const CONTROL_MASK: u16 = 4;
const ALT_MASK: u16 = 8;
const SUPER_MASK: u16 = 64;

/// Report what this session can do without creating anything.
pub fn probe() -> BackendReport {
    let support = match RustConnection::connect(None) {
        Err(error) => Support::Unavailable(format!("no X11 display: {error}")),
        Ok((conn, screen_number)) => {
            let screen = &conn.setup().roots[screen_number];
            if find_argb_visual(screen).is_none() {
                Support::Unavailable(NO_ARGB_VISUAL.into())
            } else if !has_compositor(&conn, screen_number) {
                Support::Limited(NO_COMPOSITOR.into())
            } else {
                Support::Full
            }
        }
    };
    BackendReport {
        kind: BackendKind::X11,
        support,
    }
}

/// A compositing manager announces itself by owning `_NET_WM_CM_S<screen>`.
fn has_compositor(conn: &RustConnection, screen_number: usize) -> bool {
    let name = format!("_NET_WM_CM_S{screen_number}");
    let Ok(atom) = conn.intern_atom(false, name.as_bytes()) else {
        return false;
    };
    let Ok(atom) = atom.reply() else { return false };
    conn.get_selection_owner(atom.atom)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| reply.owner != NONE)
        .unwrap_or(false)
}

fn find_argb_visual(screen: &xproto::Screen) -> Option<(u32, u8)> {
    screen
        .allowed_depths
        .iter()
        .find(|depth| depth.depth == 32)
        .and_then(|depth| {
            depth
                .visuals
                .iter()
                .find(|visual| visual.class == xproto::VisualClass::TRUE_COLOR)
                .map(|visual| (visual.visual_id, depth.depth))
        })
}

struct Atoms {
    net_wm_window_type: xproto::Atom,
    net_wm_window_type_notification: xproto::Atom,
    net_wm_state: xproto::Atom,
    net_wm_state_above: xproto::Atom,
    net_wm_state_skip_taskbar: xproto::Atom,
    net_wm_state_skip_pager: xproto::Atom,
    edid: xproto::Atom,
}

impl Atoms {
    fn intern(conn: &RustConnection) -> Result<Atoms> {
        let get = |name: &str| -> Result<xproto::Atom> {
            conn.intern_atom(false, name.as_bytes())
                .map_err(protocol_error)?
                .reply()
                .map(|r| r.atom)
                .map_err(protocol_error)
        };
        Ok(Atoms {
            net_wm_window_type: get("_NET_WM_WINDOW_TYPE")?,
            net_wm_window_type_notification: get("_NET_WM_WINDOW_TYPE_NOTIFICATION")?,
            net_wm_state: get("_NET_WM_STATE")?,
            net_wm_state_above: get("_NET_WM_STATE_ABOVE")?,
            net_wm_state_skip_taskbar: get("_NET_WM_STATE_SKIP_TASKBAR")?,
            net_wm_state_skip_pager: get("_NET_WM_STATE_SKIP_PAGER")?,
            edid: get("EDID")?,
        })
    }
}

fn protocol_error(error: impl std::fmt::Display) -> BackendError {
    BackendError::Protocol(format!("X11: {error}"))
}

struct Overlay {
    output: OutputId,
    window: xproto::Window,
    pixmap: xproto::Pixmap,
    gc: xproto::Gcontext,
    geometry: Geometry,
    surface: OverlaySurface,
    mapped: bool,
    interaction: EditorInteraction,
}

struct Pattern {
    window: xproto::Window,
    geometry: Geometry,
    rgb: [u8; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geometry {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

pub struct X11Backend {
    conn: RustConnection,
    root: xproto::Window,
    visual: u32,
    depth: u8,
    colormap: xproto::Colormap,
    atoms: Atoms,
    support: Support,

    outputs: Vec<OutputInfo>,
    geometries: HashMap<OutputId, Geometry>,
    overlays: HashMap<OverlayId, Overlay>,
    patterns: HashMap<OutputId, Pattern>,

    keymap: Keymap,
    modifiers: Modifiers,
    next_id: u32,
    pending: Vec<BackendEvent>,
}

impl X11Backend {
    pub fn connect() -> Result<X11Backend> {
        let (conn, screen_number) = RustConnection::connect(None)
            .map_err(|e| BackendError::Unavailable(format!("no X11 display: {e}")))?;

        let screen = &conn.setup().roots[screen_number];
        let root = screen.root;
        let (visual, depth) = find_argb_visual(screen)
            .ok_or_else(|| BackendError::Unavailable(NO_ARGB_VISUAL.into()))?;

        conn.extension_information(randr::X11_EXTENSION_NAME)
            .map_err(protocol_error)?
            .ok_or_else(|| BackendError::Unavailable("this X server has no RandR".into()))?;
        conn.extension_information(shape::X11_EXTENSION_NAME)
            .map_err(protocol_error)?
            .ok_or_else(|| BackendError::Unavailable("this X server has no SHAPE".into()))?;
        conn.xfixes_query_version(5, 0)
            .map_err(protocol_error)?
            .reply()
            .map_err(|_| BackendError::Unavailable("this X server has no XFIXES".into()))?;

        let colormap = conn.generate_id().map_err(protocol_error)?;
        conn.create_colormap(xproto::ColormapAlloc::NONE, colormap, root, visual)
            .map_err(protocol_error)?;

        let atoms = Atoms::intern(&conn)?;
        let support = if has_compositor(&conn, screen_number) {
            Support::Full
        } else {
            warn!("{NO_COMPOSITOR}");
            Support::Limited(NO_COMPOSITOR.into())
        };

        // Tell us when a monitor is plugged, unplugged, resized or rotated.
        conn.randr_select_input(
            root,
            randr::NotifyMask::SCREEN_CHANGE
                | randr::NotifyMask::CRTC_CHANGE
                | randr::NotifyMask::OUTPUT_CHANGE,
        )
        .map_err(protocol_error)?;

        let keymap = Keymap::fetch(&conn)?;

        let mut backend = X11Backend {
            conn,
            root,
            visual,
            depth,
            colormap,
            atoms,
            support,
            outputs: Vec::new(),
            geometries: HashMap::new(),
            overlays: HashMap::new(),
            patterns: HashMap::new(),
            keymap,
            modifiers: Modifiers::default(),
            next_id: 0,
            pending: Vec::new(),
        };

        backend.grab_bypass_hotkey();
        backend.refresh_outputs()?;
        backend.conn.flush().map_err(protocol_error)?;
        Ok(backend)
    }

    /// Take Super+Shift+B system-wide, so bypass works without the GUI focused.
    fn grab_bypass_hotkey(&self) {
        let Some(keycode) = self.keymap.keycode_for(XK_B) else {
            debug!("no key is bound to B; the bypass hotkey is unavailable");
            return;
        };
        // Also grab the combinations that include the lock modifiers, or the
        // hotkey silently stops working with caps or num lock on.
        for extra in [0u16, 2 /* caps */, 16 /* num */, 18] {
            let modifiers = xproto::ModMask::from(SUPER_MASK | SHIFT_MASK | extra);
            if self
                .conn
                .grab_key(
                    true,
                    self.root,
                    modifiers,
                    keycode,
                    xproto::GrabMode::ASYNC,
                    xproto::GrabMode::ASYNC,
                )
                .is_err()
            {
                debug!("another program already owns Super+Shift+B");
                return;
            }
        }
    }

    fn allocate_id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    /// Re-enumerate monitors from RandR.
    fn refresh_outputs(&mut self) -> Result<()> {
        let resources = self
            .conn
            .randr_get_screen_resources_current(self.root)
            .map_err(protocol_error)?
            .reply()
            .map_err(protocol_error)?;

        let mut discovered: Vec<(String, OutputInfo)> = Vec::new();

        for output in resources.outputs.iter().copied() {
            let Ok(info) = self
                .conn
                .randr_get_output_info(output, resources.config_timestamp)
            else {
                continue;
            };
            let Ok(info) = info.reply() else { continue };
            if info.connection != randr::Connection::CONNECTED || info.crtc == NONE {
                continue;
            }
            let Ok(crtc) = self
                .conn
                .randr_get_crtc_info(info.crtc, resources.config_timestamp)
            else {
                continue;
            };
            let Ok(crtc) = crtc.reply() else { continue };

            let connector = String::from_utf8_lossy(&info.name).into_owned();
            let mut identity = self.read_edid(output).unwrap_or_default();
            identity.connector = Some(connector.clone());

            discovered.push((
                connector,
                OutputInfo {
                    // Filled in below once the identifier is known.
                    id: OutputId(0),
                    identity,
                    width: crtc.width as u32,
                    height: crtc.height as u32,
                    position: (crtc.x as i32, crtc.y as i32),
                    scale: 1.0,
                    transform: convert_rotation(crtc.rotation),
                    refresh_mhz: resources
                        .modes
                        .iter()
                        .find(|m| m.id == crtc.mode)
                        .map(refresh_mhz),
                },
            ));
        }

        // Keep identifiers stable across refreshes so live overlays survive a
        // mode change on some other monitor.
        let mut outputs = Vec::new();
        let mut geometries = HashMap::new();
        for (connector, mut info) in discovered {
            let existing = self
                .outputs
                .iter()
                .find(|o| o.identity.connector.as_deref() == Some(connector.as_str()));
            info.id = match existing {
                Some(previous) => previous.id,
                None => {
                    self.next_id += 1;
                    OutputId(self.next_id)
                }
            };
            geometries.insert(
                info.id,
                Geometry {
                    x: info.position.0 as i16,
                    y: info.position.1 as i16,
                    width: info.width.max(1) as u16,
                    height: info.height.max(1) as u16,
                },
            );
            outputs.push(info);
        }

        if outputs != self.outputs {
            self.outputs = outputs;
            self.geometries = geometries;
            self.pending
                .push(BackendEvent::OutputsChanged(self.outputs.clone()));
        }
        Ok(())
    }

    fn read_edid(&self, output: randr::Output) -> Option<DisplayIdentity> {
        let reply = self
            .conn
            .randr_get_output_property(
                output,
                self.atoms.edid,
                u32::from(xproto::AtomEnum::ANY),
                0,
                256,
                false,
                false,
            )
            .ok()?
            .reply()
            .ok()?;
        (!reply.data.is_empty()).then(|| identity_from_edid(&reply.data))
    }

    fn geometry_of(&self, output: OutputId) -> Option<Geometry> {
        self.geometries.get(&output).copied()
    }

    /// Create a borderless, override-redirect ARGB window covering `geometry`.
    fn create_window(&self, geometry: Geometry, interactive: bool) -> Result<xproto::Window> {
        let window = self.conn.generate_id().map_err(protocol_error)?;
        let mut events = xproto::EventMask::EXPOSURE | xproto::EventMask::VISIBILITY_CHANGE;
        if interactive {
            events |= xproto::EventMask::BUTTON_PRESS
                | xproto::EventMask::BUTTON_RELEASE
                | xproto::EventMask::POINTER_MOTION
                | xproto::EventMask::KEY_PRESS;
        }

        let values = xproto::CreateWindowAux::new()
            // A 32-bit window inherits neither of these from a 24-bit parent.
            .background_pixel(0)
            .border_pixel(0)
            .colormap(self.colormap)
            // Keeps the window out of window management entirely: no frame, no
            // focus, no task switcher entry.
            .override_redirect(1)
            .event_mask(events);

        self.conn
            .create_window(
                self.depth,
                window,
                self.root,
                geometry.x,
                geometry.y,
                geometry.width,
                geometry.height,
                0,
                xproto::WindowClass::INPUT_OUTPUT,
                self.visual,
                &values,
            )
            .map_err(protocol_error)?;

        // Hints for the compositor, which still sees the window even though
        // the window manager does not.
        self.conn
            .change_property32(
                xproto::PropMode::REPLACE,
                window,
                self.atoms.net_wm_window_type,
                xproto::AtomEnum::ATOM,
                &[self.atoms.net_wm_window_type_notification],
            )
            .map_err(protocol_error)?;
        self.conn
            .change_property32(
                xproto::PropMode::REPLACE,
                window,
                self.atoms.net_wm_state,
                xproto::AtomEnum::ATOM,
                &[
                    self.atoms.net_wm_state_above,
                    self.atoms.net_wm_state_skip_taskbar,
                    self.atoms.net_wm_state_skip_pager,
                ],
            )
            .map_err(protocol_error)?;
        self.conn
            .change_property8(
                xproto::PropMode::REPLACE,
                window,
                xproto::AtomEnum::WM_NAME,
                xproto::AtomEnum::STRING,
                b"unburn",
            )
            .map_err(protocol_error)?;

        Ok(window)
    }

    /// Make every pointer event pass straight through the window.
    fn set_click_through(&self, window: xproto::Window, click_through: bool) -> Result<()> {
        if click_through {
            let region = self.conn.generate_id().map_err(protocol_error)?;
            self.conn
                .xfixes_create_region(region, &[])
                .map_err(protocol_error)?;
            self.conn
                .xfixes_set_window_shape_region(window, shape::SK::INPUT, 0, 0, region)
                .map_err(protocol_error)?;
            self.conn
                .xfixes_destroy_region(region)
                .map_err(protocol_error)?;
        } else {
            // A null shape means the window's whole area accepts input again.
            self.conn
                .shape_mask(shape::SO::SET, shape::SK::INPUT, window, 0, 0, NONE)
                .map_err(protocol_error)?;
        }
        Ok(())
    }

    /// Upload a framebuffer into an overlay's backing pixmap.
    ///
    /// `PutImage` requests are chunked into horizontal bands so a 4K frame does
    /// not exceed the server's maximum request length.
    fn upload(&self, overlay: &Overlay, pixels: &[u8]) -> Result<()> {
        let width = overlay.geometry.width;
        let height = overlay.geometry.height;
        let stride = width as usize * 4;
        if stride == 0 {
            return Ok(());
        }

        let budget = self
            .conn
            .maximum_request_bytes()
            .saturating_sub(64)
            .max(stride);
        let rows_per_band = (budget / stride).clamp(1, height as usize);

        let mut y = 0usize;
        while y < height as usize {
            let rows = rows_per_band.min(height as usize - y);
            let start = y * stride;
            let end = start + rows * stride;
            let band = pixels.get(start..end).unwrap_or(&[]);
            if band.len() < rows * stride {
                break;
            }
            self.conn
                .put_image(
                    xproto::ImageFormat::Z_PIXMAP,
                    overlay.pixmap,
                    overlay.gc,
                    width,
                    rows as u16,
                    0,
                    y as i16,
                    0,
                    self.depth,
                    band,
                )
                .map_err(protocol_error)?;
            y += rows;
        }
        Ok(())
    }

    fn present(&mut self, id: OverlayId) -> Result<()> {
        let Some(overlay) = self.overlays.get_mut(&id) else {
            return Ok(());
        };
        let has_new = overlay.surface.frame().is_some();
        let should_map = overlay.surface.is_visible();

        if has_new {
            let pixels = overlay.surface.pixels().to_vec();
            let overlay = self.overlays.get(&id).expect("checked");
            self.upload(overlay, &pixels)?;
            // Repainting from the background pixmap keeps expose handling in
            // the server rather than in an application-side redraw loop.
            self.conn
                .change_window_attributes(
                    overlay.window,
                    &xproto::ChangeWindowAttributesAux::new().background_pixmap(overlay.pixmap),
                )
                .map_err(protocol_error)?;
            self.conn
                .clear_area(false, overlay.window, 0, 0, 0, 0)
                .map_err(protocol_error)?;
        }

        let overlay = self.overlays.get_mut(&id).expect("checked");
        if overlay.mapped != should_map {
            if should_map {
                self.conn
                    .map_window(overlay.window)
                    .map_err(protocol_error)?;
                raise(&self.conn, overlay.window)?;
            } else {
                // Unmapping is the instant bypass: nothing is recomputed and
                // the mask is still sitting in the pixmap when it comes back.
                self.conn
                    .unmap_window(overlay.window)
                    .map_err(protocol_error)?;
            }
            overlay.mapped = should_map;
        }
        Ok(())
    }

    fn overlay_by_window(&self, window: xproto::Window) -> Option<OverlayId> {
        self.overlays
            .iter()
            .find(|(_, o)| o.window == window)
            .map(|(id, _)| *id)
    }

    fn pattern_owns(&self, window: xproto::Window) -> bool {
        self.patterns.values().any(|p| p.window == window)
    }

    fn handle_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::RandrScreenChangeNotify(_)
            | Event::RandrNotify(_)
            | Event::ConfigureNotify(_) => {
                self.refresh_outputs()?;
            }
            Event::VisibilityNotify(notify) => {
                // Something was stacked above the overlay; put it back on top.
                if notify.state != xproto::Visibility::UNOBSCURED {
                    if let Some(id) = self.overlay_by_window(notify.window) {
                        if self.overlays.get(&id).is_some_and(|o| o.mapped) {
                            raise(&self.conn, notify.window)?;
                        }
                    }
                }
            }
            Event::KeyPress(key) => self.handle_key(key)?,
            Event::ButtonPress(press) => {
                let uv = self.normalize(press.event, press.event_x, press.event_y);
                self.update_modifiers(press.state.into());
                let action = match press.detail {
                    1 => self.with_interaction(press.event, |i| i.press(uv, Button::Primary)),
                    3 => self.with_interaction(press.event, |i| i.press(uv, Button::Secondary)),
                    // Buttons 4 and 5 are the scroll wheel.
                    4 => self.with_interaction(press.event, |i| i.wheel(1.0)),
                    5 => self.with_interaction(press.event, |i| i.wheel(-1.0)),
                    _ => None,
                };
                self.emit(action);
            }
            Event::ButtonRelease(release) => {
                self.with_interaction::<()>(release.event, |i| i.release());
            }
            Event::MotionNotify(motion) => {
                let uv = self.normalize(motion.event, motion.event_x, motion.event_y);
                self.update_modifiers(motion.state.into());
                let action = self.with_interaction(motion.event, |i| i.motion(uv));
                self.emit(action);
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_key(&mut self, key: xproto::KeyPressEvent) -> Result<()> {
        self.update_modifiers(key.state.into());
        let keysym = self.keymap.keysym_for(key.detail, self.modifiers.shift);

        // The system-wide bypass hotkey arrives on the root window.
        if key.event == self.root {
            if keysym == Some(XK_B) && self.modifiers.logo && self.modifiers.shift {
                self.pending
                    .push(BackendEvent::Pattern(PatternAction::ToggleCompensation));
            }
            return Ok(());
        }

        if self.pattern_owns(key.event) {
            let action = match keysym {
                Some(XK_ESCAPE) => Some(PatternAction::Exit),
                Some(XK_SPACE) => Some(PatternAction::ToggleCompensation),
                Some(XK_LEFT) => Some(PatternAction::Previous),
                Some(XK_RIGHT) => Some(PatternAction::Next),
                _ => None,
            };
            if let Some(action) = action {
                self.pending.push(BackendEvent::Pattern(action));
            }
            return Ok(());
        }

        let editor_key = match keysym {
            Some(XK_ESCAPE) => EditorKey::Escape,
            Some(XK_DELETE) => EditorKey::Delete,
            Some(XK_BACKSPACE) => EditorKey::Backspace,
            Some(XK_TAB) => EditorKey::Tab,
            Some(XK_N) => EditorKey::NewDefect,
            Some(XK_M) => EditorKey::CycleShowMode,
            Some(XK_E) => EditorKey::ToggleSelected,
            _ => return Ok(()),
        };
        let action = self.with_interaction(key.event, |i| i.key(editor_key));
        self.emit(action);
        Ok(())
    }

    fn update_modifiers(&mut self, state: u16) {
        self.modifiers = Modifiers {
            shift: state & SHIFT_MASK != 0,
            ctrl: state & CONTROL_MASK != 0,
            alt: state & ALT_MASK != 0,
            logo: state & SUPER_MASK != 0,
        };
        let modifiers = self.modifiers;
        for overlay in self.overlays.values_mut() {
            overlay.interaction.set_modifiers(modifiers);
        }
    }

    fn normalize(&self, window: xproto::Window, x: i16, y: i16) -> Vec2 {
        let Some(overlay) = self.overlays.values().find(|o| o.window == window) else {
            return Vec2::ZERO;
        };
        Vec2::new(
            x as f32 / overlay.geometry.width.max(1) as f32,
            y as f32 / overlay.geometry.height.max(1) as f32,
        )
    }

    fn with_interaction<T>(
        &mut self,
        window: xproto::Window,
        f: impl FnOnce(&mut EditorInteraction) -> T,
    ) -> T
    where
        T: Default,
    {
        match self.overlays.values_mut().find(|o| o.window == window) {
            Some(overlay) if overlay.surface.is_interactive() => f(&mut overlay.interaction),
            _ => T::default(),
        }
    }

    fn emit(&mut self, action: Option<super::EditorAction>) {
        if let Some(action) = action {
            self.pending.push(BackendEvent::Editor(action));
        }
    }
}

fn raise(conn: &RustConnection, window: xproto::Window) -> Result<()> {
    conn.configure_window(
        window,
        &xproto::ConfigureWindowAux::new().stack_mode(xproto::StackMode::ABOVE),
    )
    .map_err(protocol_error)?;
    Ok(())
}

fn convert_rotation(rotation: randr::Rotation) -> Transform {
    let flipped = rotation.contains(randr::Rotation::REFLECT_X);
    if rotation.contains(randr::Rotation::ROTATE90) {
        if flipped {
            Transform::FlippedRotate90
        } else {
            Transform::Rotate90
        }
    } else if rotation.contains(randr::Rotation::ROTATE180) {
        if flipped {
            Transform::FlippedRotate180
        } else {
            Transform::Rotate180
        }
    } else if rotation.contains(randr::Rotation::ROTATE270) {
        if flipped {
            Transform::FlippedRotate270
        } else {
            Transform::Rotate270
        }
    } else if flipped {
        Transform::Flipped
    } else {
        Transform::Normal
    }
}

fn refresh_mhz(mode: &randr::ModeInfo) -> u32 {
    let total = mode.htotal as u64 * mode.vtotal as u64;
    if total == 0 {
        return 0;
    }
    ((mode.dot_clock as u64 * 1000) / total) as u32
}

/// The server's keycode-to-keysym table.
struct Keymap {
    min_keycode: u8,
    per_keycode: u8,
    keysyms: Vec<u32>,
}

impl Keymap {
    fn fetch(conn: &RustConnection) -> Result<Keymap> {
        let setup = conn.setup();
        let min = setup.min_keycode;
        let max = setup.max_keycode;
        let count = max - min + 1;
        let mapping = conn
            .get_keyboard_mapping(min, count)
            .map_err(protocol_error)?
            .reply()
            .map_err(protocol_error)?;
        Ok(Keymap {
            min_keycode: min,
            per_keycode: mapping.keysyms_per_keycode,
            keysyms: mapping.keysyms,
        })
    }

    fn keysym_for(&self, keycode: u8, shift: bool) -> Option<u32> {
        if keycode < self.min_keycode || self.per_keycode == 0 {
            return None;
        }
        let base = (keycode - self.min_keycode) as usize * self.per_keycode as usize;
        let level = if shift && self.per_keycode > 1 { 1 } else { 0 };
        let shifted = self.keysyms.get(base + level).copied().filter(|s| *s != 0);
        shifted.or_else(|| self.keysyms.get(base).copied().filter(|s| *s != 0))
    }

    fn keycode_for(&self, keysym: u32) -> Option<u8> {
        let position = self.keysyms.iter().position(|s| *s == keysym)?;
        Some(self.min_keycode + (position / self.per_keycode.max(1) as usize) as u8)
    }
}

impl OverlayBackend for X11Backend {
    fn kind(&self) -> BackendKind {
        BackendKind::X11
    }

    fn report(&self) -> BackendReport {
        BackendReport {
            kind: BackendKind::X11,
            support: self.support.clone(),
        }
    }

    fn outputs(&self) -> Vec<OutputInfo> {
        self.outputs.clone()
    }

    fn create_overlay(&mut self, output: OutputId) -> Result<OverlayId> {
        let geometry = self
            .geometry_of(output)
            .ok_or(BackendError::UnknownOutput)?;
        let transform = self
            .outputs
            .iter()
            .find(|o| o.id == output)
            .map(|o| o.transform)
            .unwrap_or(Transform::Normal);

        let window = self.create_window(geometry, true)?;
        self.set_click_through(window, true)?;

        let pixmap = self.conn.generate_id().map_err(protocol_error)?;
        self.conn
            .create_pixmap(self.depth, pixmap, window, geometry.width, geometry.height)
            .map_err(protocol_error)?;
        let gc = self.conn.generate_id().map_err(protocol_error)?;
        self.conn
            .create_gc(
                gc,
                pixmap,
                &xproto::CreateGCAux::new().graphics_exposures(0),
            )
            .map_err(protocol_error)?;

        let id = OverlayId(self.allocate_id());
        self.overlays.insert(
            id,
            Overlay {
                output,
                window,
                pixmap,
                gc,
                geometry,
                surface: OverlaySurface::new(geometry.width as u32, geometry.height as u32),
                mapped: false,
                interaction: EditorInteraction::new(transform),
            },
        );
        debug!(?id, ?geometry, "created an X11 overlay window");
        Ok(id)
    }

    fn destroy_overlay(&mut self, overlay: OverlayId) {
        let Some(overlay) = self.overlays.remove(&overlay) else {
            return;
        };
        // Destroying the window is what removes the compensation, which is also
        // exactly what happens if the process dies.
        self.conn.free_gc(overlay.gc).ok();
        self.conn.free_pixmap(overlay.pixmap).ok();
        self.conn.destroy_window(overlay.window).ok();
        self.conn.flush().ok();
    }

    fn set_interactive(&mut self, overlay: OverlayId, interactive: bool) {
        let Some(entry) = self.overlays.get_mut(&overlay) else {
            return;
        };
        if entry.surface.is_interactive() == interactive {
            return;
        }
        entry.surface.set_interactive(interactive);
        let window = entry.window;
        if !interactive {
            entry.interaction.release();
        }

        if let Err(error) = self.set_click_through(window, !interactive) {
            warn!(%error, "could not change the overlay's input region");
        }
        if interactive {
            // Override-redirect windows are never focused by the window
            // manager, so grab the keyboard for the duration of the edit.
            self.conn
                .grab_keyboard(
                    true,
                    window,
                    CURRENT_TIME,
                    xproto::GrabMode::ASYNC,
                    xproto::GrabMode::ASYNC,
                )
                .ok();
        } else {
            self.conn.ungrab_keyboard(CURRENT_TIME).ok();
        }
        self.conn.flush().ok();
    }

    fn set_visible(&mut self, overlay: OverlayId, visible: bool) {
        if let Some(entry) = self.overlays.get_mut(&overlay) {
            entry.surface.set_visible(visible);
        }
    }

    fn update_mask(&mut self, overlay: OverlayId, mask: &Mask) {
        if let Some(entry) = self.overlays.get_mut(&overlay) {
            entry.surface.set_mask(mask);
        }
    }

    fn set_editor(&mut self, overlay: OverlayId, editor: Option<EditorView>) {
        let transform = self
            .overlays
            .get(&overlay)
            .and_then(|o| self.outputs.iter().find(|out| out.id == o.output))
            .map(|o| o.transform)
            .unwrap_or(Transform::Normal);
        let Some(entry) = self.overlays.get_mut(&overlay) else {
            return;
        };
        if let Some(view) = editor.clone() {
            entry.interaction.set_view(view, transform);
        }
        entry.surface.set_editor(editor);
    }

    fn set_model(&mut self, overlay: OverlayId, model: Option<Mask>) {
        if let Some(entry) = self.overlays.get_mut(&overlay) {
            entry.surface.set_model(model);
        }
    }

    fn set_dither(&mut self, overlay: OverlayId, dither: bool) {
        if let Some(entry) = self.overlays.get_mut(&overlay) {
            entry.surface.set_dither(dither);
        }
    }

    fn set_test_pattern(&mut self, output: OutputId, pattern: Option<TestPatternState>) {
        let Some(state) = pattern else {
            if let Some(pattern) = self.patterns.remove(&output) {
                self.conn.destroy_window(pattern.window).ok();
                self.conn.flush().ok();
            }
            return;
        };
        let Some(geometry) = self.geometry_of(output) else {
            return;
        };
        let rgb = state.pattern.rgb();

        if let Some(existing) = self.patterns.get(&output) {
            if existing.rgb == rgb && existing.geometry == geometry {
                return;
            }
            let window = existing.window;
            self.patterns.remove(&output);
            self.conn.destroy_window(window).ok();
        }

        let window = match self.create_window(geometry, true) {
            Ok(window) => window,
            Err(error) => {
                warn!(%error, "could not create the test pattern window");
                return;
            }
        };

        // An opaque fill: the pattern is what the compensation is judged
        // against, so it must not be transparent itself.
        let mut pixels = vec![0u8; geometry.width as usize * geometry.height as usize * 4];
        fill_opaque(&mut pixels, rgb);

        let pixmap = self.conn.generate_id().ok();
        let gc = self.conn.generate_id().ok();
        if let (Some(pixmap), Some(gc)) = (pixmap, gc) {
            let created = self
                .conn
                .create_pixmap(self.depth, pixmap, window, geometry.width, geometry.height)
                .is_ok()
                && self
                    .conn
                    .create_gc(
                        gc,
                        pixmap,
                        &xproto::CreateGCAux::new().graphics_exposures(0),
                    )
                    .is_ok();
            if created {
                let dummy = Overlay {
                    output,
                    window,
                    pixmap,
                    gc,
                    geometry,
                    surface: OverlaySurface::new(1, 1),
                    mapped: false,
                    interaction: EditorInteraction::new(Transform::Normal),
                };
                self.upload(&dummy, &pixels).ok();
                self.conn
                    .change_window_attributes(
                        window,
                        &xproto::ChangeWindowAttributesAux::new().background_pixmap(pixmap),
                    )
                    .ok();
                self.conn.free_gc(gc).ok();
                self.conn.free_pixmap(pixmap).ok();
            }
        }

        self.conn.map_window(window).ok();
        // Below every compensation overlay, so the correction applies to it.
        for overlay in self.overlays.values() {
            if overlay.mapped {
                raise(&self.conn, overlay.window).ok();
            }
        }
        self.conn
            .grab_keyboard(
                true,
                window,
                CURRENT_TIME,
                xproto::GrabMode::ASYNC,
                xproto::GrabMode::ASYNC,
            )
            .ok();
        self.conn.flush().ok();

        self.patterns.insert(
            output,
            Pattern {
                window,
                geometry,
                rgb,
            },
        );
    }

    fn flush(&mut self) -> Result<()> {
        let ids: Vec<OverlayId> = self.overlays.keys().copied().collect();
        for id in ids {
            self.present(id)?;
        }
        self.conn.flush().map_err(protocol_error)
    }

    fn poll_events(
        &mut self,
        wake: BorrowedFd<'_>,
        timeout: Option<Duration>,
        events: &mut Vec<BackendEvent>,
    ) -> Result<()> {
        self.conn.flush().map_err(protocol_error)?;

        // Anything already queued means there is no point sleeping.
        let mut event = self.conn.poll_for_event().map_err(protocol_error)?;
        if event.is_none() && self.pending.is_empty() {
            let x11_fd = self.conn.stream().as_fd();
            let mut fds = [
                rustix::event::PollFd::new(&x11_fd, rustix::event::PollFlags::IN),
                rustix::event::PollFd::new(&wake, rustix::event::PollFlags::IN),
            ];
            let spec = timeout.map(|t| rustix::event::Timespec {
                tv_sec: t.as_secs() as i64,
                tv_nsec: t.subsec_nanos() as _,
            });
            match rustix::event::poll(&mut fds, spec.as_ref()) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => {}
                Err(error) => return Err(BackendError::Io(error.into())),
            }
            event = self.conn.poll_for_event().map_err(protocol_error)?;
        }

        while let Some(next) = event {
            self.handle_event(next)?;
            event = self.conn.poll_for_event().map_err(protocol_error)?;
        }

        events.append(&mut self.pending);
        Ok(())
    }
}

impl Drop for X11Backend {
    fn drop(&mut self) {
        let windows: Vec<xproto::Window> = self
            .overlays
            .values()
            .map(|o| o.window)
            .chain(self.patterns.values().map(|p| p.window))
            .collect();
        for window in windows {
            self.conn.destroy_window(window).ok();
        }
        self.conn.flush().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn randr_rotations_map_onto_transforms() {
        assert_eq!(
            convert_rotation(randr::Rotation::ROTATE0),
            Transform::Normal
        );
        assert_eq!(
            convert_rotation(randr::Rotation::ROTATE90),
            Transform::Rotate90
        );
        assert_eq!(
            convert_rotation(randr::Rotation::ROTATE180 | randr::Rotation::REFLECT_X),
            Transform::FlippedRotate180
        );
    }

    #[test]
    fn refresh_rate_is_derived_from_the_mode_timings() {
        let mode = randr::ModeInfo {
            id: 0,
            width: 1920,
            height: 1080,
            dot_clock: 148_500_000,
            hsync_start: 0,
            hsync_end: 0,
            htotal: 2200,
            hskew: 0,
            vsync_start: 0,
            vsync_end: 0,
            vtotal: 1125,
            name_len: 0,
            mode_flags: randr::ModeFlag::from(0u32),
        };
        // 148.5 MHz over 2200x1125 is the standard 60 Hz 1080p mode.
        assert_eq!(refresh_mhz(&mode), 60_000);
    }

    #[test]
    fn an_empty_mode_does_not_divide_by_zero() {
        let mode = randr::ModeInfo {
            id: 0,
            width: 0,
            height: 0,
            dot_clock: 0,
            hsync_start: 0,
            hsync_end: 0,
            htotal: 0,
            hskew: 0,
            vsync_start: 0,
            vsync_end: 0,
            vtotal: 0,
            name_len: 0,
            mode_flags: randr::ModeFlag::from(0u32),
        };
        assert_eq!(refresh_mhz(&mode), 0);
    }

    #[test]
    fn the_keymap_resolves_shift_levels() {
        let keymap = Keymap {
            min_keycode: 8,
            per_keycode: 2,
            keysyms: vec![0x61, 0x41, 0x62, 0x42],
        };
        assert_eq!(keymap.keysym_for(8, false), Some(0x61));
        assert_eq!(keymap.keysym_for(8, true), Some(0x41));
        assert_eq!(keymap.keycode_for(0x62), Some(9));
        assert_eq!(keymap.keysym_for(3, false), None);
    }
}
