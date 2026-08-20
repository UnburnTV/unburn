//! Platform-specific overlay windows, and the platform-independent logic that
//! drives them.
//!
//! The overlay deliberately does not go through the same abstraction as the
//! GUI: what a compensation layer needs from the windowing system — always on
//! top, no input, no reserved space, exactly one output — is inherently
//! platform-specific, and pretending otherwise only hides the differences that
//! matter.

pub mod identity;
pub mod interaction;
pub mod wayland;
pub mod x11;

mod service;

use std::collections::HashMap;

use uuid::Uuid;

use crate::{
    compensation::{mask, Defect, Mask, MaskParams},
    display::{DisplayIdentity, OutputId, OutputInfo, OverlayId},
    overlay::{CalibrationDisc, EditorDefect, EditorView, ShowMode, TestPatternState},
};

pub use interaction::{Button, EditorAction, EditorKey, Modifiers};
pub use service::OverlayService;

pub type Result<T> = std::result::Result<T, BackendError>;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("no usable overlay backend: {0}")]
    Unavailable(String),
    #[error("{0}")]
    Protocol(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("no such output")]
    UnknownOutput,
    #[error("no such overlay")]
    UnknownOverlay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Wayland,
    X11,
}

impl BackendKind {
    pub fn label(self) -> &'static str {
        match self {
            BackendKind::Wayland => "Wayland",
            BackendKind::X11 => "X11",
        }
    }
}

/// How well this session can host a compensation layer.
///
/// The distinction matters: a fallback window is not the same thing as a real
/// overlay layer, and the program must not pretend that it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    /// Everything the overlay needs is available.
    Full,
    /// Usable, but with a caveat the user needs to know about.
    Limited(String),
    /// This backend cannot run here at all.
    Unavailable(String),
}

impl Support {
    pub fn is_usable(&self) -> bool {
        !matches!(self, Support::Unavailable(_))
    }

    pub fn headline(&self) -> &'static str {
        match self {
            Support::Full => "Full",
            Support::Limited(_) => "Limited",
            Support::Unavailable(_) => "Unavailable",
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            Support::Full => None,
            Support::Limited(reason) | Support::Unavailable(reason) => Some(reason),
        }
    }
}

/// What a backend reports about the session it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendReport {
    pub kind: BackendKind,
    pub support: Support,
}

impl BackendReport {
    pub fn describe(&self) -> String {
        match self.support.detail() {
            None => format!("{} support: {}", self.kind.label(), self.support.headline()),
            Some(detail) => format!(
                "{} support: {}\n{}",
                self.kind.label(),
                self.support.headline(),
                detail
            ),
        }
    }
}

/// Overlay windows for one windowing system.
pub trait OverlayBackend {
    fn kind(&self) -> BackendKind;

    /// How well this session supports a real compensation layer.
    fn report(&self) -> BackendReport;

    fn outputs(&self) -> Vec<OutputInfo>;

    fn create_overlay(&mut self, output: OutputId) -> Result<OverlayId>;

    fn destroy_overlay(&mut self, overlay: OverlayId);

    /// Let the overlay take pointer and keyboard input, for on-screen editing.
    /// In normal mode this is always off and the surface is click-through.
    fn set_interactive(&mut self, overlay: OverlayId, interactive: bool);

    fn set_visible(&mut self, overlay: OverlayId, visible: bool);

    fn update_mask(&mut self, overlay: OverlayId, mask: &Mask);

    /// The annotations drawn on top of the compensation while editing.
    fn set_editor(&mut self, overlay: OverlayId, editor: Option<EditorView>);

    /// Rotating calibration disc behind the spot whose Edit panel is open.
    fn set_disc(&mut self, overlay: OverlayId, disc: Option<CalibrationDisc>);

    /// Red cross locating the spot the GUI is pointing at. Must not force a
    /// mask resample: hovering a list row has to stay cheap.
    fn set_hover(&mut self, overlay: OverlayId, center: Option<crate::compensation::Vec2>);

    /// The modelled defect field, needed only by the editor's "show model".
    fn set_model(&mut self, overlay: OverlayId, model: Option<Mask>);

    fn set_dither(&mut self, overlay: OverlayId, dither: bool);

    /// Show or hide the fullscreen calibration pattern on one output.
    fn set_test_pattern(&mut self, output: OutputId, pattern: Option<TestPatternState>);

    /// Push everything queued to the display server.
    fn flush(&mut self) -> Result<()>;

    /// Sleep until the display server or `wake` has something to say, then
    /// process it.
    ///
    /// The backend owns the wait because getting it right is protocol-specific,
    /// and because this is where the program spends essentially all of its
    /// time: nothing should run between configuration changes.
    fn poll_events(
        &mut self,
        wake: std::os::fd::BorrowedFd<'_>,
        timeout: Option<std::time::Duration>,
        events: &mut Vec<BackendEvent>,
    ) -> Result<()>;
}

/// Something the backend noticed that the application should know about.
#[derive(Debug, Clone, PartialEq)]
pub enum BackendEvent {
    /// The set of connected monitors, or their geometry, changed.
    OutputsChanged(Vec<OutputInfo>),
    /// An edit made through the on-screen editor.
    Editor(EditorAction),
    /// A key pressed on the calibration pattern surface.
    Pattern(PatternAction),
    /// The display server went away; overlays are gone with it.
    Disconnected(String),
}

/// Keyboard control of the calibration patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternAction {
    Next,
    Previous,
    ToggleCompensation,
    Exit,
}

/// Per-display compensation settings, resolved and ready to render.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplaySettings {
    pub identity: DisplayIdentity,
    pub enabled: bool,
    pub params: MaskParams,
    /// Panel coordinates; each surface applies its own rotation.
    pub defects: Vec<Defect>,
}

/// Which screen is being edited on, and how.
#[derive(Debug, Clone, PartialEq)]
pub struct EditingState {
    pub identity: DisplayIdentity,
    pub selected: Option<Uuid>,
    pub show: ShowMode,
}

/// Everything the application wants on screen right now.
///
/// The application sends whole snapshots rather than incremental commands, so
/// the backend can always reconcile towards a known state instead of replaying
/// a history it might have missed part of.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DesiredState {
    /// The instant bypass. Overlays stay alive but present nothing.
    pub bypass: bool,
    pub displays: Vec<DisplaySettings>,
    pub editing: Option<EditingState>,
    /// Spot whose Edit panel is open, on that display: draw the rotating disc.
    pub calibration_disc: Option<(DisplayIdentity, Uuid)>,
    /// Spot the GUI is pointing at, so the overlay can mark it. Not an edit.
    pub hovered: Option<(DisplayIdentity, Uuid)>,
    /// Wedges on that disc, in palette order.
    pub disc_colors: Vec<[u8; 3]>,
    pub test_pattern: Option<TestPatternState>,
}

impl DesiredState {
    fn settings_for<'a>(&'a self, output: &OutputInfo) -> Option<&'a DisplaySettings> {
        self.displays
            .iter()
            .map(|d| (d.identity.match_score(&output.identity), d))
            .filter(|(score, _)| *score >= crate::display::MatchScore::WEAK)
            .max_by_key(|(score, _)| *score)
            .map(|(_, d)| d)
    }

    fn editing_output(&self, output: &OutputInfo) -> Option<&EditingState> {
        let editing = self.editing.as_ref()?;
        (editing.identity.match_score(&output.identity) >= crate::display::MatchScore::WEAK)
            .then_some(editing)
    }

    fn disc_for_output(&self, output: &OutputInfo) -> Option<Uuid> {
        let (identity, id) = self.calibration_disc.as_ref()?;
        (identity.match_score(&output.identity) >= crate::display::MatchScore::WEAK).then_some(*id)
    }

    fn hovered_for_output(&self, output: &OutputInfo) -> Option<Uuid> {
        let (identity, id) = self.hovered.as_ref()?;
        (identity.match_score(&output.identity) >= crate::display::MatchScore::WEAK).then_some(*id)
    }
}

/// What one overlay's sampled fields were last generated from.
#[derive(Debug, Clone, PartialEq)]
struct MaskKey {
    defects: Vec<Defect>,
    params: MaskParams,
    width: u32,
    height: u32,
    transform: crate::display::Transform,
}

struct Live {
    overlay: OverlayId,
    /// What the compensation the overlay is holding was generated from. Left
    /// where it is while the editor is up, since nothing is drawing it.
    mask: Option<MaskKey>,
    /// The same for the modelled defect field, which only the editor asks for.
    model: Option<MaskKey>,
}

/// Drives a backend towards a [`DesiredState`].
///
/// This holds all the "when do we actually need to recompute" logic, which is
/// what keeps the program idle: masks are regenerated only when a defect,
/// compensation, quality or the output geometry moves.
#[derive(Default)]
pub struct Reconciler {
    desired: DesiredState,
    live: HashMap<OutputId, Live>,
    dirty: bool,
}

impl Reconciler {
    pub fn new() -> Self {
        Self {
            dirty: true,
            ..Default::default()
        }
    }

    pub fn desired(&self) -> &DesiredState {
        &self.desired
    }

    pub fn set_desired(&mut self, desired: DesiredState) {
        if self.desired != desired {
            self.desired = desired;
            self.dirty = true;
        }
    }

    /// Force a full pass, for instance after the outputs changed.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Remove every overlay. Used by bypass-to-nothing, quit and the panic
    /// button, all of which must not depend on any recomputation.
    pub fn tear_down(&mut self, backend: &mut dyn OverlayBackend) {
        for (_, live) in self.live.drain() {
            backend.destroy_overlay(live.overlay);
        }
        self.dirty = true;
    }

    /// Bring the backend in line with the desired state.
    pub fn sync(&mut self, backend: &mut dyn OverlayBackend) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        self.dirty = false;

        let outputs = backend.outputs();

        // Drop overlays whose output disappeared. The configuration stays put, so the
        // compensation comes back untouched if the monitor returns.
        let present: Vec<OutputId> = outputs.iter().map(|o| o.id).collect();
        let stale: Vec<OutputId> = self
            .live
            .keys()
            .copied()
            .filter(|id| !present.contains(id))
            .collect();
        for id in stale {
            if let Some(live) = self.live.remove(&id) {
                backend.destroy_overlay(live.overlay);
            }
        }

        for output in &outputs {
            let settings = self.desired.settings_for(output);
            let wanted = settings.map(|s| s.enabled).unwrap_or(false);

            if !wanted {
                if let Some(live) = self.live.remove(&output.id) {
                    backend.destroy_overlay(live.overlay);
                }
                backend.set_test_pattern(output.id, None);
                continue;
            }
            let settings = settings.expect("wanted implies settings");

            let live = match self.live.get_mut(&output.id) {
                Some(live) => live,
                None => {
                    let overlay = backend.create_overlay(output.id)?;
                    self.live.insert(
                        output.id,
                        Live {
                            overlay,
                            mask: None,
                            model: None,
                        },
                    );
                    self.live.get_mut(&output.id).expect("just inserted")
                }
            };
            let overlay = live.overlay;

            let editing = self.desired.editing_output(output);
            let show = editing.map(|e| e.show).unwrap_or_default();

            let key = MaskKey {
                defects: settings.defects.clone(),
                params: settings.params,
                width: output.width,
                height: output.height,
                transform: output.transform,
            };

            // Nothing draws the compensation while the editor is up, so
            // nothing generates it either: a pointer produces edits far faster
            // than a mask can be built and resampled, and every one of them
            // would be thrown away undrawn. The one the overlay is already
            // holding stays there, which is what makes leaving the editor
            // instant when the geometry came back to where it started.
            let stale_mask = editing.is_none() && live.mask.as_ref() != Some(&key);
            let stale_model = show.draws_model() && live.model.as_ref() != Some(&key);

            if stale_mask || stale_model {
                let surface_defects: Vec<Defect> = settings
                    .defects
                    .iter()
                    .map(|d| crate::overlay::transform_defect(d, output.transform))
                    .collect();

                if stale_mask {
                    let mask = mask::generate(
                        &surface_defects,
                        &settings.params,
                        output.width,
                        output.height,
                    );
                    backend.update_mask(overlay, &mask);
                    live.mask = Some(key.clone());
                }

                if stale_model {
                    let (w, h) = settings
                        .params
                        .quality
                        .resolution_for(output.width, output.height);
                    let model = mask::generate_model_field(&surface_defects, w, h);
                    backend.set_model(overlay, Some(model));
                    live.model = Some(key);
                }
            }

            if !show.draws_model() && live.model.is_some() {
                backend.set_model(overlay, None);
                live.model = None;
            }

            backend.set_dither(overlay, settings.params.dither);
            backend.set_visible(overlay, !self.desired.bypass);
            backend.set_interactive(overlay, editing.is_some());
            backend.set_editor(
                overlay,
                editing.map(|editing| EditorView {
                    defects: settings
                        .defects
                        .iter()
                        .filter_map(|d| EditorDefect::from_defect(d, output.transform))
                        .collect(),
                    selected: editing.selected,
                    show: editing.show,
                }),
            );
            let disc = self.desired.disc_for_output(output).and_then(|id| {
                settings
                    .defects
                    .iter()
                    .find(|d| d.id() == id)
                    .and_then(|d| EditorDefect::from_defect(d, output.transform))
                    .map(|defect| CalibrationDisc {
                        defect,
                        colors: self.desired.disc_colors.clone(),
                    })
            });
            backend.set_disc(overlay, disc);

            let hover = self.desired.hovered_for_output(output).and_then(|id| {
                if editing.is_some_and(|e| e.selected == Some(id)) {
                    return None;
                }
                if self.desired.disc_for_output(output) == Some(id) {
                    return None;
                }
                settings
                    .defects
                    .iter()
                    .find(|d| d.id() == id)
                    .and_then(|d| EditorDefect::from_defect(d, output.transform))
                    .map(|defect| defect.center)
            });
            backend.set_hover(overlay, hover);

            backend.set_test_pattern(output.id, self.desired.test_pattern);
        }

        backend.flush()
    }
}

/// Ask each windowing system what it can do here.
pub fn detect() -> Vec<BackendReport> {
    vec![wayland::probe(), x11::probe()]
}

/// The backend that should be used, given what the session offers.
pub fn preferred_kind(reports: &[BackendReport]) -> Option<BackendKind> {
    reports
        .iter()
        .filter(|r| r.support.is_usable())
        .max_by_key(|r| match r.support {
            Support::Full => 2,
            Support::Limited(_) => 1,
            Support::Unavailable(_) => 0,
        })
        .map(|r| r.kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compensation::Rgb;
    use crate::{
        compensation::{RadialDefect, Vec2},
        display::Transform,
    };

    #[derive(Debug, PartialEq)]
    enum Call {
        Create(OutputId),
        Destroy(OverlayId),
        Mask(OverlayId),
        Visible(OverlayId, bool),
        Interactive(OverlayId, bool),
        Editor(OverlayId, bool),
        Hover(OverlayId, bool),
    }

    #[derive(Default)]
    struct FakeBackend {
        outputs: Vec<OutputInfo>,
        next: u32,
        calls: Vec<Call>,
    }

    impl FakeBackend {
        fn masks(&self) -> usize {
            self.calls
                .iter()
                .filter(|c| matches!(c, Call::Mask(_)))
                .count()
        }
    }

    impl OverlayBackend for FakeBackend {
        fn kind(&self) -> BackendKind {
            BackendKind::X11
        }
        fn report(&self) -> BackendReport {
            BackendReport {
                kind: BackendKind::X11,
                support: Support::Full,
            }
        }
        fn outputs(&self) -> Vec<OutputInfo> {
            self.outputs.clone()
        }
        fn create_overlay(&mut self, output: OutputId) -> Result<OverlayId> {
            self.calls.push(Call::Create(output));
            self.next += 1;
            Ok(OverlayId(self.next))
        }
        fn destroy_overlay(&mut self, overlay: OverlayId) {
            self.calls.push(Call::Destroy(overlay));
        }
        fn set_interactive(&mut self, overlay: OverlayId, interactive: bool) {
            self.calls.push(Call::Interactive(overlay, interactive));
        }
        fn set_visible(&mut self, overlay: OverlayId, visible: bool) {
            self.calls.push(Call::Visible(overlay, visible));
        }
        fn update_mask(&mut self, overlay: OverlayId, _mask: &Mask) {
            self.calls.push(Call::Mask(overlay));
        }
        fn set_editor(&mut self, overlay: OverlayId, editor: Option<EditorView>) {
            self.calls.push(Call::Editor(overlay, editor.is_some()));
        }
        fn set_disc(&mut self, _overlay: OverlayId, _disc: Option<CalibrationDisc>) {}
        fn set_hover(&mut self, overlay: OverlayId, center: Option<Vec2>) {
            self.calls.push(Call::Hover(overlay, center.is_some()));
        }
        fn set_model(&mut self, _overlay: OverlayId, _model: Option<Mask>) {}
        fn set_dither(&mut self, _overlay: OverlayId, _dither: bool) {}
        fn set_test_pattern(&mut self, _output: OutputId, _pattern: Option<TestPatternState>) {}
        fn flush(&mut self) -> Result<()> {
            Ok(())
        }
        fn poll_events(
            &mut self,
            _wake: std::os::fd::BorrowedFd<'_>,
            _timeout: Option<std::time::Duration>,
            _events: &mut Vec<BackendEvent>,
        ) -> Result<()> {
            Ok(())
        }
    }

    fn identity(connector: &str) -> DisplayIdentity {
        DisplayIdentity {
            connector: Some(connector.into()),
            ..Default::default()
        }
    }

    fn output(id: u32, connector: &str) -> OutputInfo {
        OutputInfo {
            id: OutputId(id),
            identity: identity(connector),
            width: 1920,
            height: 1080,
            position: (0, 0),
            scale: 1.0,
            transform: Transform::Normal,
            refresh_mhz: None,
        }
    }

    fn settings(connector: &str, enabled: bool) -> DisplaySettings {
        DisplaySettings {
            identity: identity(connector),
            enabled,
            params: MaskParams::default(),
            defects: vec![Defect::Radial(RadialDefect {
                center: Vec2::splat(0.5),
                strength: Rgb::splat(0.1),
                ..Default::default()
            })],
        }
    }

    #[test]
    fn an_enabled_display_gets_exactly_one_overlay() {
        let mut backend = FakeBackend {
            outputs: vec![output(1, "HDMI-A-1")],
            ..Default::default()
        };
        let mut reconciler = Reconciler::new();
        reconciler.set_desired(DesiredState {
            displays: vec![settings("HDMI-A-1", true)],
            ..Default::default()
        });

        reconciler.sync(&mut backend).unwrap();
        assert!(backend.calls.contains(&Call::Create(OutputId(1))));
        assert_eq!(backend.masks(), 1);
    }

    #[test]
    fn a_disabled_display_gets_none() {
        let mut backend = FakeBackend {
            outputs: vec![output(1, "HDMI-A-1")],
            ..Default::default()
        };
        let mut reconciler = Reconciler::new();
        reconciler.set_desired(DesiredState {
            displays: vec![settings("HDMI-A-1", false)],
            ..Default::default()
        });

        reconciler.sync(&mut backend).unwrap();
        assert!(!backend.calls.iter().any(|c| matches!(c, Call::Create(_))));
    }

    /// The whole point of the exercise: corrections traced on one panel must not
    /// be painted onto whatever is plugged into that port next.
    #[test]
    fn a_replacement_monitor_is_not_painted_with_the_old_ones_corrections() {
        let tv = DisplayIdentity {
            connector: Some("HDMI-A-1".into()),
            model: Some("QN90B".into()),
            serial: Some("SN12345".into()),
            ..Default::default()
        };
        let replacement = DisplayIdentity {
            connector: Some("HDMI-A-1".into()),
            model: Some("U2723QE".into()),
            serial: Some("CN-0ABCDE".into()),
            ..Default::default()
        };

        let mut backend = FakeBackend {
            outputs: vec![OutputInfo {
                identity: replacement,
                ..output(1, "HDMI-A-1")
            }],
            ..Default::default()
        };
        let mut reconciler = Reconciler::new();
        reconciler.set_desired(DesiredState {
            displays: vec![DisplaySettings {
                identity: tv,
                ..settings("HDMI-A-1", true)
            }],
            ..Default::default()
        });

        reconciler.sync(&mut backend).unwrap();
        assert!(!backend.calls.iter().any(|c| matches!(c, Call::Create(_))));
    }

    #[test]
    fn an_unconfigured_display_is_left_alone() {
        let mut backend = FakeBackend {
            outputs: vec![output(1, "DP-9")],
            ..Default::default()
        };
        let mut reconciler = Reconciler::new();
        reconciler.set_desired(DesiredState {
            displays: vec![settings("HDMI-A-1", true)],
            ..Default::default()
        });

        reconciler.sync(&mut backend).unwrap();
        assert!(!backend.calls.iter().any(|c| matches!(c, Call::Create(_))));
    }

    #[test]
    fn nothing_happens_when_nothing_changed() {
        let mut backend = FakeBackend {
            outputs: vec![output(1, "HDMI-A-1")],
            ..Default::default()
        };
        let mut reconciler = Reconciler::new();
        let state = DesiredState {
            displays: vec![settings("HDMI-A-1", true)],
            ..Default::default()
        };
        reconciler.set_desired(state.clone());
        reconciler.sync(&mut backend).unwrap();

        let before = backend.calls.len();
        reconciler.set_desired(state);
        reconciler.sync(&mut backend).unwrap();
        assert_eq!(
            backend.calls.len(),
            before,
            "an unchanged state must do no work"
        );
    }

    #[test]
    fn bypass_hides_the_overlay_without_regenerating_it() {
        let mut backend = FakeBackend {
            outputs: vec![output(1, "HDMI-A-1")],
            ..Default::default()
        };
        let mut reconciler = Reconciler::new();
        let state = DesiredState {
            displays: vec![settings("HDMI-A-1", true)],
            ..Default::default()
        };
        reconciler.set_desired(state.clone());
        reconciler.sync(&mut backend).unwrap();
        let masks = backend.masks();

        reconciler.set_desired(DesiredState {
            bypass: true,
            ..state
        });
        reconciler.sync(&mut backend).unwrap();

        assert_eq!(backend.masks(), masks, "bypass must not recompute the mask");
        assert!(backend.calls.contains(&Call::Visible(OverlayId(1), false)));
    }

    #[test]
    fn moving_a_defect_regenerates_the_mask() {
        let mut backend = FakeBackend {
            outputs: vec![output(1, "HDMI-A-1")],
            ..Default::default()
        };
        let mut reconciler = Reconciler::new();
        reconciler.set_desired(DesiredState {
            displays: vec![settings("HDMI-A-1", true)],
            ..Default::default()
        });
        reconciler.sync(&mut backend).unwrap();

        let mut moved = settings("HDMI-A-1", true);
        moved.defects[0].set_center(Vec2::new(0.2, 0.3));
        reconciler.set_desired(DesiredState {
            displays: vec![moved],
            ..Default::default()
        });
        reconciler.sync(&mut backend).unwrap();

        assert_eq!(backend.masks(), 2);
    }

    #[test]
    fn a_resolution_change_regenerates_the_mask() {
        let mut backend = FakeBackend {
            outputs: vec![output(1, "HDMI-A-1")],
            ..Default::default()
        };
        let mut reconciler = Reconciler::new();
        reconciler.set_desired(DesiredState {
            displays: vec![settings("HDMI-A-1", true)],
            ..Default::default()
        });
        reconciler.sync(&mut backend).unwrap();

        backend.outputs[0].width = 3840;
        backend.outputs[0].height = 2160;
        reconciler.invalidate();
        reconciler.sync(&mut backend).unwrap();

        assert_eq!(backend.masks(), 2);
    }

    #[test]
    fn an_unplugged_monitor_loses_its_overlay_but_keeps_its_settings() {
        let mut backend = FakeBackend {
            outputs: vec![output(1, "HDMI-A-1")],
            ..Default::default()
        };
        let mut reconciler = Reconciler::new();
        let state = DesiredState {
            displays: vec![settings("HDMI-A-1", true)],
            ..Default::default()
        };
        reconciler.set_desired(state.clone());
        reconciler.sync(&mut backend).unwrap();

        backend.outputs.clear();
        reconciler.invalidate();
        reconciler.sync(&mut backend).unwrap();
        assert!(backend.calls.contains(&Call::Destroy(OverlayId(1))));

        // Replugged, possibly on another connector.
        backend.outputs.push(output(2, "HDMI-A-1"));
        reconciler.invalidate();
        reconciler.sync(&mut backend).unwrap();
        assert!(backend.calls.contains(&Call::Create(OutputId(2))));
        assert_eq!(reconciler.desired().displays, state.displays);
    }

    #[test]
    fn editing_makes_only_that_screen_interactive() {
        let mut backend = FakeBackend {
            outputs: vec![output(1, "HDMI-A-1"), output(2, "DP-1")],
            ..Default::default()
        };
        let mut reconciler = Reconciler::new();
        reconciler.set_desired(DesiredState {
            displays: vec![settings("HDMI-A-1", true), settings("DP-1", true)],
            editing: Some(EditingState {
                identity: identity("HDMI-A-1"),
                selected: None,
                show: ShowMode::Outlines,
            }),
            ..Default::default()
        });
        reconciler.sync(&mut backend).unwrap();

        let edited = backend
            .calls
            .iter()
            .filter_map(|c| match c {
                Call::Interactive(id, true) => Some(*id),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(edited.len(), 1);
        assert!(backend.calls.contains(&Call::Editor(edited[0], true)));
    }

    #[test]
    fn hovering_a_spot_does_not_regenerate_the_mask() {
        let mut backend = FakeBackend {
            outputs: vec![output(1, "HDMI-A-1")],
            ..Default::default()
        };
        let mut reconciler = Reconciler::new();
        let state = DesiredState {
            displays: vec![settings("HDMI-A-1", true)],
            ..Default::default()
        };
        let id = state.displays[0].defects[0].id();
        reconciler.set_desired(state.clone());
        reconciler.sync(&mut backend).unwrap();
        let masks = backend.masks();

        reconciler.set_desired(DesiredState {
            hovered: Some((identity("HDMI-A-1"), id)),
            ..state
        });
        reconciler.sync(&mut backend).unwrap();

        assert_eq!(
            backend.masks(),
            masks,
            "pointing at a list row must not rebuild the compensation"
        );
        assert!(backend.calls.contains(&Call::Hover(OverlayId(1), true)));
    }

    #[test]
    fn the_locator_stays_off_a_spot_being_moved() {
        let mut backend = FakeBackend {
            outputs: vec![output(1, "HDMI-A-1")],
            ..Default::default()
        };
        let mut reconciler = Reconciler::new();
        let displays = vec![settings("HDMI-A-1", true)];
        let id = displays[0].defects[0].id();
        reconciler.set_desired(DesiredState {
            displays,
            editing: Some(EditingState {
                identity: identity("HDMI-A-1"),
                selected: Some(id),
                show: ShowMode::Outlines,
            }),
            hovered: Some((identity("HDMI-A-1"), id)),
            ..Default::default()
        });
        reconciler.sync(&mut backend).unwrap();

        let last_hover = backend.calls.iter().rev().find_map(|c| match c {
            Call::Hover(_, on) => Some(*on),
            _ => None,
        });
        assert_eq!(
            last_hover,
            Some(false),
            "Move already marks the spot; the list hover must not pile on"
        );
    }

    #[test]
    fn tearing_down_removes_every_overlay() {
        let mut backend = FakeBackend {
            outputs: vec![output(1, "HDMI-A-1"), output(2, "DP-1")],
            ..Default::default()
        };
        let mut reconciler = Reconciler::new();
        reconciler.set_desired(DesiredState {
            displays: vec![settings("HDMI-A-1", true), settings("DP-1", true)],
            ..Default::default()
        });
        reconciler.sync(&mut backend).unwrap();

        reconciler.tear_down(&mut backend);
        assert_eq!(
            backend
                .calls
                .iter()
                .filter(|c| matches!(c, Call::Destroy(_)))
                .count(),
            2
        );
    }
}
