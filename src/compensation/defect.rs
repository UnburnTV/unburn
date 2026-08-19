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

/// A single modelled panel defect.
///
/// This is an enum from the very beginning even though only one variant exists,
/// so that gradients, polygons and painted or imported masks can be added
/// without disturbing anything that stores or renders defects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "StoredDefect", into = "StoredDefect")]
pub enum Defect {
    Radial(RadialDefect),
}

/// How a defect is written to and read from a profile.
///
/// An ordinary profile writes the radial fields and nothing else. A `kind` key
/// is still read, so a hand-written `kind = "radial"` keeps loading and a kind
/// we cannot represent is refused rather than silently flattened into a
/// Gaussian. Serializing `RadialDefect` directly would leave nowhere to put
/// that check.
#[derive(Serialize, Deserialize)]
struct StoredDefect {
    #[serde(default, skip_serializing)]
    kind: Option<String>,
    #[serde(flatten)]
    radial: RadialDefect,
}

impl TryFrom<StoredDefect> for Defect {
    type Error = String;

    fn try_from(stored: StoredDefect) -> Result<Self, Self::Error> {
        match stored.kind.as_deref() {
            None | Some("radial") => Ok(Defect::Radial(stored.radial)),
            Some(kind) => Err(format!("unknown defect kind `{kind}`")),
        }
    }
}

impl From<Defect> for StoredDefect {
    fn from(defect: Defect) -> Self {
        match defect {
            Defect::Radial(radial) => StoredDefect { kind: None, radial },
        }
    }
}

impl Defect {
    pub fn id(&self) -> Uuid {
        match self {
            Defect::Radial(d) => d.id,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defects_round_trip_without_persisting_runtime_ids() {
        for defect in [
            Defect::Radial(RadialDefect::default()),
            Defect::Radial(RadialDefect {
                enabled: false,
                rotation: 0.5,
                strength: Rgb::splat(0.11),
                ..Default::default()
            }),
            Defect::Radial(RadialDefect {
                strength: Rgb::new(0.2, 0.1, 0.05),
                ..Default::default()
            }),
        ] {
            let text = toml::to_string(&defect).unwrap();
            assert!(
                !text.contains("id"),
                "runtime ids must not be saved:\n{text}"
            );

            let back: Defect = toml::from_str(&text).unwrap();
            let (original, back) = (defect.as_radial().unwrap(), back.as_radial().unwrap());
            assert_ne!(back.id, original.id, "loading must mint a runtime id");
            assert_eq!(back.center, original.center);
            assert_eq!(back.radius, original.radius);
            assert_eq!(back.rotation, original.rotation);
            assert_eq!(back.strength, original.strength);
            assert_eq!(back.enabled, original.enabled);
        }
    }

    /// Profiles written before any of this became optional must keep loading:
    /// the fields that are now implied, the readable defect ids such a profile
    /// may pin, and the names defects no longer have.
    #[test]
    fn a_profile_that_spells_everything_out_still_loads() {
        let defect: Defect = toml::from_str(
            r#"
            kind = "radial"
            id = "top-left"
            name = "Spot 1"
            enabled = true
            center = [0.2, 0.3]
            radius = [0.1, 0.1]
            rotation = 0.0
            strength = 0.08
            falloff = 1.0
            "#,
        )
        .unwrap();
        assert_eq!(defect.center(), Vec2::new(0.2, 0.3));
        assert!(defect.enabled());
    }

    #[test]
    fn an_unknown_kind_is_refused_rather_than_taken_for_a_radial_one() {
        let text = "kind = \"polygon\"\ncenter = [0.5, 0.5]\nradius = [0.1, 0.1]\nstrength = 0.1\n";
        assert!(toml::from_str::<Defect>(text).is_err());
    }
}
