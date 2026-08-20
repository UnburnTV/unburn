//! On-screen editing: translating raw pointer and key events into edits.
//!
//! Both backends deliver input in surface-normalized coordinates and get back
//! actions expressed in panel coordinates, so the rules for what a drag or a
//! scroll means live in exactly one place and can be tested without a display.

use std::cmp::Ordering;

use uuid::Uuid;

use crate::{
    compensation::{angle_in_square, Vec2},
    display::Transform,
    overlay::{EditorView, Grab},
};

/// Multiplicative radius change per wheel notch.
const WHEEL_RADIUS_STEP: f32 = 1.08;
/// Strength change per wheel notch, in absolute brightness excess.
const WHEEL_STRENGTH_STEP: f32 = 0.005;
/// Falloff change per wheel notch.
const WHEEL_FALLOFF_STEP: f32 = 0.05;
/// Rotation change per wheel notch, in radians. Two degrees: fine enough to
/// line a spot up by eye, coarse enough to cross a quarter turn by hand.
const WHEEL_ROTATION_STEP: f32 = std::f32::consts::PI / 90.0;

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
    SetRotation {
        id: Uuid,
        rotation: f32,
    },
    AdjustRotation {
        id: Uuid,
        delta: f32,
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
    /// For a rotation grab, the angle between the spot's own axis and the
    /// direction the press came from, so a handle taken slightly off centre
    /// does not snap the spot round to meet the pointer.
    turn: f32,
}

/// Editing state for one interactive surface.
#[derive(Debug, Default)]
pub struct EditorInteraction {
    view: EditorView,
    transform: Transform,
    grab: Option<Grabbed>,
    pointer: Vec2,
    modifiers: Modifiers,
    surface_size: (u32, u32),
    has_pointer: bool,
}

impl EditorInteraction {
    pub fn new(transform: Transform) -> Self {
        Self {
            transform,
            surface_size: (1000, 1000),
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

    pub fn set_surface_size(&mut self, width: u32, height: u32) {
        self.surface_size = (width.max(1), height.max(1));
    }

    pub fn set_modifiers(&mut self, modifiers: Modifiers) {
        self.modifiers = modifiers;
    }

    pub fn release(&mut self) {
        self.grab = None;
    }

    /// The pointer left the surface; nothing is hovered or grabbed.
    pub fn leave(&mut self) {
        self.grab = None;
        self.has_pointer = false;
    }

    pub fn pointer_position(&self) -> Vec2 {
        self.pointer
    }

    /// Centre of the resize handle under the pointer, if any.
    pub fn hovered_handle(&self) -> Option<Vec2> {
        if !self.has_pointer {
            return None;
        }
        let (width, height) = self.surface_size;
        if let Some(grabbed) = self.grab {
            let defect = self.view.defects.iter().find(|d| d.id == grabbed.id)?;
            let handles = defect.handles();
            return match grabbed.grab {
                Grab::Center => None,
                Grab::Width => Some(nearest(&handles[0..2], self.pointer)),
                Grab::Height => Some(nearest(&handles[2..4], self.pointer)),
                Grab::Rotate => Some(defect.rotation_handle(height)),
            };
        }
        self.view.hovered_handle(self.pointer, width, height)
    }

    fn hit_test(&self, uv: Vec2) -> Option<(Uuid, Grab)> {
        let (width, height) = self.surface_size;
        self.view.hit_test(uv, width, height)
    }

    pub fn press(&mut self, uv: Vec2, button: Button) -> Option<EditorAction> {
        self.pointer = uv;
        self.has_pointer = true;
        match button {
            Button::Primary => match self.hit_test(uv) {
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
                let (id, _) = self.hit_test(uv)?;
                Some(EditorAction::ToggleEnabled(id))
            }
        }
    }

    pub fn motion(&mut self, uv: Vec2) -> Option<EditorAction> {
        self.pointer = uv;
        self.has_pointer = true;

        let Grabbed {
            id,
            grab,
            offset,
            turn,
        } = self.grab?;
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
            Grab::Width => EditorAction::SetRadiusX {
                id,
                radius: defect.ellipse().radius_from(at - defect.center, false),
            },
            Grab::Height => EditorAction::SetRadiusY {
                id,
                radius: defect.ellipse().radius_from(at - defect.center, true),
            },
            // The pointer itself carries the angle, so this one works from
            // where the pointer is rather than from the handle it took.
            Grab::Rotate => EditorAction::SetRotation {
                id,
                rotation: self.panel_angle(uv - defect.center, defect.aspect) + turn,
            },
        })
    }

    /// Direction of a surface offset as the stored profile would measure it:
    /// an angle on the glass, in the panel's own frame.
    fn panel_angle(&self, offset: Vec2, surface_aspect: f32) -> f32 {
        let panel_aspect = if self.transform.swaps_axes() {
            1.0 / surface_aspect
        } else {
            surface_aspect
        };
        angle_in_square(self.transform.direction_to_panel(offset), panel_aspect)
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
            Grab::Rotate => defect.rotation_handle(self.surface_size.1),
        };
        let turn = if grab == Grab::Rotate {
            self.panel_angle(grabbed - defect.center, defect.aspect)
                - self.panel_angle(uv - defect.center, defect.aspect)
        } else {
            0.0
        };
        Some(Grabbed {
            id,
            grab,
            offset: uv - grabbed,
            turn,
        })
    }

    /// One wheel notch; `notches` is positive when scrolling up.
    pub fn wheel(&mut self, notches: f32) -> Option<EditorAction> {
        let id = self
            .view
            .selected
            .or_else(|| self.hit_test(self.pointer).map(|(id, _)| id))?;

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
        } else if self.modifiers.alt {
            // Scrolling up turns the spot the way the pointer would, which on
            // a mirrored screen is the other way round in the stored frame.
            let handed = if self.transform.reflects() { -1.0 } else { 1.0 };
            EditorAction::AdjustRotation {
                id,
                delta: notches * WHEEL_ROTATION_STEP * handed,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::{EditorDefect, ShowMode};

    fn view(center: Vec2, radius: Vec2) -> (Uuid, EditorView) {
        let id = Uuid::new_v4();
        let defect = EditorDefect {
            id,
            center,
            radius,
            rotation: 0.0,
            aspect: 1.0,
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
        editor.set_surface_size(1000, 1000);
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

    /// A drag has to be measured on the glass: on a 2:1 screen, pulling the
    /// width handle of a spot standing on end by 0.3 of the height must set a
    /// radius that is the same number of pixels, not the same fraction.
    #[test]
    fn a_resize_is_measured_in_pixels_not_in_normalized_units() {
        let (id, mut view) = view(Vec2::splat(0.5), Vec2::splat(0.1));
        view.defects[0].rotation = std::f32::consts::FRAC_PI_2;
        view.defects[0].aspect = 2.0;
        let mut editor = EditorInteraction::new(Transform::Normal);
        editor.set_view(view, Transform::Normal);
        editor.set_surface_size(2000, 1000);

        // The width axis points up now, so its handle is above the centre.
        editor.press(Vec2::new(0.5, 0.7), Button::Primary);
        match editor.motion(Vec2::new(0.5, 0.8)) {
            Some(EditorAction::SetRadiusX { id: got, radius }) => {
                assert_eq!(got, id);
                // 0.3 of a 1000 px height is 300 px, which is 0.15 of a 2000
                // px width.
                assert!((radius - 0.15).abs() < 1e-5, "{radius}");
            }
            other => panic!("expected a width change, got {other:?}"),
        }
    }

    #[test]
    fn dragging_the_rotation_handle_turns_the_spot() {
        let (id, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.1));
        let handle = {
            let (_, view) = view(Vec2::splat(0.5), Vec2::splat(0.1));
            view.defects[0].rotation_handle(1000)
        };
        assert_eq!(editor.press(handle, Button::Primary), None);

        // Straight up from the centre is a quarter turn counter-clockwise, and
        // y grows downwards.
        match editor.motion(Vec2::new(0.5, 0.2)) {
            Some(EditorAction::SetRotation { id: got, rotation }) => {
                assert_eq!(got, id);
                assert!(
                    (rotation + std::f32::consts::FRAC_PI_2).abs() < 1e-4,
                    "{rotation}"
                );
            }
            other => panic!("expected a rotation, got {other:?}"),
        }
    }

    /// The handle is a square several pixels across, so a press rarely lands on
    /// its exact centre. That must not jerk the spot round to meet the pointer.
    #[test]
    fn taking_the_rotation_handle_off_centre_does_not_snap_the_spot() {
        let (_, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.1));
        let handle = {
            let (_, view) = view(Vec2::splat(0.5), Vec2::splat(0.1));
            view.defects[0].rotation_handle(1000)
        };
        let beside = Vec2::new(handle.x, handle.y + 4.0 / 1000.0);
        editor.press(beside, Button::Primary);

        match editor.motion(beside) {
            Some(EditorAction::SetRotation { rotation, .. }) => {
                assert!(rotation.abs() < 1e-4, "the spot jumped to {rotation}");
            }
            other => panic!("expected a rotation, got {other:?}"),
        }
    }

    #[test]
    fn a_rotation_drag_comes_back_in_panel_coordinates() {
        let (_, view) = view(Vec2::splat(0.5), Vec2::splat(0.1));
        let handle = view.defects[0].rotation_handle(1000);
        let mut editor = EditorInteraction::new(Transform::Rotate90);
        editor.set_view(view, Transform::Rotate90);
        editor.set_surface_size(1000, 1000);
        editor.press(handle, Button::Primary);

        match editor.motion(Vec2::new(0.5, 0.2)) {
            Some(EditorAction::SetRotation { rotation, .. }) => {
                // Under a quarter turn of the screen, dragging the handle to
                // the top of the surface points the spot along the panel's x.
                assert!(rotation.abs() < 1e-4, "{rotation}");
            }
            other => panic!("expected a rotation, got {other:?}"),
        }
    }

    #[test]
    fn alt_redirects_the_wheel_to_the_rotation() {
        let (id, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.1));
        editor.set_modifiers(Modifiers {
            alt: true,
            ..Default::default()
        });
        match editor.wheel(2.0) {
            Some(EditorAction::AdjustRotation { id: got, delta }) => {
                assert_eq!(got, id);
                assert!((delta - 4.0f32.to_radians()).abs() < 1e-5, "{delta}");
            }
            other => panic!("expected a rotation, got {other:?}"),
        }

        // A mirrored screen turns the other way, so the pointer and the spot
        // still agree.
        let (_, view) = view(Vec2::splat(0.5), Vec2::splat(0.1));
        let mut flipped = EditorInteraction::new(Transform::Flipped);
        flipped.set_view(view, Transform::Flipped);
        flipped.set_modifiers(Modifiers {
            alt: true,
            ..Default::default()
        });
        match flipped.wheel(2.0) {
            Some(EditorAction::AdjustRotation { delta, .. }) => assert!(delta < 0.0, "{delta}"),
            other => panic!("expected a rotation, got {other:?}"),
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
    fn a_press_beside_a_handle_grabs_the_body() {
        let (_, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.1));
        // One pixel inside the ellipse from the right-hand handle on a
        // 1000 px surface: the old circular tolerance would have grabbed Width.
        let beside = Vec2::new(594.0 / 1000.0, 0.5);
        editor.press(beside, Button::Primary);
        match editor.motion(Vec2::new(0.55, 0.5)) {
            Some(EditorAction::Move { .. }) => {}
            other => panic!("expected a move, got {other:?}"),
        }
    }

    #[test]
    fn hovering_a_handle_reports_its_centre() {
        let (_, mut editor) = editor(Vec2::splat(0.5), Vec2::splat(0.1));
        editor.motion(Vec2::new(0.6, 0.5));
        let hovered = editor.hovered_handle().expect("on the handle");
        assert!((hovered.x - 0.6).abs() < 1e-5);
        assert!((hovered.y - 0.5).abs() < 1e-5);

        editor.motion(Vec2::new(0.5, 0.5));
        assert_eq!(editor.hovered_handle(), None);

        editor.leave();
        editor.motion(Vec2::new(0.6, 0.5));
        editor.leave();
        assert_eq!(editor.hovered_handle(), None);
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
            aspect: 1.0,
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
