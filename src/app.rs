//! The application: configuration, live overlays, and the rules connecting them.
//!
//! Everything the GUI can do goes through this type, so the headless mode and
//! the control socket get exactly the same behaviour without duplicating it.

use std::{path::PathBuf, sync::mpsc::Sender};

use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    cli::{Args, BackendChoice},
    compensation::{Defect, RadialDefect, Vec2},
    config::{self, DisplayProfile, Profile},
    display::{DisplayIdentity, OutputInfo},
    ipc,
    overlay::{DiscSwatch, ShowMode, TestPattern, TestPatternState},
    platform::{
        self, BackendEvent, BackendKind, BackendReport, DesiredState, DisplaySettings,
        EditingState, EditorAction, OverlayService, PatternAction,
    },
};

/// Bounds that keep hand-editing and wheel adjustments inside sane values.
const MIN_RADIUS: f32 = 0.002;
const MAX_RADIUS: f32 = 2.0;
const MIN_FALLOFF: f32 = 0.2;
const MAX_FALLOFF: f32 = 4.0;

/// Largest per-channel strength the editor will set, in either direction.
///
/// Strength is brightness excess, so 5.0 describes a patch emitting six times
/// the light of the rest of the panel. Correcting that leaves a sixth of the
/// light, which at a gamma of 2.2 is an overlay a little over half opaque.
/// Nothing in the compensation maths needs an upper bound; this only exists to
/// keep the sliders over a range that means something.
pub const MAX_STRENGTH: f32 = 5.0;

pub struct App {
    pub profile: Profile,
    config_path: PathBuf,

    service: Option<OverlayService>,
    reports: Vec<BackendReport>,
    /// Monitors to work from when there is no backend to ask. Empty for a
    /// normally started app; see [`App::offline`].
    offline_outputs: Vec<OutputInfo>,

    /// The instant bypass. Nothing is recomputed when this flips.
    bypass: bool,
    /// Which display the GUI is working on.
    selected_display: Option<DisplayIdentity>,
    selected_defect: Option<Uuid>,
    editing: bool,
    /// Spot whose Edit panel is open: rotating calibration disc behind it.
    calibration_disc: Option<Uuid>,
    /// Spot the GUI list is pointing at. Locator only; not an edit.
    hovered_defect: Option<Uuid>,
    /// Colours on that disc, in palette order. Shared across spots.
    disc_colors: Vec<[u8; 3]>,
    show_mode: ShowMode,
    test_pattern: Option<TestPatternState>,

    unsaved: bool,
    should_quit: bool,
    status: String,
}

impl App {
    /// Load the configuration and bring up the overlay backend.
    ///
    /// `wake` is signalled whenever the backend has something to report, so the
    /// caller's main loop can stay asleep the rest of the time.
    pub fn start(args: &Args, wake: Sender<()>) -> Result<App, String> {
        let config_path = config::config_path().map_err(|e| e.to_string())?;
        let profile = Profile::load_or_default(&config_path).map_err(|e| e.to_string())?;

        let mut app = App {
            profile,
            config_path,
            service: None,
            reports: platform::detect(),
            offline_outputs: Vec::new(),
            bypass: false,
            selected_display: None,
            selected_defect: None,
            editing: false,
            calibration_disc: None,
            hovered_defect: None,
            disc_colors: DiscSwatch::default_colors(),
            show_mode: ShowMode::default(),
            test_pattern: args
                .test_pattern
                .as_deref()
                .and_then(TestPattern::parse)
                .map(|pattern| TestPatternState {
                    pattern,
                    ..Default::default()
                }),
            unsaved: false,
            should_quit: false,
            status: String::new(),
        };

        app.connect(args.backend, move || {
            wake.send(()).ok();
        })?;
        app.adopt_connected_displays();
        app.sync();
        Ok(app)
    }

    /// An app that never touches a display server, working from the profile,
    /// monitors and backend reports it is handed.
    ///
    /// Everything the calibration window draws comes from here, so the window
    /// can be rendered on a machine that has neither the monitor nor the
    /// compositor the profile describes. That is what the documentation
    /// screenshot tool uses; nothing else should need it.
    pub fn offline(profile: Profile, outputs: Vec<OutputInfo>, reports: Vec<BackendReport>) -> App {
        let mut app = App {
            profile,
            config_path: config::config_path().unwrap_or_else(|_| PathBuf::from("config.toml")),
            service: None,
            reports,
            offline_outputs: outputs,
            bypass: false,
            selected_display: None,
            selected_defect: None,
            editing: false,
            calibration_disc: None,
            hovered_defect: None,
            disc_colors: DiscSwatch::default_colors(),
            show_mode: ShowMode::default(),
            test_pattern: None,
            unsaved: false,
            should_quit: false,
            status: String::new(),
        };
        app.adopt_connected_displays();
        app
    }

    /// Start the overlay backend. `notify` is called when the backend has news.
    pub fn connect(
        &mut self,
        choice: BackendChoice,
        notify: impl Fn() + Send + 'static,
    ) -> Result<(), String> {
        let kind = match choice {
            BackendChoice::Wayland => Some(BackendKind::Wayland),
            BackendChoice::X11 => Some(BackendKind::X11),
            BackendChoice::Auto => platform::preferred_kind(&self.reports),
        };
        let Some(kind) = kind else {
            return Err(self
                .reports
                .iter()
                .map(|r| r.describe())
                .collect::<Vec<_>>()
                .join("\n"));
        };

        match OverlayService::start(kind, notify) {
            Ok(service) => {
                info!(backend = kind.label(), "overlay backend ready");
                self.status = service.report().describe();
                self.service = Some(service);
                Ok(())
            }
            Err(error) => Err(format!(
                "could not start the {} backend: {error}",
                kind.label()
            )),
        }
    }

    pub fn backend_reports(&self) -> &[BackendReport] {
        &self.reports
    }

    pub fn active_report(&self) -> Option<&BackendReport> {
        self.service.as_ref().map(|s| s.report())
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn outputs(&self) -> Vec<OutputInfo> {
        match self.service.as_ref() {
            Some(service) => service.outputs(),
            None => self.offline_outputs.clone(),
        }
    }

    pub fn config_path(&self) -> &PathBuf {
        &self.config_path
    }

    pub fn is_bypassed(&self) -> bool {
        self.bypass
    }

    pub fn set_bypass(&mut self, bypass: bool) {
        if self.bypass != bypass {
            self.bypass = bypass;
            self.sync();
        }
    }

    pub fn toggle_bypass(&mut self) {
        self.set_bypass(!self.bypass);
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn set_editing(&mut self, editing: bool) {
        if self.editing != editing {
            self.editing = editing;
            self.sync();
        }
    }

    /// Open or close the rotating calibration disc behind a spot.
    pub fn set_calibration_disc(&mut self, id: Option<Uuid>) {
        if self.calibration_disc != id {
            self.calibration_disc = id;
            self.sync();
        }
    }

    /// Point at a spot from the list. Cheap: the overlay patches a cross
    /// onto the pixels it already has.
    pub fn set_hovered_defect(&mut self, id: Option<Uuid>) {
        if self.hovered_defect != id {
            self.hovered_defect = id;
            self.sync();
        }
    }

    pub fn disc_colors(&self) -> &[[u8; 3]] {
        &self.disc_colors
    }

    pub fn set_disc_colors(&mut self, colors: Vec<[u8; 3]>) {
        if self.disc_colors != colors {
            self.disc_colors = colors;
            self.sync();
        }
    }

    pub fn show_mode(&self) -> ShowMode {
        self.show_mode
    }

    pub fn set_show_mode(&mut self, mode: ShowMode) {
        if self.show_mode != mode {
            self.show_mode = mode;
            self.sync();
        }
    }

    pub fn test_pattern(&self) -> Option<TestPatternState> {
        self.test_pattern
    }

    pub fn set_test_pattern(&mut self, pattern: Option<TestPatternState>) {
        if self.test_pattern != pattern {
            self.test_pattern = pattern;
            self.sync();
        }
    }

    pub fn unsaved_changes(&self) -> bool {
        self.unsaved
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn request_quit(&mut self) {
        self.should_quit = true;
    }

    /// Make sure every connected monitor has an entry to configure.
    pub fn adopt_connected_displays(&mut self) {
        for output in self.outputs() {
            let known = self.profile.find(&output.identity).is_some();
            if !known {
                let entry = self.profile.entry(&output.identity);
                entry.name = output.identity.describe();
                debug!(display = %entry.name, "first sight of this monitor");
            }
        }
        if self.selected_display.is_none() {
            self.selected_display = self
                .outputs()
                .first()
                .map(|o| o.identity.clone())
                .or_else(|| self.profile.displays.first().map(|d| d.identity.clone()));
            self.selected_defect = self.first_defect();
        }
    }

    /// The display's first defect, so the parameter panel is never empty when
    /// there is something to edit.
    fn first_defect(&self) -> Option<Uuid> {
        self.selected_display()?.defects.first().map(|d| d.id())
    }

    // ---- selection -------------------------------------------------------

    pub fn selected_display(&self) -> Option<&DisplayProfile> {
        let identity = self.selected_display.as_ref()?;
        self.profile.find(identity)
    }

    pub fn selected_display_mut(&mut self) -> Option<&mut DisplayProfile> {
        let identity = self.selected_display.clone()?;
        self.profile.find_mut(&identity)
    }

    pub fn select_display(&mut self, identity: DisplayIdentity) {
        if self.selected_display.as_ref() == Some(&identity) {
            return;
        }
        self.selected_display = Some(identity);
        self.selected_defect = self.first_defect();
        self.calibration_disc = None;
        self.sync();
    }

    pub fn selected_defect(&self) -> Option<Uuid> {
        self.selected_defect
    }

    pub fn select_defect(&mut self, id: Option<Uuid>) {
        if self.selected_defect != id {
            self.selected_defect = id;
            self.sync();
        }
    }

    /// The output the selected display is currently connected to, if any.
    pub fn selected_output(&self) -> Option<OutputInfo> {
        let identity = self.selected_display.as_ref()?;
        let outputs = self.outputs();
        crate::display::best_match(identity, outputs.iter()).cloned()
    }

    // ---- editing ---------------------------------------------------------

    /// Add a defect at a normalized panel position and select it.
    pub fn add_defect(&mut self, at: Vec2) -> Option<Uuid> {
        let aspect = self
            .selected_output()
            .map(|o| o.panel_aspect())
            .unwrap_or(16.0 / 9.0);
        let display = self.selected_display_mut()?;
        let defect = RadialDefect::new_at(clamp_unit(at), aspect);
        let id = defect.id;
        display.defects.push(Defect::Radial(defect));
        self.selected_defect = Some(id);
        self.mark_changed();
        Some(id)
    }

    /// Copy a defect, offset far enough to be grabbed separately.
    ///
    /// Panels tend to have several blemishes of much the same shape, so the
    /// quickest way to the second one is usually a copy of the first.
    pub fn clone_defect(&mut self, id: Uuid) -> Option<Uuid> {
        let display = self.selected_display_mut()?;
        let index = display.defect_index(id)?;
        let source = display.defects[index].as_radial()?;

        // Inserted next to its original rather than appended, so the list stays
        // in the order the spots were reasoned about.
        let copy = cloned_beside(source);
        let new_id = copy.id;
        display.defects.insert(index + 1, Defect::Radial(copy));
        self.selected_defect = Some(new_id);
        self.mark_changed();
        Some(new_id)
    }

    pub fn delete_defect(&mut self, id: Uuid) {
        let Some(display) = self.selected_display_mut() else {
            return;
        };
        display.defects.retain(|d| d.id() != id);
        if self.selected_defect == Some(id) {
            self.selected_defect = self
                .selected_display()
                .and_then(|d| d.defects.last())
                .map(|d| d.id());
        }
        if self.calibration_disc == Some(id) {
            self.calibration_disc = None;
        }
        self.mark_changed();
    }

    pub fn select_next_defect(&mut self) {
        let Some(display) = self.selected_display() else {
            return;
        };
        if display.defects.is_empty() {
            return;
        }
        let next = match self.selected_defect.and_then(|id| display.defect_index(id)) {
            Some(index) => (index + 1) % display.defects.len(),
            None => 0,
        };
        self.selected_defect = Some(display.defects[next].id());
        self.sync();
    }

    /// Apply `edit` to one defect and push the result to the overlays.
    pub fn edit_defect(&mut self, id: Uuid, edit: impl FnOnce(&mut RadialDefect)) {
        let Some(display) = self.selected_display_mut() else {
            return;
        };
        let Some(index) = display.defect_index(id) else {
            return;
        };
        if let Some(radial) = display.defects[index].as_radial_mut() {
            edit(radial);
            radial.center = clamp_unit(radial.center);
            radial.radius.x = radial.radius.x.clamp(MIN_RADIUS, MAX_RADIUS);
            radial.radius.y = radial.radius.y.clamp(MIN_RADIUS, MAX_RADIUS);
            radial.strength = radial
                .strength
                .map(|s| s.clamp(-MAX_STRENGTH, MAX_STRENGTH));
            radial.falloff = radial.falloff.clamp(MIN_FALLOFF, MAX_FALLOFF);
        }
        self.mark_changed();
    }

    pub fn mark_changed(&mut self) {
        self.unsaved = true;
        self.sync();
    }

    pub fn save(&mut self) -> Result<(), String> {
        self.profile
            .save(&self.config_path)
            .map_err(|e| e.to_string())?;
        self.unsaved = false;
        info!(path = %self.config_path.display(), "configuration saved");
        Ok(())
    }

    /// Load the configuration file from disk, discarding unsaved edits.
    pub fn reload(&mut self) -> Result<(), String> {
        let profile = Profile::load_or_default(&self.config_path).map_err(|e| e.to_string())?;
        self.profile = profile;
        self.unsaved = false;
        self.selected_display = None;
        self.selected_defect = None;
        self.editing = false;
        self.calibration_disc = None;
        self.adopt_connected_displays();
        self.sync();
        Ok(())
    }

    // ---- driving the overlays -------------------------------------------

    /// Push the current configuration to the overlay backend.
    pub fn sync(&mut self) {
        let Some(service) = self.service.as_ref() else {
            return;
        };
        service.apply(self.desired_state());
    }

    fn desired_state(&self) -> DesiredState {
        let editing_identity = if self.editing {
            self.selected_display.clone()
        } else {
            None
        };
        let showing_pattern = self.test_pattern.is_some();

        let displays = self
            .profile
            .displays
            .iter()
            .map(|display| {
                let being_edited = editing_identity
                    .as_ref()
                    .is_some_and(|i| &display.identity == i);
                let showing_disc = self
                    .calibration_disc
                    .is_some_and(|id| display.defects.iter().any(|d| d.id() == id));
                DisplaySettings {
                    identity: display.identity.clone(),
                    // An overlay that would draw nothing is not created at all;
                    // the editor, the disc, and the calibration patterns still
                    // need one. The disc stays up while Edit is open even if
                    // this spot is unchecked.
                    enabled: showing_disc
                        || (display.enabled
                            && (has_effect(display) || being_edited || showing_pattern)),
                    params: display.mask_params(),
                    defects: display.defects.clone(),
                }
            })
            .collect();

        DesiredState {
            bypass: self.bypass,
            displays,
            editing: editing_identity.map(|identity| EditingState {
                identity,
                selected: self.selected_defect,
                show: self.show_mode,
            }),
            calibration_disc: self.calibration_disc.and_then(|id| {
                self.profile
                    .displays
                    .iter()
                    .find(|d| d.defects.iter().any(|defect| defect.id() == id))
                    .map(|d| (d.identity.clone(), id))
            }),
            hovered: self.hovered_defect.and_then(|id| {
                self.profile
                    .displays
                    .iter()
                    .find(|d| d.defects.iter().any(|defect| defect.id() == id))
                    .map(|d| (d.identity.clone(), id))
            }),
            disc_colors: self.disc_colors.clone(),
            test_pattern: self.test_pattern,
        }
    }

    /// Handle everything the backend reported. Returns true if anything moved.
    pub fn pump(&mut self) -> bool {
        let Some(service) = self.service.as_ref() else {
            return false;
        };
        let events = service.poll();
        if events.is_empty() {
            return false;
        }

        let mut resync = false;
        for event in events {
            match event {
                BackendEvent::OutputsChanged(_) => {
                    self.adopt_connected_displays();
                    resync = true;
                }
                BackendEvent::Editor(action) => {
                    self.apply_editor_action(action);
                    resync = true;
                }
                BackendEvent::Pattern(action) => {
                    self.apply_pattern_action(action);
                    resync = true;
                }
                BackendEvent::Disconnected(reason) => {
                    warn!(%reason, "the display server connection was lost");
                    self.status = format!("Display server connection lost: {reason}");
                    self.service = None;
                    return true;
                }
            }
        }
        if resync {
            self.sync();
        }
        true
    }

    fn apply_editor_action(&mut self, action: EditorAction) {
        match action {
            EditorAction::Select(id) => self.selected_defect = Some(id),
            EditorAction::SelectNext => self.select_next_defect(),
            EditorAction::Create(at) => {
                self.add_defect(at);
            }
            EditorAction::Move { id, center } => self.edit_defect(id, |d| d.center = center),
            EditorAction::SetRadiusX { id, radius } => {
                self.edit_defect(id, |d| d.radius.x = radius)
            }
            EditorAction::SetRadiusY { id, radius } => {
                self.edit_defect(id, |d| d.radius.y = radius)
            }
            EditorAction::ScaleRadius { id, factor } => {
                self.edit_defect(id, |d| d.scale_radius(factor))
            }
            EditorAction::AdjustStrength { id, delta } => {
                self.edit_defect(id, |d| d.strength = d.strength.map(|s| s + delta))
            }
            EditorAction::AdjustFalloff { id, delta } => {
                self.edit_defect(id, |d| d.falloff += delta)
            }
            EditorAction::ToggleEnabled(id) => {
                let Some(display) = self.selected_display_mut() else {
                    return;
                };
                if let Some(index) = display.defect_index(id) {
                    let enabled = display.defects[index].enabled();
                    display.defects[index].set_enabled(!enabled);
                }
                self.mark_changed();
            }
            EditorAction::Delete(id) => self.delete_defect(id),
            EditorAction::CycleShowMode => self.show_mode = self.show_mode.next(),
            EditorAction::Leave => self.editing = false,
            EditorAction::EmergencyDisable => self.emergency_disable(),
        }
    }

    fn apply_pattern_action(&mut self, action: PatternAction) {
        match action {
            PatternAction::Exit => self.test_pattern = None,
            // Space compares before and after, which is the same thing as
            // flipping the bypass.
            PatternAction::ToggleCompensation => self.bypass = !self.bypass,
            PatternAction::Next | PatternAction::Previous => {
                let delta = if action == PatternAction::Next { 1 } else { -1 };
                if let Some(state) = self.test_pattern.as_mut() {
                    state.pattern = state.pattern.step(delta);
                }
            }
        }
    }

    /// Remove every overlay right now, whatever else is going on.
    pub fn emergency_disable(&mut self) {
        warn!("emergency disable: removing all overlays");
        self.bypass = true;
        self.editing = false;
        self.calibration_disc = None;
        self.test_pattern = None;
        if let Some(service) = self.service.as_ref() {
            service.tear_down();
        }
        self.sync();
        self.status = "Compensation disabled".into();
    }

    /// Act on a request that arrived over the control socket.
    pub fn handle_request(&mut self, request: &ipc::Request) {
        match request {
            ipc::Request::Hide => self.set_bypass(true),
            ipc::Request::Show => self.set_bypass(false),
            ipc::Request::Quit => self.should_quit = true,
            ipc::Request::ShowWindow | ipc::Request::Status => {}
            ipc::Request::TestPattern(text) => {
                let pattern = if text.is_empty() || text == "off" {
                    None
                } else {
                    TestPattern::parse(text).map(|pattern| TestPatternState {
                        pattern,
                        ..Default::default()
                    })
                };
                self.set_test_pattern(pattern);
            }
        }
    }

    /// One line describing what is on screen, for `unburn status`.
    pub fn status_line(&self) -> String {
        let active = self
            .profile
            .displays
            .iter()
            .filter(|d| d.enabled && has_effect(d))
            .count();
        let backend = self
            .service
            .as_ref()
            .map(|s| s.report().kind.label())
            .unwrap_or("none");
        format!(
            "backend={backend} compensation={} displays={active} editing={} pattern={}",
            if self.bypass { "bypassed" } else { "on" },
            self.editing,
            self.test_pattern
                .map(|p| p.pattern.label())
                .unwrap_or_else(|| "off".into())
        )
    }

    /// Let the backend know the outputs may have changed.
    pub fn refresh_outputs(&self) {
        if let Some(service) = self.service.as_ref() {
            service.refresh();
        }
    }
}

/// Whether a display's settings would actually darken anything.
///
/// Either sign counts: a bright spot is dimmed where it sits, a dim patch is
/// matched by dimming everything else.
pub fn has_effect(display: &DisplayProfile) -> bool {
    display.compensation > 0.0
        && display
            .defects
            .iter()
            .any(|d| d.enabled() && d.as_radial().is_some_and(|r| r.strength.max_abs() > 0.0))
}

fn clamp_unit(v: Vec2) -> Vec2 {
    Vec2::new(v.x.clamp(0.0, 1.0), v.y.clamp(0.0, 1.0))
}

/// A copy of `source` shifted clear of it, with an identity of its own.
///
/// The step is measured in the spot's own radii so that a copy of a small spot
/// lands nearby and a copy of a broad one does not overlap it.
fn cloned_beside(source: &RadialDefect) -> RadialDefect {
    let mut copy = source.clone();
    copy.id = Uuid::new_v4();

    let step = Vec2::new(source.radius.x * 2.5, source.radius.y * 2.5);
    copy.center = clamp_unit(source.center + step);
    // Against the bottom-right corner there is no room that way, so go back
    // towards the middle instead of stacking the copy on the original.
    if copy.center == source.center {
        copy.center = clamp_unit(source.center - step);
    }
    copy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compensation::RadialDefect;

    fn display_with(strength: f32, enabled: bool) -> DisplayProfile {
        let mut display = DisplayProfile::new(DisplayIdentity {
            connector: Some("HDMI-A-1".into()),
            ..Default::default()
        });
        display.defects.push(Defect::Radial(RadialDefect {
            strength: crate::compensation::Rgb::splat(strength),
            enabled,
            ..Default::default()
        }));
        display
    }

    #[test]
    fn a_display_has_an_effect_only_when_compensation_can_change_it() {
        for (strength, enabled, compensation, expected) in [
            (0.1, true, 1.0, true),
            (0.0, true, 1.0, false),
            (-0.1, true, 1.0, true),
            (0.1, false, 1.0, false),
            (0.1, true, 0.0, false),
        ] {
            let mut display = display_with(strength, enabled);
            display.compensation = compensation;
            assert_eq!(
                has_effect(&display),
                expected,
                "strength {strength}, enabled {enabled}, compensation {compensation}"
            );
        }
    }

    #[test]
    fn a_clone_lands_clear_of_its_original() {
        let source = RadialDefect {
            center: Vec2::splat(0.4),
            radius: Vec2::new(0.06, 0.1),
            strength: crate::compensation::Rgb::new(0.3, 0.2, 0.1),
            rotation: 0.5,
            ..Default::default()
        };
        let copy = cloned_beside(&source);

        assert_ne!(copy.id, source.id);
        // Everything but where it sits is carried over.
        assert_eq!(copy.strength, source.strength);
        assert_eq!(copy.radius, source.radius);
        assert_eq!(copy.rotation, source.rotation);

        // Far enough apart that neither centre is inside the other ellipse.
        let delta = copy.center - source.center;
        let normalized = (delta.x / source.radius.x).hypot(delta.y / source.radius.y);
        assert!(
            normalized > 1.0,
            "the clone overlaps its original: {normalized}"
        );
    }

    #[test]
    fn a_clone_in_the_corner_stays_on_the_panel() {
        let source = RadialDefect {
            center: Vec2::splat(1.0),
            radius: Vec2::splat(0.1),
            ..Default::default()
        };
        let copy = cloned_beside(&source);

        assert_ne!(
            copy.center, source.center,
            "the clone hid under the original"
        );
        for value in [copy.center.x, copy.center.y] {
            assert!((0.0..=1.0).contains(&value), "{:?}", copy.center);
        }
    }

    #[test]
    fn reload_replaces_memory_with_what_is_on_disk() {
        let dir = std::env::temp_dir().join(format!("unburn-reload-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let identity = DisplayIdentity {
            connector: Some("HDMI-A-1".into()),
            ..Default::default()
        };

        let mut on_disk = Profile::default();
        on_disk.entry(&identity).compensation = 0.4;
        on_disk.save(&path).unwrap();

        let mut live = Profile::default();
        live.entry(&identity).compensation = 0.9;

        let mut app = App::offline(live, vec![], vec![]);
        app.config_path = path;
        app.reload().unwrap();

        assert_eq!(app.profile.displays[0].compensation, 0.4);
        std::fs::remove_dir_all(&dir).ok();
    }
}
