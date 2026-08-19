//! The elliptical Gaussian defect.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{defect::DefectModel, Rgb, Vec2};

/// Smallest radius we allow, to keep `gain_at` free of divisions by zero.
pub const MIN_RADIUS: f32 = 1.0e-4;

/// An elliptical Gaussian blemish.
///
/// Deliberately carries no name. A defect is one of a handful of spots on one
/// panel, and its position in the profile is the only handle a person needs; a
/// stored name would be a second, redundant way to refer to the same entry, free
/// to drift out of step with the order it is presented in.
///
/// The brightness response contributed by this defect is
/// `1 + strength * exp(-0.5 * r²^falloff)` where `r` is the distance from the
/// centre expressed in units of the (rotated) radii. `falloff = 1` is the plain
/// Gaussian of the specification; larger values flatten the centre and sharpen
/// the edge, smaller values produce a longer, softer tail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadialDefect {
    /// Handle the GUI, the on-screen editor and the overlay process use to agree
    /// on which defect is selected, where an index would not survive a defect
    /// being inserted or removed ahead of it.
    ///
    /// Never written to a profile: nothing refers to a defect between runs, so a
    /// stored id would be noise, and a fresh one on load serves just as well. It
    /// is still *read*, so that a hand-written profile can pin a readable handle
    /// of its own.
    #[serde(
        skip_serializing,
        default = "Uuid::new_v4",
        deserialize_with = "lenient_uuid"
    )]
    pub id: Uuid,
    #[serde(default = "enabled_by_default", skip_serializing_if = "is_enabled")]
    pub enabled: bool,

    pub center: Vec2,
    pub radius: Vec2,
    /// Rotation of the ellipse in radians, counter-clockwise.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rotation: f32,

    /// Peak brightness excess at the centre, per channel.
    ///
    /// Positive means the spot emits more light than the rest of the panel, so
    /// the overlay darkens the spot itself. Negative describes a dim patch,
    /// which can only be matched by dimming everything around it instead.
    pub strength: Rgb,
    /// Edge softness exponent.
    #[serde(default = "unit_falloff")]
    pub falloff: f32,
}

fn enabled_by_default() -> bool {
    true
}

fn unit_falloff() -> f32 {
    1.0
}

/// A profile records only the defects that are *switched off*, since that is the
/// unusual state and the one worth spelling out.
fn is_enabled(enabled: &bool) -> bool {
    *enabled
}

fn is_zero(value: &f32) -> bool {
    *value == 0.0
}

/// Accept any string as a defect id, so hand-written profiles may use readable
/// names like `spot-1` instead of a UUID. Non-UUID text is folded into a
/// deterministic UUID so it keeps referring to the same defect across saves.
fn lenient_uuid<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Uuid, D::Error> {
    let text = String::deserialize(deserializer)?;
    if let Ok(uuid) = Uuid::parse_str(&text) {
        return Ok(uuid);
    }
    let mut bytes = [0u8; 16];
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (i, byte) in text.as_bytes().iter().enumerate() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
        bytes[8 + (i % 8)] ^= (hash >> ((i % 8) * 8)) as u8;
    }
    bytes[..8].copy_from_slice(&hash.to_le_bytes());
    Ok(Uuid::from_bytes(bytes))
}

impl Default for RadialDefect {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            enabled: true,
            center: Vec2::splat(0.5),
            radius: Vec2::splat(0.1),
            rotation: 0.0,
            strength: Rgb::splat(0.08),
            falloff: 1.0,
        }
    }
}

impl RadialDefect {
    /// A new defect centred on `center`, sized so that it looks circular on a
    /// panel with the given pixel aspect ratio (width / height).
    pub fn new_at(center: Vec2, aspect: f32) -> Self {
        let radius_x = 0.08_f32;
        Self {
            center,
            radius: Vec2::new(radius_x, radius_x * aspect.max(0.01)),
            ..Default::default()
        }
    }

    /// Multiply both radii, keeping the ellipse's shape.
    pub fn scale_radius(&mut self, factor: f32) {
        self.radius.x = (self.radius.x * factor).clamp(MIN_RADIUS, 4.0);
        self.radius.y = (self.radius.y * factor).clamp(MIN_RADIUS, 4.0);
    }

    /// Coordinates of `uv` in the defect's own rotated, radius-normalized frame.
    fn local(&self, uv: Vec2) -> Vec2 {
        let d = uv - self.center;
        let (sin, cos) = self.rotation.sin_cos();
        // Rotate by -rotation to undo the ellipse's own rotation.
        let x = d.x * cos + d.y * sin;
        let y = -d.x * sin + d.y * cos;
        Vec2::new(
            x / self.radius.x.max(MIN_RADIUS),
            y / self.radius.y.max(MIN_RADIUS),
        )
    }

    /// Normalized elliptical distance: `1.0` on the nominal radius contour.
    pub fn normalized_distance(&self, uv: Vec2) -> f32 {
        self.local(uv).length()
    }

    /// The unit Gaussian profile `d_i(x, y)` from the specification: `1.0` at
    /// the centre, falling to zero far away.
    pub fn profile_at(&self, uv: Vec2) -> f32 {
        let l = self.local(uv);
        let r2 = l.x * l.x + l.y * l.y;
        if r2 <= 0.0 {
            return 1.0;
        }
        let shaped = if (self.falloff - 1.0).abs() < f32::EPSILON {
            r2
        } else {
            r2.powf(self.falloff)
        };
        (-0.5 * shaped).exp()
    }

    /// How much brighter than a healthy pixel this defect makes `uv`.
    pub fn excess_at(&self, uv: Vec2) -> Rgb {
        self.strength * self.profile_at(uv)
    }
}

impl DefectModel for RadialDefect {
    fn gain_at(&self, uv: Vec2) -> Rgb {
        (Rgb::ONE + self.excess_at(uv)).map(|g| g.max(0.0))
    }

    /// Beyond roughly four sigma the Gaussian contributes less than 0.04 % of
    /// its peak, which is far below the 8-bit quantum of the final alpha.
    fn bounds(&self) -> (Vec2, Vec2) {
        let extent = 4.0 / self.falloff.max(0.2);
        let rx = self.radius.x.abs() * extent;
        let ry = self.radius.y.abs() * extent;
        // Axis-aligned bound of the rotated ellipse's own bounding box.
        let (sin, cos) = self.rotation.sin_cos();
        let hx = (rx * cos).abs() + (ry * sin).abs();
        let hy = (rx * sin).abs() + (ry * cos).abs();
        (
            Vec2::new(self.center.x - hx, self.center.y - hy),
            Vec2::new(self.center.x + hx, self.center.y + hy),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    fn unit_spot(strength: Rgb) -> RadialDefect {
        RadialDefect {
            center: Vec2::new(0.5, 0.5),
            radius: Vec2::new(0.1, 0.1),
            strength,
            ..Default::default()
        }
    }

    #[test]
    fn peak_excess_is_strength() {
        let d = RadialDefect {
            strength: Rgb::splat(0.12),
            ..Default::default()
        };
        assert_relative_eq!(d.excess_at(d.center).r, 0.12, epsilon = 1e-6);
        // A spot that emits 12 % too much light.
        assert_relative_eq!(d.gain_at(d.center).r, 1.12, epsilon = 1e-6);
    }

    #[test]
    fn a_negative_strength_describes_a_dim_patch() {
        let d = RadialDefect {
            strength: Rgb::splat(-0.12),
            ..Default::default()
        };
        assert_relative_eq!(d.gain_at(d.center).r, 0.88, epsilon = 1e-6);
        assert_relative_eq!(d.gain_at(Vec2::ZERO).r, 1.0, epsilon = 1e-6);
    }

    #[test]
    fn each_channel_carries_its_own_strength() {
        let d = unit_spot(Rgb::new(0.2, 0.1, 0.0));
        let peak = d.excess_at(d.center);
        assert_relative_eq!(peak.r, 0.2, epsilon = 1e-6);
        assert_relative_eq!(peak.g, 0.1, epsilon = 1e-6);
        assert_relative_eq!(peak.b, 0.0, epsilon = 1e-6);

        // The same Gaussian shapes every channel.
        let off = d.excess_at(Vec2::new(0.6, 0.5));
        assert_relative_eq!(off.r / peak.r, (-0.5f32).exp(), epsilon = 1e-6);
        assert_relative_eq!(off.g / peak.g, (-0.5f32).exp(), epsilon = 1e-6);
    }

    #[test]
    fn gaussian_falls_off_as_specified() {
        let d = unit_spot(Rgb::ONE);
        // One sigma away: exp(-0.5).
        assert_relative_eq!(
            d.profile_at(Vec2::new(0.6, 0.5)),
            (-0.5f32).exp(),
            epsilon = 1e-6
        );
        // Two sigma away: exp(-2).
        assert_relative_eq!(
            d.profile_at(Vec2::new(0.7, 0.5)),
            (-2.0f32).exp(),
            epsilon = 1e-6
        );
    }

    #[test]
    fn ellipse_respects_separate_radii() {
        let d = RadialDefect {
            radius: Vec2::new(0.2, 0.05),
            ..unit_spot(Rgb::ONE)
        };
        let along_x = d.profile_at(Vec2::new(0.7, 0.5));
        let along_y = d.profile_at(Vec2::new(0.5, 0.55));
        assert_relative_eq!(along_x, along_y, epsilon = 1e-6);
    }

    #[test]
    fn rotation_turns_the_ellipse() {
        let d = RadialDefect {
            radius: Vec2::new(0.2, 0.05),
            rotation: std::f32::consts::FRAC_PI_2,
            ..unit_spot(Rgb::ONE)
        };
        // With a quarter turn the long axis now points along y.
        assert_relative_eq!(
            d.profile_at(Vec2::new(0.5, 0.7)),
            (-0.5f32).exp(),
            epsilon = 1e-5
        );
    }

    #[test]
    fn gain_never_goes_negative() {
        let d = RadialDefect {
            strength: Rgb::splat(-5.0),
            ..Default::default()
        };
        assert_eq!(d.gain_at(d.center).r, 0.0);
    }

    #[test]
    fn bounds_contain_the_significant_part() {
        let d = RadialDefect {
            radius: Vec2::new(0.1, 0.05),
            ..unit_spot(Rgb::ONE)
        };
        let (min, max) = d.bounds();
        assert!(d.profile_at(Vec2::new(min.x, 0.5)) < 1e-3);
        assert!(d.profile_at(Vec2::new(max.x, 0.5)) < 1e-3);
        assert!(min.x < 0.5 && max.x > 0.5);
    }
}
