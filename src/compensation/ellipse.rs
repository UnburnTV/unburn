//! The shape a defect describes on a panel, in one place.
//!
//! Everything is stored in normalized coordinates so a profile survives a
//! resolution change, but those coordinates are not isotropic: one unit of `x`
//! is a whole panel width and one unit of `y` a whole height. Rotating a pair
//! of normalized radii therefore does not turn the shape on the glass, it
//! shears it -- a spot that looked round becomes a slanted ellipse of the same
//! area, which is not what anybody asks a rotation control for.
//!
//! So every angle here is measured in *square space*: the same offsets scaled
//! so that both axes are in one physical unit, the panel's height. A rotation
//! in that space is a rigid turn of the pixels, which is what the eye sees, and
//! it is exactly the identity on a spot that is already round.

use super::Vec2;

/// Smallest semi-axis, in square space, that keeps the divisions below safe.
const MIN_EXTENT: f32 = 1.0e-4;

/// A normalized offset with both axes in units of the space's height.
pub fn to_square(offset: Vec2, aspect: f32) -> Vec2 {
    Vec2::new(offset.x * aspect, offset.y)
}

/// The inverse of [`to_square`]: back to normalized coordinates.
pub fn from_square(offset: Vec2, aspect: f32) -> Vec2 {
    Vec2::new(offset.x / aspect, offset.y)
}

/// Direction of a normalized offset, as an angle in square space.
pub fn angle_in_square(offset: Vec2, aspect: f32) -> f32 {
    let s = to_square(offset, aspect);
    s.y.atan2(s.x)
}

/// An ellipse in normalized coordinates, rotated in square space.
///
/// This is the one place the relation between the stored numbers and the shape
/// on the glass is written down; the model, the on-screen outlines and the
/// hit-testing all read it from here so they cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ellipse {
    pub center: Vec2,
    /// Semi-axes before the rotation: `x` as a fraction of the width, `y` of
    /// the height, so an unrotated ellipse is spelled the obvious way.
    pub radius: Vec2,
    /// Counter-clockwise turn of those axes, in radians, in square space.
    pub rotation: f32,
    /// Width divided by height of the space the ellipse lives in.
    pub aspect: f32,
}

impl Ellipse {
    pub fn new(center: Vec2, radius: Vec2, rotation: f32, aspect: f32) -> Self {
        let aspect = if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            1.0
        };
        Self {
            center,
            radius,
            rotation,
            aspect,
        }
    }

    /// The semi-axes in square space, where the rotation happens.
    fn extent(&self) -> Vec2 {
        Vec2::new(
            (self.radius.x * self.aspect).abs().max(MIN_EXTENT),
            self.radius.y.abs().max(MIN_EXTENT),
        )
    }

    /// `uv` in the ellipse's own frame, `1.0` away on the nominal contour.
    pub fn normalized(&self, uv: Vec2) -> Vec2 {
        let s = to_square(uv - self.center, self.aspect);
        let (sin, cos) = self.rotation.sin_cos();
        let extent = self.extent();
        Vec2::new(
            (s.x * cos + s.y * sin) / extent.x,
            (-s.x * sin + s.y * cos) / extent.y,
        )
    }

    /// Normalized elliptical distance: `1.0` on the nominal contour.
    pub fn distance(&self, uv: Vec2) -> f32 {
        self.normalized(uv).length()
    }

    /// The two semi-axis vectors, as normalized offsets from the centre.
    ///
    /// The first is the rotated width axis, the second the height axis. They
    /// are perpendicular on the glass, not in these coordinates.
    pub fn axes(&self) -> (Vec2, Vec2) {
        let (sin, cos) = self.rotation.sin_cos();
        let extent = self.extent();
        (
            from_square(Vec2::new(extent.x * cos, extent.x * sin), self.aspect),
            from_square(Vec2::new(-extent.y * sin, extent.y * cos), self.aspect),
        )
    }

    /// A point on the contour, `scale` times the nominal radii out, at
    /// parameter `t` radians around it.
    pub fn contour(&self, t: f32, scale: f32) -> Vec2 {
        let (sin, cos) = t.sin_cos();
        let (along, across) = self.axes();
        self.center + along * (cos * scale) + across * (sin * scale)
    }

    /// Axis-aligned normalized bounds of the contour at `scale`.
    ///
    /// Exact rather than merely sufficient: the extreme of
    /// `along·cos t + across·sin t` in either axis is the hypotenuse of that
    /// axis' two components.
    pub fn bounds(&self, scale: f32) -> (Vec2, Vec2) {
        let (along, across) = self.axes();
        let hx = (along.x * scale).hypot(across.x * scale);
        let hy = (along.y * scale).hypot(across.y * scale);
        (
            Vec2::new(self.center.x - hx, self.center.y - hy),
            Vec2::new(self.center.x + hx, self.center.y + hy),
        )
    }

    /// The semi-axis a handle dragged to `delta` from the centre implies,
    /// expressed the way [`Ellipse::radius`] is: as a fraction of the width for
    /// the width axis, of the height for the height axis.
    pub fn radius_from(&self, delta: Vec2, across: bool) -> f32 {
        let s = to_square(delta, self.aspect);
        let (sin, cos) = self.rotation.sin_cos();
        if across {
            (-s.x * sin + s.y * cos).abs().max(MIN_EXTENT)
        } else {
            (s.x * cos + s.y * sin).abs().max(MIN_EXTENT) / self.aspect
        }
    }
}

/// Fold a rotation into `(-pi, pi]`, so a control with a full turn on its rail
/// can always show what a drag on screen produced.
pub fn normalize_rotation(radians: f32) -> f32 {
    if !radians.is_finite() {
        return 0.0;
    }
    let tau = std::f32::consts::TAU;
    let folded = (radians + std::f32::consts::PI).rem_euclid(tau) - std::f32::consts::PI;
    // `rem_euclid` lands on the open end; -pi and pi are the same angle.
    if folded <= -std::f32::consts::PI {
        folded + tau
    } else {
        folded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};

    /// A spot that is round on a 16:9 panel: its normalized radii differ by
    /// exactly the aspect ratio.
    fn round_spot(rotation: f32) -> Ellipse {
        let aspect = 16.0 / 9.0;
        Ellipse::new(
            Vec2::splat(0.5),
            Vec2::new(0.1, 0.1 * aspect),
            rotation,
            aspect,
        )
    }

    #[test]
    fn an_unrotated_ellipse_is_read_straight_off_its_radii() {
        for aspect in [1.0, 16.0 / 9.0, 0.5625] {
            let e = Ellipse::new(Vec2::splat(0.5), Vec2::new(0.2, 0.05), 0.0, aspect);
            assert_relative_eq!(e.distance(Vec2::new(0.7, 0.5)), 1.0, epsilon = 1e-5);
            assert_relative_eq!(e.distance(Vec2::new(0.5, 0.55)), 1.0, epsilon = 1e-5);
            let (along, across) = e.axes();
            assert_relative_eq!(along.x, 0.2, epsilon = 1e-6);
            assert_relative_eq!(across.y, 0.05, epsilon = 1e-6);
        }
    }

    /// The bug this module exists for: turning something round has to leave it
    /// round, whatever the panel's aspect ratio. Rotating the stored radii
    /// instead sheared it into a slant.
    #[test]
    fn rotating_a_round_spot_does_not_deform_it() {
        let upright = round_spot(0.0);
        for turn in [0.2, FRAC_PI_4, FRAC_PI_2, 1.9] {
            let turned = round_spot(turn);
            for probe in [
                Vec2::new(0.6, 0.5),
                Vec2::new(0.5, 0.62),
                Vec2::new(0.56, 0.57),
                Vec2::new(0.44, 0.61),
            ] {
                assert_relative_eq!(
                    turned.distance(probe),
                    upright.distance(probe),
                    epsilon = 1e-5
                );
            }
        }
    }

    #[test]
    fn a_quarter_turn_swaps_the_axes_on_the_glass() {
        let aspect = 2.0;
        // 0.2 of the width is 0.4 of the height; 0.05 of the height stays 0.05.
        let e = Ellipse::new(Vec2::splat(0.5), Vec2::new(0.2, 0.05), FRAC_PI_2, aspect);
        // The long axis now points along y, and 0.4 of a height is 0.4 of y.
        assert_relative_eq!(e.distance(Vec2::new(0.5, 0.9)), 1.0, epsilon = 1e-5);
        // The short one points along x: 0.05 of a height is 0.025 of a width.
        assert_relative_eq!(e.distance(Vec2::new(0.525, 0.5)), 1.0, epsilon = 1e-5);
    }

    #[test]
    fn the_axes_reach_the_contour() {
        let e = Ellipse::new(Vec2::new(0.4, 0.6), Vec2::new(0.15, 0.05), 0.7, 1.6);
        let (along, across) = e.axes();
        assert_relative_eq!(e.distance(e.center + along), 1.0, epsilon = 1e-5);
        assert_relative_eq!(e.distance(e.center - across), 1.0, epsilon = 1e-5);
        for i in 0..16 {
            let t = i as f32 / 16.0 * std::f32::consts::TAU;
            assert_relative_eq!(e.distance(e.contour(t, 1.0)), 1.0, epsilon = 1e-5);
            assert_relative_eq!(e.distance(e.contour(t, 2.0)), 2.0, epsilon = 1e-5);
        }
    }

    #[test]
    fn bounds_touch_the_contour_and_never_cut_it() {
        let e = Ellipse::new(Vec2::splat(0.5), Vec2::new(0.15, 0.05), 0.6, 1.7);
        let (min, max) = e.bounds(1.0);
        let mut widest = 0.0f32;
        for i in 0..512 {
            let t = i as f32 / 512.0 * std::f32::consts::TAU;
            let p = e.contour(t, 1.0);
            assert!(p.x >= min.x - 1e-5 && p.x <= max.x + 1e-5, "{p:?}");
            assert!(p.y >= min.y - 1e-5 && p.y <= max.y + 1e-5, "{p:?}");
            widest = widest.max((p.x - 0.5).abs());
        }
        assert_relative_eq!(widest, max.x - 0.5, epsilon = 1e-3);
    }

    /// Dragging a handle has to be the inverse of drawing it, or a grab would
    /// resize the spot the instant it was taken.
    #[test]
    fn a_handle_dragged_where_it_already_is_changes_nothing() {
        let e = Ellipse::new(Vec2::splat(0.5), Vec2::new(0.12, 0.04), 0.9, 1.85);
        let (along, across) = e.axes();
        assert_relative_eq!(e.radius_from(along, false), 0.12, epsilon = 1e-6);
        assert_relative_eq!(e.radius_from(across, true), 0.04, epsilon = 1e-6);
        // Either end of an axis says the same thing.
        assert_relative_eq!(e.radius_from(along * -1.0, false), 0.12, epsilon = 1e-6);
    }

    #[test]
    fn an_angle_is_measured_on_the_glass_not_in_the_coordinates() {
        // Half a width across and a quarter height up on a 2:1 panel is one
        // height across and a quarter up: shallow, not the 26 degrees the bare
        // normalized numbers suggest.
        let angle = angle_in_square(Vec2::new(0.5, 0.25), 2.0);
        assert_relative_eq!(angle, 0.25f32.atan(), epsilon = 1e-6);
    }

    #[test]
    fn rotations_fold_onto_a_single_turn() {
        assert_relative_eq!(normalize_rotation(0.3), 0.3, epsilon = 1e-6);
        assert_relative_eq!(normalize_rotation(-0.3), -0.3, epsilon = 1e-6);
        assert_relative_eq!(normalize_rotation(PI), PI, epsilon = 1e-6);
        assert_relative_eq!(normalize_rotation(-PI), PI, epsilon = 1e-6);
        assert_relative_eq!(
            normalize_rotation(PI + FRAC_PI_2),
            -FRAC_PI_2,
            epsilon = 1e-5
        );
        assert_relative_eq!(
            normalize_rotation(7.0 * std::f32::consts::TAU + 0.4),
            0.4,
            epsilon = 1e-4
        );
        assert_eq!(normalize_rotation(f32::NAN), 0.0);
    }
}
