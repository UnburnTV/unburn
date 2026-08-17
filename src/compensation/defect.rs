//! The defect enum and the rules for combining several defects.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{radial::RadialDefect, Rgb, Vec2};

/// Anything that can describe a region where the panel is off-brightness.
pub trait DefectModel {
    /// Relative brightness the panel delivers at `uv` per channel, where `1.0`
    /// is a healthy pixel. Above `1.0` is a spot that emits too much light.
    fn gain_at(&self, uv: Vec2) -> Rgb;

    /// Normalized axis-aligned box outside of which `gain_at` is
    /// indistinguishable from `1.0`. Used to skip work during mask generation.
    fn bounds(&self) -> (Vec2, Vec2);
}

/// Discriminant used by the GUI and by the on-disk format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefectKind {
    Radial,
}

impl DefectKind {
    pub fn label(self) -> &'static str {
        match self {
            DefectKind::Radial => "radial",
        }
    }
}

/// A single modelled panel defect.
///
/// This is an enum from the very beginning even though only one variant exists,
/// so that gradients, polygons and painted or imported masks can be added
/// without disturbing anything that stores or renders defects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Defect {
    Radial(RadialDefect),
}

impl Defect {
    pub fn id(&self) -> Uuid {
        match self {
            Defect::Radial(d) => d.id,
        }
    }

    pub fn kind(&self) -> DefectKind {
        match self {
            Defect::Radial(_) => DefectKind::Radial,
        }
    }

    pub fn enabled(&self) -> bool {
        match self {
            Defect::Radial(d) => d.enabled,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        match self {
            Defect::Radial(d) => d.enabled = enabled,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Defect::Radial(d) => &d.name,
        }
    }

    pub fn set_name(&mut self, name: String) {
        match self {
            Defect::Radial(d) => d.name = name,
        }
    }

    pub fn center(&self) -> Vec2 {
        match self {
            Defect::Radial(d) => d.center,
        }
    }

    pub fn set_center(&mut self, center: Vec2) {
        match self {
            Defect::Radial(d) => d.center = center,
        }
    }

    pub fn as_radial(&self) -> Option<&RadialDefect> {
        match self {
            Defect::Radial(d) => Some(d),
        }
    }

    pub fn as_radial_mut(&mut self) -> Option<&mut RadialDefect> {
        match self {
            Defect::Radial(d) => Some(d),
        }
    }
}

impl DefectModel for Defect {
    fn gain_at(&self, uv: Vec2) -> Rgb {
        match self {
            Defect::Radial(d) => d.gain_at(uv),
        }
    }

    fn bounds(&self) -> (Vec2, Vec2) {
        match self {
            Defect::Radial(d) => d.bounds(),
        }
    }
}

/// How the individual defect responses are combined into `D(x, y)`.
///
/// Multiplicative composition is the specified behaviour and the only one that
/// handles overlap sensibly; the enum exists so alternatives can be tried
/// without touching the mask generator's call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Composition {
    #[default]
    Multiplicative,
    /// Take the worst defect at each point instead of stacking them.
    #[serde(alias = "minimum")]
    Strongest,
}

impl Composition {
    pub const ALL: [Composition; 2] = [Composition::Multiplicative, Composition::Strongest];

    pub fn label(self) -> &'static str {
        match self {
            Composition::Multiplicative => "Multiplicative",
            Composition::Strongest => "Strongest",
        }
    }

    /// Neutral element to start an accumulation from.
    pub fn identity(self) -> Rgb {
        Rgb::ONE
    }

    pub fn combine(self, accumulated: Rgb, next: Rgb) -> Rgb {
        match self {
            Composition::Multiplicative => accumulated.zip(next, |a, b| a * b),
            // "Worst" is the largest deviation from healthy in either direction,
            // so this works for bright and dim defects alike.
            Composition::Strongest => accumulated.zip(next, |a, b| {
                if (b - 1.0).abs() > (a - 1.0).abs() {
                    b
                } else {
                    a
                }
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiplicative_composition_stacks_overlap() {
        let c = Composition::Multiplicative;
        let g = c.combine(c.combine(c.identity(), Rgb::splat(1.1)), Rgb::splat(1.1));
        assert!((g.r - 1.21).abs() < 1e-6);
    }

    #[test]
    fn strongest_composition_keeps_the_worst_in_either_direction() {
        let c = Composition::Strongest;
        let bright = c.combine(c.combine(c.identity(), Rgb::splat(1.1)), Rgb::splat(1.2));
        assert!((bright.r - 1.2).abs() < 1e-6);

        let dim = c.combine(c.combine(c.identity(), Rgb::splat(0.9)), Rgb::splat(0.8));
        assert!((dim.r - 0.8).abs() < 1e-6);
    }

    #[test]
    fn composition_works_channel_by_channel() {
        let c = Composition::Multiplicative;
        let g = c.combine(Rgb::new(1.2, 1.0, 1.0), Rgb::new(1.0, 1.5, 1.0));
        assert_eq!(g, Rgb::new(1.2, 1.5, 1.0));
    }

    #[test]
    fn the_old_composition_name_still_parses() {
        #[derive(serde::Deserialize)]
        struct Holder {
            composition: Composition,
        }
        let holder: Holder = toml::from_str("composition = \"minimum\"").unwrap();
        assert_eq!(holder.composition, Composition::Strongest);
    }

    #[test]
    fn defect_roundtrips_through_toml() {
        let d = Defect::Radial(RadialDefect::default());
        let text = toml::to_string(&d).unwrap();
        assert!(text.contains("kind = \"radial\""));
        let back: Defect = toml::from_str(&text).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn a_neutral_strength_stays_a_bare_number() {
        let d = Defect::Radial(RadialDefect {
            strength: Rgb::splat(0.11),
            ..Default::default()
        });
        assert!(toml::to_string(&d).unwrap().contains("strength = 0.11"));
    }

    #[test]
    fn a_tinted_strength_round_trips_as_a_triple() {
        let d = Defect::Radial(RadialDefect {
            strength: Rgb::new(0.2, 0.1, 0.05),
            ..Default::default()
        });
        let text = toml::to_string(&d).unwrap();
        assert!(text.contains("strength = [0.2, 0.1, 0.05]"), "{text}");
        assert_eq!(toml::from_str::<Defect>(&text).unwrap(), d);
    }
}
