//! On-screen editing: translating raw pointer and key events into edits.
//!
//! Both backends deliver input in surface-normalized coordinates and get back
//! actions expressed in panel coordinates, so the rules for what a drag or a
//! scroll means live in exactly one place and can be tested without a display.

use std::cmp::Ordering;

use uuid::Uuid;

use crate::{
    compensation::Vec2,
    display::Transform,
    overlay::{EditorView, Grab, ShowMode},
};

/// How close, in normalized units, the pointer must get to grab a handle.
const HANDLE_TOLERANCE: f32 = 0.012;

/// Multiplicative radius change per wheel notch.
const WHEEL_RADIUS_STEP: f32 = 1.08;
/// Strength change per wheel notch, in absolute brightness excess.
const WHEEL_STRENGTH_STEP: f32 = 0.005;
/// Falloff change per wheel notch.
const WHEEL_FALLOFF_STEP: f32 = 0.05;

/// Keys the overlay reacts to while it is interactive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorKey {
    Escape,
    Delete,
    Backspace,
    Tab,
    NewDefect,
    CycleShowMode,
    ToggleSelected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub logo: bool,
}

/// An edit the overlay wants the application to make.
///
/// Geometry is in panel coordinates, so a rotated screen needs no special
/// handling anywhere above this module.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorAction {
    Select(Uuid),
    SelectNext,
    Create(Vec2),
    Move {
        id: Uuid,
        center: Vec2,
    },
    SetRadiusX {
        id: Uuid,
        radius: f32,
    },
    SetRadiusY {
        id: Uuid,
        radius: f32,
    },
    ScaleRadius {
        id: Uuid,
        factor: f32,
    },
    AdjustStrength {
        id: Uuid,
        delta: f32,
    },
    AdjustFalloff {
        id: Uuid,
        delta: f32,
    },
    ToggleEnabled(Uuid),
    Delete(Uuid),
    CycleShowMode,
    Leave,
    /// The panic button: drop every overlay at once.
    EmergencyDisable,
}

/// Pointer buttons, reduced to what the editor uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Primary,
    Secondary,
}

/// What a press took hold of, kept for the length of the drag.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Grabbed {
    id: Uuid,
    grab: Grab,
    /// Pointer minus the grabbed point at the moment of the press.
    offset: Vec2,
}

/// Editing state for one interactive surface.
#[derive(Debug, Default)]
pub struct EditorInteraction {
    view: EditorView,
    transform: Transform,
    grab: Option<Grabbed>,
    pointer: Vec2,
    modifiers: Modifiers,
}

impl EditorInteraction {
    pub fn new(transform: Transform) -> Self {
        Self {
            transform,
            ..Default::default()
        }
    }

    /// Refresh the geometry the hit-tester works against.
    pub fn set_view(&mut self, view: EditorView, transform: Transform) {
        self.view = view;
        self.transform = transform;
        // A defect that disappeared cannot still be grabbed.
        if let Some(grabbed) = self.grab {
            if !self.view.defects.iter().any(|d| d.id == grabbed.id) {
                self.grab = None;
            }
        }
    }

    pub fn set_modifiers(&mut self, modifiers: Modifiers) {
        self.modifiers = modifiers;
    }

    pub fn show_mode(&self) -> ShowMode {
        self.view.show
    }

    pub fn release(&mut self) {
        self.grab = None;
    }

    pub fn pointer_position(&self) -> Vec2 {
        self.pointer
    }

    pub fn press(&mut self, uv: Vec2, button: Button) -> Option<EditorAction> {
        self.pointer = uv;
        match button {
            Button::Primary => match self.view.hit_test(uv, HANDLE_TOLERANCE) {
                Some((id, grab)) => {
                    self.grab = self.anchor(id, grab, uv);
                    (self.view.selected != Some(id)).then_some(EditorAction::Select(id))
                }
                // Clicking past every spot is the way out of editing mode. New
                // spots come from the window's button or the `n` key, so a
                // misjudged click cannot litter the panel with them.
                None => {
                    self.grab = None;
                    Some(EditorAction::Leave)
                }
            },
            Button::Secondary => {
                let (id, _) = self.view.hit_test(uv, HANDLE_TOLERANCE)?;
                Some(EditorAction::ToggleEnabled(id))
            }
        }
    }

    pub fn motion(&mut self, uv: Vec2) -> Option<EditorAction> {
        self.pointer = uv;

        let Grabbed { id, grab, offset } = self.grab?;
        let defect = self.view.defects.iter().find(|d| d.id == id)?;
        // Where the grabbed point belongs now. Working from the press anchor
        // rather than from the last position keeps the drag exact: the view
        // this reads only catches up after a round trip through the GUI, so
        // anything accumulated against it would lag and stutter.
        let at = uv - offset;

        Some(match grab {
            Grab::Center => EditorAction::Move {
                id,
                center: self.transform.surface_to_panel(at),
            },
            Grab::Width => {
                let radius = project(at - defect.center, defect.rotation, false);
                EditorAction::SetRadiusX { id, radius }
            }
            Grab::Height => {
                let radius = project(at - defect.center, defect.rotation, true);
                EditorAction::SetRadiusY { id, radius }
            }
        })
    }

    /// Work out how far a press landed from the thing it grabbed.
    ///
    /// Each axis has a handle at both ends, so the anchor is the nearer of the
    /// two; otherwise grabbing the left handle would resize as if the right one
    /// had been taken.
    fn anchor(&self, id: Uuid, grab: Grab, uv: Vec2) -> Option<Grabbed> {
        let defect = self.view.defects.iter().find(|d| d.id == id)?;
        let handles = defect.handles();
        let grabbed = match grab {
            Grab::Center => defect.center,
            Grab::Width => nearest(&handles[0..2], uv),
            Grab::Height => nearest(&handles[2..4], uv),
        };
        Some(Grabbed {
            id,
            grab,
            offset: uv - grabbed,
        })
    }

    /// One wheel notch; `notches` is positive when scrolling up.
    pub fn wheel(&mut self, notches: f32) -> Option<EditorAction> {
        let id = self.view.selected.or_else(|| {
            self.view
                .hit_test(self.pointer, HANDLE_TOLERANCE)
                .map(|(id, _)| id)
        })?;

        Some(if self.modifiers.shift {
            EditorAction::AdjustStrength {
                id,
                delta: notches * WHEEL_STRENGTH_STEP,
            }
        } else if self.modifiers.ctrl {
            EditorAction::AdjustFalloff {
                id,
                delta: notches * WHEEL_FALLOFF_STEP,
            }
        } else {
            EditorAction::ScaleRadius {
                id,
                factor: WHEEL_RADIUS_STEP.powf(notches),
            }
        })
    }

    pub fn key(&mut self, key: EditorKey) -> Option<EditorAction> {
        // The escape hatch for a platform bug that leaves an opaque surface on
        // screen: it must work regardless of what else is going on.
        if key == EditorKey::Backspace
            && self.modifiers.ctrl
            && self.modifiers.alt
            && self.modifiers.shift
        {
            return Some(EditorAction::EmergencyDisable);
        }

        match key {
            EditorKey::Escape => Some(EditorAction::Leave),
            EditorKey::Tab => Some(EditorAction::SelectNext),
            EditorKey::NewDefect => Some(EditorAction::Create(
                self.transform.surface_to_panel(self.pointer),
            )),
            EditorKey::Delete => self.view.selected.map(EditorAction::Delete),
            EditorKey::ToggleSelected => self.view.selected.map(EditorAction::ToggleEnabled),
            EditorKey::CycleShowMode => Some(EditorAction::CycleShowMode),
            EditorKey::Backspace => None,
        }
    }
}

/// Whichever candidate is closest to `uv`.
fn nearest(candidates: &[Vec2], uv: Vec2) -> Vec2 {
    candidates
        .iter()
        .copied()
        .min_by(|a, b| {
            (*a - uv)
                .length()
                .partial_cmp(&(*b - uv).length())
                .unwrap_or(Ordering::Equal)
        })
        .unwrap_or(uv)
}

/// Length of `delta` along one of the ellipse's own axes.
fn project(delta: Vec2, rotation: f32, across: bool) -> f32 {
    let (sin, cos) = rotation.sin_cos();
    let value = if across {
        -delta.x * sin + delta.y * cos
    } else {
        delta.x * cos + delta.y * sin
    };
    value.abs().max(1.0e-4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compensation::Rgb;
    use crate::overlay::EditorDefect;

    fn view(center: Vec2, radius: Vec2) -> (Uuid, EditorView) {
        let id = Uuid::new_v4();
        let defect = EditorDefect {
            id,
            center,
            radius,
            rotation: 0.0,
            strength: Rgb::splat(0.1),
            enabled: true,
        };
        (
            id,
            EditorView {
                defects: vec![defect],
                selected: Some(id),
                show: ShowMode::Outlines,
            },
        )
    }

    fn editor(center: Vec2, radius: Vec2) -> (Uuid, EditorInteraction) {
        let (id, view) = view(center, radius);
        let mut editor = EditorInteraction::new(Transform::Normal);
        editor.set_view(view, Transform::Normal);
        (id, editor)
    }

    #[test]
    fn dragging_the_centre_moves_the_defect() {
        let (id, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.1));
        assert_eq!(editor.press(Vec2::splat(0.5), Button::Primary), None);

        let action = editor.motion(Vec2::new(0.55, 0.52)).unwrap();
        match action {
            EditorAction::Move { id: moved, center } => {
                assert_eq!(moved, id);
                assert!((center.x - 0.55).abs() < 1e-5);
                assert!((center.y - 0.52).abs() < 1e-5);
            }
            other => panic!("expected a move, got {other:?}"),
        }
    }

    /// The regression that made on-screen dragging crawl: the view a drag reads
    /// only refreshes after the edit has been round-tripped through the GUI
    /// thread, so several motion events in a row see the same stale centre.
    /// Movement used to be accumulated onto that centre one event at a time,
    /// which threw away everything but the last step.
    #[test]
    fn a_drag_keeps_up_with_the_pointer_while_the_view_is_stale() {
        let (id, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.1));
        editor.press(Vec2::splat(0.5), Button::Primary);

        let mut last = None;
        for step in 1..=4 {
            let uv = Vec2::new(0.5 + 0.05 * step as f32, 0.5);
            last = editor.motion(uv);
        }

        match last {
            Some(EditorAction::Move { id: moved, center }) => {
                assert_eq!(moved, id);
                // Four steps of 0.05 from 0.5, not one.
                assert!(
                    (center.x - 0.7).abs() < 1e-5,
                    "the drag fell behind: {center:?}"
                );
            }
            other => panic!("expected a move, got {other:?}"),
        }
    }

    #[test]
    fn a_stale_view_does_not_drag_the_spot_backwards() {
        // The flicker: a late refresh carrying an old centre used to shift the
        // next motion by however far the view had fallen behind.
        let (id, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.1));
        editor.press(Vec2::splat(0.5), Button::Primary);
        editor.motion(Vec2::new(0.7, 0.5));

        let (_, stale) = view(Vec2::splat(0.5), Vec2::splat(0.1));
        let stale = EditorView {
            defects: vec![EditorDefect {
                id,
                ..stale.defects[0]
            }],
            selected: Some(id),
            show: ShowMode::Outlines,
        };
        editor.set_view(stale, Transform::Normal);

        match editor.motion(Vec2::new(0.72, 0.5)) {
            Some(EditorAction::Move { center, .. }) => {
                assert!((center.x - 0.72).abs() < 1e-5, "{center:?}");
            }
            other => panic!("expected a move, got {other:?}"),
        }
    }

    #[test]
    fn grabbing_a_spot_off_centre_does_not_snap_it_to_the_pointer() {
        let (_, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.2));
        // Inside the ellipse, well away from its middle and from the handles.
        editor.press(Vec2::new(0.6, 0.5), Button::Primary);

        match editor.motion(Vec2::new(0.65, 0.5)) {
            Some(EditorAction::Move { center, .. }) => {
                assert!(
                    (center.x - 0.55).abs() < 1e-5,
                    "snapped to the pointer: {center:?}"
                );
            }
            other => panic!("expected a move, got {other:?}"),
        }
    }

    #[test]
    fn dragging_either_width_handle_sets_the_width() {
        for (pressed, moved) in [
            (Vec2::new(0.4, 0.5), Vec2::new(0.35, 0.5)),
            (Vec2::new(0.6, 0.5), Vec2::new(0.65, 0.5)),
        ] {
            let (id, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.1));
            editor.press(pressed, Button::Primary);
            match editor.motion(moved) {
                Some(EditorAction::SetRadiusX { id: got, radius }) => {
                    assert_eq!(got, id);
                    assert!((radius - 0.15).abs() < 1e-5, "{radius}");
                }
                other => panic!("expected a width change, got {other:?}"),
            }
        }
    }

    #[test]
    fn dragging_the_top_handle_sets_the_height() {
        let (id, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.1));
        editor.press(Vec2::new(0.5, 0.6), Button::Primary);
        match editor.motion(Vec2::new(0.5, 0.68)) {
            Some(EditorAction::SetRadiusY { id: got, radius }) => {
                assert_eq!(got, id);
                assert!((radius - 0.18).abs() < 1e-5);
            }
            other => panic!("expected a height change, got {other:?}"),
        }
    }

    #[test]
    fn releasing_the_pointer_ends_the_drag() {
        let (_, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.1));
        editor.press(Vec2::splat(0.5), Button::Primary);
        editor.release();
        assert_eq!(editor.motion(Vec2::new(0.7, 0.7)), None);
    }

    #[test]
    fn clicking_empty_space_leaves_editing() {
        let (_, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.05));
        assert_eq!(
            editor.press(Vec2::new(0.1, 0.9), Button::Primary),
            Some(EditorAction::Leave)
        );
    }

    #[test]
    fn leaving_by_click_does_not_grab_anything() {
        // Otherwise the release-less exit would leave a stale grab behind that
        // the next drag would pick up.
        let (_, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.05));
        editor.press(Vec2::new(0.1, 0.9), Button::Primary);
        assert_eq!(editor.motion(Vec2::new(0.2, 0.8)), None);
    }

    #[test]
    fn the_new_defect_key_still_creates_one_under_the_pointer() {
        let (_, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.05));
        editor.press(Vec2::new(0.1, 0.9), Button::Primary);
        match editor.key(EditorKey::NewDefect) {
            Some(EditorAction::Create(at)) => {
                assert!(
                    (at.x - 0.1).abs() < 1e-5 && (at.y - 0.9).abs() < 1e-5,
                    "{at:?}"
                );
            }
            other => panic!("expected a create, got {other:?}"),
        }
    }

    #[test]
    fn clicking_another_defect_selects_it() {
        let (first, mut view) = view(Vec2::new(0.2, 0.2), Vec2::splat(0.05));
        let second = Uuid::new_v4();
        view.defects.push(EditorDefect {
            id: second,
            center: Vec2::new(0.8, 0.8),
            radius: Vec2::splat(0.05),
            rotation: 0.0,
            strength: Rgb::splat(0.1),
            enabled: true,
        });
        view.selected = Some(first);
        let mut editor = EditorInteraction::new(Transform::Normal);
        editor.set_view(view, Transform::Normal);

        assert_eq!(
            editor.press(Vec2::new(0.8, 0.8), Button::Primary),
            Some(EditorAction::Select(second))
        );
    }

    #[test]
    fn the_wheel_scales_the_radius() {
        let (id, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.1));
        match editor.wheel(1.0) {
            Some(EditorAction::ScaleRadius { id: got, factor }) => {
                assert_eq!(got, id);
                assert!(factor > 1.0);
            }
            other => panic!("expected a radius change, got {other:?}"),
        }
        match editor.wheel(-1.0) {
            Some(EditorAction::ScaleRadius { factor, .. }) => assert!(factor < 1.0),
            other => panic!("expected a radius change, got {other:?}"),
        }
    }

    #[test]
    fn shift_and_control_redirect_the_wheel() {
        let (id, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.1));

        editor.set_modifiers(Modifiers {
            shift: true,
            ..Default::default()
        });
        assert!(
            matches!(editor.wheel(2.0), Some(EditorAction::AdjustStrength { id: got, delta }) if got == id && delta > 0.0)
        );

        editor.set_modifiers(Modifiers {
            ctrl: true,
            ..Default::default()
        });
        assert!(matches!(
            editor.wheel(1.0),
            Some(EditorAction::AdjustFalloff { .. })
        ));
    }

    #[test]
    fn the_emergency_combination_disables_everything() {
        let (_, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.1));
        editor.set_modifiers(Modifiers {
            ctrl: true,
            alt: true,
            shift: true,
            logo: false,
        });
        assert_eq!(
            editor.key(EditorKey::Backspace),
            Some(EditorAction::EmergencyDisable)
        );

        editor.set_modifiers(Modifiers::default());
        assert_eq!(editor.key(EditorKey::Backspace), None);
    }

    #[test]
    fn edits_on_a_rotated_screen_come_back_in_panel_coordinates() {
        let (_, view) = view(Vec2::new(0.5, 0.75), Vec2::splat(0.05));
        let mut editor = EditorInteraction::new(Transform::Rotate90);
        editor.set_view(view, Transform::Rotate90);

        editor.motion(Vec2::new(0.9, 0.1));
        match editor.key(EditorKey::NewDefect) {
            Some(EditorAction::Create(at)) => {
                // Under a quarter turn the surface's top-right corner is the
                // panel's bottom-right one.
                assert!((at.x - 0.9).abs() < 1e-5, "{at:?}");
                assert!((at.y - 0.9).abs() < 1e-5, "{at:?}");
            }
            other => panic!("expected a create, got {other:?}"),
        }
    }

    #[test]
    fn a_deleted_defect_cannot_stay_grabbed() {
        let (_, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.1));
        editor.press(Vec2::splat(0.5), Button::Primary);
        editor.set_view(EditorView::default(), Transform::Normal);
        assert_eq!(editor.motion(Vec2::new(0.7, 0.7)), None);
    }
}
