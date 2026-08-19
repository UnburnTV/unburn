//! Wayland overlay windows built on `wlr-layer-shell-v1`.
//!
//! Wayland gives an ordinary client no control over the compositor's finished
//! framebuffer, so the compensation layer has to be a surface of its own. The
//! layer shell is the protocol that grants exactly the properties needed: a
//! defined place in the z-order, edge anchoring, no reserved space and explicit
//! input semantics. Where it is missing, a real compensation layer cannot be
//! guaranteed and the program says so rather than pretending otherwise.

use std::{collections::HashMap, os::fd::BorrowedFd, time::Duration};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers as SctkModifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{
        slot::{Buffer, SlotPool},
        Shm, ShmHandler,
    },
};
use tracing::{debug, warn};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, EventQueue, QueueHandle,
};

use crate::{
    compensation::{Mask, Vec2},
    display::{DisplayIdentity, OutputId, OutputInfo, OverlayId, Transform},
    overlay::{renderer::fill_opaque, EditorView, OverlaySurface, TestPatternState},
};

use super::{
    identity::{self, IdentitySource},
    interaction::{Button, EditorInteraction, EditorKey, Modifiers},
    BackendError, BackendEvent, BackendKind, BackendReport, OverlayBackend, PatternAction, Result,
    Support,
};

/// The layer shell is what makes a guaranteed compensation layer possible.
const LAYER_SHELL_MISSING: &str = "Your compositor does not provide the layer-shell protocol \
required for a guaranteed always-on-top compensation layer. unburn can only offer an ordinary \
window, which the compositor may place below other windows or give input focus to.";

/// Report what this session can do without creating anything.
pub fn probe() -> BackendReport {
    let support = match Connection::connect_to_env() {
        Err(error) => Support::Unavailable(format!("no Wayland display: {error}")),
        Ok(conn) => match registry_queue_init::<ProbeState>(&conn) {
            Err(error) => Support::Unavailable(format!("Wayland registry: {error}")),
            Ok((globals, _)) => {
                let has_layer_shell = globals
                    .contents()
                    .with_list(|list| list.iter().any(|g| g.interface == "zwlr_layer_shell_v1"));
                if has_layer_shell {
                    Support::Full
                } else {
                    Support::Limited(LAYER_SHELL_MISSING.to_string())
                }
            }
        },
    };
    BackendReport {
        kind: BackendKind::Wayland,
        support,
    }
}

/// Minimal state used only to enumerate globals during [`probe`].
struct ProbeState;
delegate_registry!(ProbeState);
impl ProvidesRegistryState for ProbeState {
    fn registry(&mut self) -> &mut RegistryState {
        unreachable!("the probe never dispatches registry events")
    }
    registry_handlers![];
}
smithay_client_toolkit::delegate_dispatch2!(ProbeState);

pub struct WaylandBackend {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
}

impl WaylandBackend {
    pub fn connect() -> Result<WaylandBackend> {
        let conn = Connection::connect_to_env()
            .map_err(|e| BackendError::Unavailable(format!("no Wayland display: {e}")))?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)
            .map_err(|e| BackendError::Protocol(format!("Wayland registry: {e}")))?;
        let qh = queue.handle();

        let compositor = CompositorState::bind(&globals, &qh)
            .map_err(|e| BackendError::Protocol(format!("wl_compositor: {e}")))?;
        let shm =
            Shm::bind(&globals, &qh).map_err(|e| BackendError::Protocol(format!("wl_shm: {e}")))?;
        let layer_shell = LayerShell::bind(&globals, &qh).ok();
        if layer_shell.is_none() {
            warn!("{LAYER_SHELL_MISSING}");
        }
        let pool = SlotPool::new(4 * 1024 * 1024, &shm)
            .map_err(|e| BackendError::Protocol(format!("shm pool: {e}")))?;

        let mut state = State {
            registry_state: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &qh),
            seat_state: SeatState::new(&globals, &qh),
            shm,
            compositor,
            layer_shell,
            pool,
            outputs: Vec::new(),
            overlays: HashMap::new(),
            patterns: HashMap::new(),
            // Probed before the round trips below, so that even the initial
            // output list arrives with serial numbers attached.
            identity: identity::detect(),
            keyboard: None,
            pointer: None,
            modifiers: Modifiers::default(),
            focus: None,
            events: Vec::new(),
            next_id: 0,
            qh: qh.clone(),
        };

        // Two round trips: the first binds the outputs, the second delivers
        // their properties (including the xdg-output name we identify them by).
        queue
            .roundtrip(&mut state)
            .map_err(|e| BackendError::Protocol(format!("Wayland roundtrip: {e}")))?;
        queue
            .roundtrip(&mut state)
            .map_err(|e| BackendError::Protocol(format!("Wayland roundtrip: {e}")))?;
        state.refresh_outputs();

        Ok(WaylandBackend { conn, queue, state })
    }
}

struct TrackedOutput {
    id: OutputId,
    output: wl_output::WlOutput,
    info: OutputInfo,
}

struct Overlay {
    layer: LayerSurface,
    surface: OverlaySurface,
    buffer: Option<Buffer>,
    buffer_size: (u32, u32),
    /// The compositor has told us how large the surface is.
    configured: bool,
    /// Pixels currently attached differ from what we want to show.
    dirty: bool,
    /// Whether the last attached buffer was the fully transparent one.
    blanked: bool,
    interaction: EditorInteraction,
    transform: Transform,
}

struct Pattern {
    layer: LayerSurface,
    buffer: Option<Buffer>,
    size: (u32, u32),
    rgb: [u8; 3],
    configured: bool,
    dirty: bool,
}

struct State {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    compositor: CompositorState,
    layer_shell: Option<LayerShell>,
    pool: SlotPool,
    qh: QueueHandle<State>,

    outputs: Vec<TrackedOutput>,
    overlays: HashMap<OverlayId, Overlay>,
    patterns: HashMap<OutputId, Pattern>,

    /// Whoever in this session can name the monitors, if anyone can.
    ///
    /// Kept on the state rather than the backend because this is where the
    /// output list is assembled. The source talks to the compositor over its own
    /// connection, so nothing here interleaves with this queue.
    identity: Option<Box<dyn IdentitySource>>,

    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    modifiers: Modifiers,
    focus: Option<wl_surface::WlSurface>,

    events: Vec<BackendEvent>,
    next_id: u32,
}

impl State {
    fn allocate_id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    /// Rebuild the output list from what the compositor has told us.
    fn refresh_outputs(&mut self) {
        // The compositor's own view is collected in full first, so the identity
        // source can be asked once for the whole set rather than once per
        // monitor: a query is a Wayland roundtrip or a D-Bus call.
        let mut handles = Vec::new();
        let mut infos = Vec::new();
        for output in self.output_state.outputs() {
            let Some(info) = self.output_state.info(&output) else {
                continue;
            };
            let id = match self.outputs.iter().position(|t| t.output == output) {
                Some(index) => self.outputs[index].id,
                None => {
                    self.next_id += 1;
                    OutputId(self.next_id)
                }
            };
            handles.push(output);
            infos.push(convert_output(id, &info));
        }

        // `wl_output` has no field for a serial number, so this is the only
        // chance to tell two units of one model apart. It happens before the
        // comparison below so that the stored list, and the event announcing it,
        // both carry whatever was learned.
        if let Some(source) = self.identity.as_deref_mut() {
            identity::enrich(&mut infos, source);
        }

        let mut seen = Vec::new();
        let mut changed = false;
        for (output, info) in handles.into_iter().zip(infos) {
            let id = info.id;
            match self.outputs.iter().position(|t| t.output == output) {
                Some(index) => {
                    if self.outputs[index].info != info {
                        self.outputs[index].info = info;
                        changed = true;
                    }
                }
                None => {
                    self.outputs.push(TrackedOutput { id, output, info });
                    changed = true;
                }
            }
            seen.push(id);
        }

        let before = self.outputs.len();
        self.outputs.retain(|t| seen.contains(&t.id));
        if self.outputs.len() != before {
            changed = true;
        }

        if changed {
            let list: Vec<OutputInfo> = self.outputs.iter().map(|t| t.info.clone()).collect();
            self.events.push(BackendEvent::OutputsChanged(list));
        }
    }

    fn output_of(&self, id: OutputId) -> Option<&TrackedOutput> {
        self.outputs.iter().find(|t| t.id == id)
    }

    fn overlay_for_surface(&self, surface: &wl_surface::WlSurface) -> Option<OverlayId> {
        self.overlays
            .iter()
            .find(|(_, overlay)| overlay.layer.wl_surface() == surface)
            .map(|(id, _)| *id)
    }

    fn pattern_has_surface(&self, surface: &wl_surface::WlSurface) -> bool {
        self.patterns
            .values()
            .any(|p| p.layer.wl_surface() == surface)
    }

    /// Attach fresh pixels wherever they changed.
    fn present(&mut self) {
        let State {
            pool,
            overlays,
            patterns,
            ..
        } = self;

        for overlay in overlays.values_mut() {
            if !overlay.configured {
                continue;
            }
            let size = (overlay.surface.width(), overlay.surface.height());
            let has_new_pixels = overlay.surface.frame().is_some();
            let wants_blank = !overlay.surface.is_visible();

            let needs_attach = overlay.dirty
                || has_new_pixels
                || overlay.buffer.is_none()
                || overlay.buffer_size != size
                || overlay.blanked != wants_blank;
            if !needs_attach {
                continue;
            }

            let stride = size.0 as i32 * 4;
            let fresh = overlay.buffer_size != size
                || overlay.buffer.is_none()
                || pool
                    .canvas(overlay.buffer.as_ref().expect("checked"))
                    .is_none();

            let buffer = if fresh {
                match pool.create_buffer(
                    size.0 as i32,
                    size.1 as i32,
                    stride,
                    wl_shm::Format::Argb8888,
                ) {
                    Ok((buffer, canvas)) => {
                        write_pixels(canvas, overlay.surface.pixels(), wants_blank);
                        overlay.buffer = Some(buffer);
                        overlay.buffer_size = size;
                        overlay.buffer.as_ref().expect("just set")
                    }
                    Err(error) => {
                        warn!(%error, "could not allocate an overlay buffer");
                        continue;
                    }
                }
            } else {
                let buffer = overlay.buffer.as_ref().expect("checked");
                if let Some(canvas) = pool.canvas(buffer) {
                    write_pixels(canvas, overlay.surface.pixels(), wants_blank);
                }
                buffer
            };

            let wl_surface = overlay.layer.wl_surface();
            wl_surface.damage_buffer(0, 0, size.0 as i32, size.1 as i32);
            if buffer.attach_to(wl_surface).is_err() {
                warn!("could not attach the overlay buffer");
                continue;
            }
            overlay.layer.commit();
            overlay.blanked = wants_blank;
            overlay.dirty = false;
        }

        for pattern in patterns.values_mut() {
            if !pattern.configured || !pattern.dirty {
                continue;
            }
            let (width, height) = pattern.size;
            let stride = width as i32 * 4;
            let fresh = pattern.buffer.is_none()
                || pool
                    .canvas(pattern.buffer.as_ref().expect("checked"))
                    .is_none();

            let buffer = if fresh {
                match pool.create_buffer(
                    width as i32,
                    height as i32,
                    stride,
                    wl_shm::Format::Argb8888,
                ) {
                    Ok((buffer, canvas)) => {
                        fill_opaque(canvas, pattern.rgb);
                        pattern.buffer = Some(buffer);
                        pattern.buffer.as_ref().expect("just set")
                    }
                    Err(error) => {
                        warn!(%error, "could not allocate a test pattern buffer");
                        continue;
                    }
                }
            } else {
                let buffer = pattern.buffer.as_ref().expect("checked");
                if let Some(canvas) = pool.canvas(buffer) {
                    fill_opaque(canvas, pattern.rgb);
                }
                buffer
            };

            let wl_surface = pattern.layer.wl_surface();
            wl_surface.damage_buffer(0, 0, width as i32, height as i32);
            if buffer.attach_to(wl_surface).is_err() {
                continue;
            }
            pattern.layer.commit();
            pattern.dirty = false;
        }
    }

    /// Where the pointer is, in normalized coordinates of a given surface.
    fn normalize(&self, overlay: &Overlay, position: (f64, f64)) -> Vec2 {
        let w = overlay.surface.width().max(1) as f64;
        let h = overlay.surface.height().max(1) as f64;
        Vec2::new((position.0 / w) as f32, (position.1 / h) as f32)
    }
}

/// Copy the rendered pixels into a shm buffer, or blank it for bypass.
///
/// Blanking is a memset, not a recomputation: the mask is untouched and comes
/// straight back when compensation is restored.
fn write_pixels(canvas: &mut [u8], pixels: &[u8], blank: bool) {
    if blank {
        canvas.fill(0);
    } else {
        let len = canvas.len().min(pixels.len());
        canvas[..len].copy_from_slice(&pixels[..len]);
        canvas[len..].fill(0);
    }
}

fn convert_output(id: OutputId, info: &smithay_client_toolkit::output::OutputInfo) -> OutputInfo {
    // Prefer the logical size: that is the coordinate space a layer surface is
    // configured in, and therefore the space we paint.
    let (width, height) = info
        .logical_size
        .map(|(w, h)| (w.max(0) as u32, h.max(0) as u32))
        .or_else(|| {
            info.modes
                .iter()
                .find(|m| m.current)
                .map(|m| (m.dimensions.0.max(0) as u32, m.dimensions.1.max(0) as u32))
        })
        .unwrap_or((0, 0));

    let refresh = info
        .modes
        .iter()
        .find(|m| m.current)
        .map(|m| m.refresh_rate.max(0) as u32);

    OutputInfo {
        id,
        identity: DisplayIdentity {
            connector: info.name.clone(),
            manufacturer: (!info.make.is_empty()).then(|| info.make.clone()),
            model: (!info.model.is_empty()).then(|| info.model.clone()),
            // Wayland exposes no serial or raw EDID to ordinary clients.
            serial: None,
            edid_hash: None,
        },
        width,
        height,
        position: info.logical_position.unwrap_or(info.location),
        scale: info.scale_factor as f64,
        transform: convert_transform(info.transform),
        refresh_mhz: refresh,
    }
}

fn convert_transform(transform: wl_output::Transform) -> Transform {
    match transform {
        wl_output::Transform::Normal => Transform::Normal,
        wl_output::Transform::_90 => Transform::Rotate90,
        wl_output::Transform::_180 => Transform::Rotate180,
        wl_output::Transform::_270 => Transform::Rotate270,
        wl_output::Transform::Flipped => Transform::Flipped,
        wl_output::Transform::Flipped90 => Transform::FlippedRotate90,
        wl_output::Transform::Flipped180 => Transform::FlippedRotate180,
        wl_output::Transform::Flipped270 => Transform::FlippedRotate270,
        _ => Transform::Normal,
    }
}

fn convert_key(event: &KeyEvent) -> Option<EditorKey> {
    Some(match event.keysym {
        Keysym::Escape => EditorKey::Escape,
        Keysym::Delete | Keysym::KP_Delete => EditorKey::Delete,
        Keysym::BackSpace => EditorKey::Backspace,
        Keysym::Tab | Keysym::ISO_Left_Tab => EditorKey::Tab,
        Keysym::n | Keysym::N => EditorKey::NewDefect,
        Keysym::m | Keysym::M => EditorKey::CycleShowMode,
        Keysym::e | Keysym::E => EditorKey::ToggleSelected,
        _ => return None,
    })
}

fn convert_pattern_key(event: &KeyEvent) -> Option<PatternAction> {
    Some(match event.keysym {
        Keysym::Escape => PatternAction::Exit,
        Keysym::space | Keysym::KP_Space => PatternAction::ToggleCompensation,
        Keysym::Left | Keysym::KP_Left => PatternAction::Previous,
        Keysym::Right | Keysym::KP_Right => PatternAction::Next,
        _ => return None,
    })
}

impl OverlayBackend for WaylandBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Wayland
    }

    fn report(&self) -> BackendReport {
        let support = if self.state.layer_shell.is_some() {
            Support::Full
        } else {
            Support::Limited(LAYER_SHELL_MISSING.to_string())
        };
        BackendReport {
            kind: BackendKind::Wayland,
            support,
        }
    }

    fn outputs(&self) -> Vec<OutputInfo> {
        self.state.outputs.iter().map(|t| t.info.clone()).collect()
    }

    fn create_overlay(&mut self, output: OutputId) -> Result<OverlayId> {
        let Some(tracked) = self.state.output_of(output) else {
            return Err(BackendError::UnknownOutput);
        };
        let wl_output = tracked.output.clone();
        let info = tracked.info.clone();

        let Some(layer_shell) = self.state.layer_shell.as_ref() else {
            return Err(BackendError::Unavailable(LAYER_SHELL_MISSING.into()));
        };

        let qh = self.state.qh.clone();
        let surface = self.state.compositor.create_surface(&qh);
        let layer = layer_shell.create_layer_surface(
            &qh,
            surface,
            Layer::Overlay,
            Some("unburn"),
            Some(&wl_output),
        );

        // Anchoring to all four edges with a zero size asks for the whole
        // output; a negative exclusive zone keeps other clients' space intact.
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_size(0, 0);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);

        let wl_surface = layer.wl_surface();
        // No input region at all: every click goes to whatever is underneath.
        set_empty_input_region(&self.state.compositor, wl_surface);
        // Nothing here is opaque; telling the compositor so lets it optimize.
        wl_surface.set_opaque_region(None);
        layer.commit();

        let id = OverlayId(self.state.allocate_id());
        self.state.overlays.insert(
            id,
            Overlay {
                layer,
                surface: OverlaySurface::new(info.width.max(1), info.height.max(1)),
                buffer: None,
                buffer_size: (0, 0),
                configured: false,
                dirty: true,
                blanked: false,
                interaction: EditorInteraction::new(info.transform),
                transform: info.transform,
            },
        );
        debug!(?id, connector = ?info.identity.connector, "created a layer-shell overlay");
        Ok(id)
    }

    fn destroy_overlay(&mut self, overlay: OverlayId) {
        // Dropping the LayerSurface destroys the wl_surface, which is what
        // makes a crash remove the compensation too.
        self.state.overlays.remove(&overlay);
    }

    fn set_interactive(&mut self, overlay: OverlayId, interactive: bool) {
        let compositor = &self.state.compositor;
        let Some(overlay) = self.state.overlays.get_mut(&overlay) else {
            return;
        };
        if overlay.surface.is_interactive() == interactive {
            return;
        }
        overlay.surface.set_interactive(interactive);

        let wl_surface = overlay.layer.wl_surface();
        if interactive {
            overlay
                .layer
                .set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            // A null input region means "the whole surface".
            wl_surface.set_input_region(None);
        } else {
            overlay
                .layer
                .set_keyboard_interactivity(KeyboardInteractivity::None);
            set_empty_input_region(compositor, wl_surface);
            overlay.interaction.release();
        }
        overlay.layer.commit();
    }

    fn set_visible(&mut self, overlay: OverlayId, visible: bool) {
        if let Some(overlay) = self.state.overlays.get_mut(&overlay) {
            overlay.surface.set_visible(visible);
        }
    }

    fn update_mask(&mut self, overlay: OverlayId, mask: &Mask) {
        if let Some(overlay) = self.state.overlays.get_mut(&overlay) {
            overlay.surface.set_mask(mask);
        }
    }

    fn set_editor(&mut self, overlay: OverlayId, editor: Option<EditorView>) {
        let Some(overlay) = self.state.overlays.get_mut(&overlay) else {
            return;
        };
        if let Some(view) = editor.clone() {
            overlay.interaction.set_view(view, overlay.transform);
        }
        overlay.surface.set_editor(editor);
    }

    fn set_model(&mut self, overlay: OverlayId, model: Option<Mask>) {
        if let Some(overlay) = self.state.overlays.get_mut(&overlay) {
            overlay.surface.set_model(model);
        }
    }

    fn set_dither(&mut self, overlay: OverlayId, dither: bool) {
        if let Some(overlay) = self.state.overlays.get_mut(&overlay) {
            overlay.surface.set_dither(dither);
        }
    }

    fn set_test_pattern(&mut self, output: OutputId, pattern: Option<TestPatternState>) {
        let Some(state) = pattern else {
            self.state.patterns.remove(&output);
            return;
        };
        let Some(tracked) = self.state.output_of(output) else {
            return;
        };
        let wl_output = tracked.output.clone();
        let size = (tracked.info.width.max(1), tracked.info.height.max(1));
        let rgb = state.pattern.rgb();

        if let Some(existing) = self.state.patterns.get_mut(&output) {
            if existing.rgb != rgb || existing.size != size {
                existing.rgb = rgb;
                existing.size = size;
                existing.dirty = true;
            }
            return;
        }

        let Some(layer_shell) = self.state.layer_shell.as_ref() else {
            return;
        };
        let qh = self.state.qh.clone();
        let surface = self.state.compositor.create_surface(&qh);
        // Below the compensation layer, so the correction applies to it.
        let layer = layer_shell.create_layer_surface(
            &qh,
            surface,
            Layer::Top,
            Some("unburn-test-pattern"),
            Some(&wl_output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_size(0, 0);
        layer.set_exclusive_zone(-1);
        // The pattern is driven from the keyboard, so it must be focusable.
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.commit();

        self.state.patterns.insert(
            output,
            Pattern {
                layer,
                buffer: None,
                size,
                rgb,
                configured: false,
                dirty: true,
            },
        );
    }

    fn flush(&mut self) -> Result<()> {
        self.state.present();
        self.conn
            .flush()
            .map_err(|e| BackendError::Protocol(format!("Wayland flush: {e}")))
    }

    fn poll_events(
        &mut self,
        wake: BorrowedFd<'_>,
        timeout: Option<Duration>,
        events: &mut Vec<BackendEvent>,
    ) -> Result<()> {
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|e| BackendError::Protocol(format!("Wayland dispatch: {e}")))?;

        if self.state.events.is_empty() {
            self.conn
                .flush()
                .map_err(|e| BackendError::Protocol(format!("Wayland flush: {e}")))?;

            if let Some(guard) = self.conn.prepare_read() {
                let wayland_fd = guard.connection_fd();
                let mut fds = [
                    rustix::event::PollFd::new(&wayland_fd, rustix::event::PollFlags::IN),
                    rustix::event::PollFd::new(&wake, rustix::event::PollFlags::IN),
                ];
                let spec = timeout.map(to_timespec);
                match rustix::event::poll(&mut fds, spec.as_ref()) {
                    Ok(_) => {}
                    Err(rustix::io::Errno::INTR) => {}
                    Err(error) => return Err(BackendError::Io(error.into())),
                }

                if fds[0].revents().contains(rustix::event::PollFlags::IN) {
                    match guard.read() {
                        Ok(_) => {}
                        Err(wayland_client::backend::WaylandError::Io(error))
                            if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(error) => {
                            return Err(BackendError::Protocol(format!(
                                "the Wayland connection failed: {error}"
                            )))
                        }
                    }
                }
            }

            self.queue
                .dispatch_pending(&mut self.state)
                .map_err(|e| BackendError::Protocol(format!("Wayland dispatch: {e}")))?;
        }

        events.append(&mut self.state.events);
        Ok(())
    }
}

fn to_timespec(duration: Duration) -> rustix::event::Timespec {
    rustix::event::Timespec {
        tv_sec: duration.as_secs() as i64,
        tv_nsec: duration.subsec_nanos() as _,
    }
}

/// Give a surface an empty input region so every event passes through it.
fn set_empty_input_region(compositor: &CompositorState, surface: &wl_surface::WlSurface) {
    match Region::new(compositor) {
        Ok(region) => surface.set_input_region(Some(region.wl_region())),
        Err(error) => warn!(%error, "could not make the overlay click-through"),
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
        self.refresh_outputs();
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
        self.refresh_outputs();
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        // Nothing animates, so no frame callbacks are ever requested.
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.refresh_outputs();
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.refresh_outputs();
    }

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.refresh_outputs();
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        let surface = layer.wl_surface().clone();
        self.overlays
            .retain(|_, overlay| overlay.layer.wl_surface() != &surface);
        self.patterns
            .retain(|_, pattern| pattern.layer.wl_surface() != &surface);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let surface = layer.wl_surface().clone();
        let (width, height) = configure.new_size;

        for overlay in self.overlays.values_mut() {
            if overlay.layer.wl_surface() != &surface {
                continue;
            }
            let (width, height) = if width == 0 || height == 0 {
                (overlay.surface.width(), overlay.surface.height())
            } else {
                (width, height)
            };
            overlay.surface.set_size(width.max(1), height.max(1));
            overlay.configured = true;
            overlay.dirty = true;
            return;
        }

        for pattern in self.patterns.values_mut() {
            if pattern.layer.wl_surface() != &surface {
                continue;
            }
            if width != 0 && height != 0 {
                pattern.size = (width, height);
            }
            pattern.configured = true;
            pattern.dirty = true;
            return;
        }
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard if self.keyboard.is_none() => {
                self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok();
            }
            Capability::Pointer if self.pointer.is_none() => {
                self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard => {
                if let Some(keyboard) = self.keyboard.take() {
                    keyboard.release();
                }
            }
            Capability::Pointer => {
                if let Some(pointer) = self.pointer.take() {
                    pointer.release();
                }
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        self.focus = Some(surface.clone());
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if self.focus.as_ref() == Some(surface) {
            self.focus = None;
        }
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        let Some(surface) = self.focus.clone() else {
            return;
        };

        if self.pattern_has_surface(&surface) {
            if let Some(action) = convert_pattern_key(&event) {
                self.events.push(BackendEvent::Pattern(action));
            }
            return;
        }

        let Some(id) = self.overlay_for_surface(&surface) else {
            return;
        };
        let Some(key) = convert_key(&event) else {
            return;
        };
        let modifiers = self.modifiers;
        if let Some(overlay) = self.overlays.get_mut(&id) {
            overlay.interaction.set_modifiers(modifiers);
            if let Some(action) = overlay.interaction.key(key) {
                self.events.push(BackendEvent::Editor(action));
            }
        }
    }

    fn repeat_key(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        // Holding Tab to walk a long defect list should work.
        self.press_key(conn, qh, keyboard, serial, event);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: SctkModifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.modifiers = Modifiers {
            shift: modifiers.shift,
            ctrl: modifiers.ctrl,
            alt: modifiers.alt,
            logo: modifiers.logo,
        };
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let modifiers = self.modifiers;
        for event in events {
            let Some(id) = self.overlay_for_surface(&event.surface) else {
                continue;
            };
            let Some(overlay) = self.overlays.get(&id) else {
                continue;
            };
            if !overlay.surface.is_interactive() {
                continue;
            }
            let uv = self.normalize(overlay, event.position);

            let Some(overlay) = self.overlays.get_mut(&id) else {
                continue;
            };
            overlay.interaction.set_modifiers(modifiers);

            let action = match event.kind {
                PointerEventKind::Press { button, .. } => match button {
                    0x110 => overlay.interaction.press(uv, Button::Primary),
                    0x111 => overlay.interaction.press(uv, Button::Secondary),
                    _ => None,
                },
                PointerEventKind::Release { .. } => {
                    overlay.interaction.release();
                    None
                }
                PointerEventKind::Motion { .. } | PointerEventKind::Enter { .. } => {
                    overlay.interaction.motion(uv)
                }
                PointerEventKind::Leave { .. } => {
                    overlay.interaction.release();
                    None
                }
                PointerEventKind::Axis { vertical, .. } => {
                    // Scroll down is positive in the protocol, but scrolling up
                    // should grow the defect.
                    let notches = if vertical.discrete != 0 {
                        -vertical.discrete as f32
                    } else {
                        (-vertical.absolute / 15.0) as f32
                    };
                    if notches.abs() < 0.01 {
                        None
                    } else {
                        overlay.interaction.wheel(notches)
                    }
                }
            };

            if let Some(action) = action {
                self.events.push(BackendEvent::Editor(action));
            }
        }
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(State);

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(State);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_wayland_transform_is_understood() {
        assert_eq!(
            convert_transform(wl_output::Transform::Normal),
            Transform::Normal
        );
        assert_eq!(
            convert_transform(wl_output::Transform::_90),
            Transform::Rotate90
        );
        assert_eq!(
            convert_transform(wl_output::Transform::Flipped270),
            Transform::FlippedRotate270
        );
    }

    #[test]
    fn blanking_writes_a_transparent_buffer() {
        let pixels = vec![0xFFu8; 16];
        let mut canvas = vec![0u8; 16];
        write_pixels(&mut canvas, &pixels, false);
        assert!(canvas.iter().all(|b| *b == 0xFF));
        write_pixels(&mut canvas, &pixels, true);
        assert!(canvas.iter().all(|b| *b == 0));
    }

    #[test]
    fn a_short_source_leaves_no_stale_pixels() {
        let pixels = vec![0xABu8; 8];
        let mut canvas = vec![0xFFu8; 16];
        write_pixels(&mut canvas, &pixels, false);
        assert_eq!(&canvas[..8], &[0xAB; 8]);
        assert_eq!(&canvas[8..], &[0; 8]);
    }
}
