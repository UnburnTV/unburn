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
    fn defect_roundtrips_through_toml() {
        let d = Defect::Radial(RadialDefect::default());
        let back: Defect = toml::from_str(&toml::to_string(&d).unwrap()).unwrap();
        // The id is minted anew on the way in, so compare everything else.
        assert_eq!(back.as_radial().unwrap().center, d.center());
        assert_eq!(back.enabled(), d.enabled());
    }

    /// A defect at its defaults should write only what cannot be inferred:
    /// where it is, how big it is and how strong.
    #[test]
    fn a_plain_defect_writes_nothing_it_need_not() {
        let text = toml::to_string(&Defect::Radial(RadialDefect::default())).unwrap();
        for absent in ["kind", "id", "name", "enabled", "rotation"] {
            assert!(
                !text.contains(absent),
                "{absent} should not appear in:\n{text}"
            );
        }
        assert!(text.contains("center"), "{text}");
        assert!(text.contains("strength"), "{text}");
    }

    #[test]
    fn the_unusual_states_are_still_written() {
        let text = toml::to_string(&Defect::Radial(RadialDefect {
            enabled: false,
            rotation: 0.5,
            ..Default::default()
        }))
        .unwrap();
        assert!(text.contains("enabled = false"), "{text}");
        assert!(text.contains("rotation = 0.5"), "{text}");
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
        let back = toml::from_str::<Defect>(&text).unwrap();
        assert_eq!(back.as_radial().unwrap().strength, Rgb::new(0.2, 0.1, 0.05));
    }
}
