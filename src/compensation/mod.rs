//! Compensation mathematics.
//!
//! Everything in this module is pure computation over normalized coordinates.
//! It must never depend on Wayland, X11, GUI or any other platform code so that
//! the model stays deterministically testable.

pub mod defect;
pub mod mask;
pub mod radial;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use defect::{Composition, Defect, DefectKind, DefectModel};
pub use mask::{Mask, MaskParams, MaskQuality};
pub use radial::RadialDefect;

/// A per-channel triple: a defect's strength, a panel response, an attenuation.
///
/// Everything the compensation model computes is per channel, because a defect
/// is rarely neutral: a patch is usually not just dim but dim *and* tinted.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

/// Written as a bare number when the channels agree and as `[r, g, b]`
/// otherwise, so a neutral defect does not clutter a hand-edited profile.
impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.is_neutral() {
            self.r.serialize(serializer)
        } else {
            [self.r, self.g, self.b].serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Rgb;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a number or a list of three numbers")
            }

            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Rgb, E> {
                Ok(Rgb::splat(v as f32))
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Rgb, E> {
                Ok(Rgb::splat(v as f32))
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Rgb, E> {
                Ok(Rgb::splat(v as f32))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Rgb, A::Error> {
                let [r, g, b] = [0usize, 1, 2].map(|_| seq.next_element::<f32>());
                match (r?, g?, b?) {
                    (Some(r), Some(g), Some(b)) => Ok(Rgb::new(r, g, b)),
                    _ => Err(serde::de::Error::invalid_length(0, &"three channel values")),
                }
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

impl Rgb {
    pub const ZERO: Rgb = Rgb::splat(0.0);
    pub const ONE: Rgb = Rgb::splat(1.0);

    pub const fn new(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b }
    }

    pub const fn splat(v: f32) -> Self {
        Self { r: v, g: v, b: v }
    }

    /// True when all three channels agree, i.e. the value is a plain scalar.
    pub fn is_neutral(self) -> bool {
        self.r == self.g && self.g == self.b
    }

    pub fn min_channel(self) -> f32 {
        self.r.min(self.g).min(self.b)
    }

    pub fn max_channel(self) -> f32 {
        self.r.max(self.g).max(self.b)
    }

    /// Largest absolute channel, i.e. the strength of the value ignoring sign.
    pub fn max_abs(self) -> f32 {
        self.r.abs().max(self.g.abs()).max(self.b.abs())
    }

    pub fn map(self, f: impl Fn(f32) -> f32) -> Rgb {
        Rgb::new(f(self.r), f(self.g), f(self.b))
    }

    pub fn zip(self, other: Rgb, f: impl Fn(f32, f32) -> f32) -> Rgb {
        Rgb::new(f(self.r, other.r), f(self.g, other.g), f(self.b, other.b))
    }

    pub fn to_array(self) -> [f32; 3] {
        [self.r, self.g, self.b]
    }
}

impl From<f32> for Rgb {
    fn from(v: f32) -> Rgb {
        Rgb::splat(v)
    }
}

impl std::ops::Add for Rgb {
    type Output = Rgb;
    fn add(self, rhs: Rgb) -> Rgb {
        self.zip(rhs, |a, b| a + b)
    }
}

impl std::ops::Mul<f32> for Rgb {
    type Output = Rgb;
    fn mul(self, rhs: f32) -> Rgb {
        self.map(|v| v * rhs)
    }
}

/// A point or extent in normalized display space, `x, y ∈ [0, 1]`.
///
/// Storing everything normalized is what lets a profile survive resolution
/// changes: the geometry means the same thing on a 1080p and a 4K mode of the
/// same panel.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

/// Serialized as `[x, y]` so hand-edited profiles stay readable.
impl Serialize for Vec2 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        [self.x, self.y].serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Vec2 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let [x, y] = <[f32; 2]>::deserialize(deserializer)?;
        Ok(Vec2 { x, y })
    }
}

impl Vec2 {
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };

    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub const fn splat(v: f32) -> Self {
        Self { x: v, y: v }
    }

    pub fn length(self) -> f32 {
        self.x.hypot(self.y)
    }
}

impl From<[f32; 2]> for Vec2 {
    fn from([x, y]: [f32; 2]) -> Self {
        Self { x, y }
    }
}

impl From<Vec2> for [f32; 2] {
    fn from(v: Vec2) -> Self {
        [v.x, v.y]
    }
}

impl std::ops::Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl std::ops::Add for Vec2 {
    type Output = Vec2;
    fn add(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl std::ops::Mul<f32> for Vec2 {
    type Output = Vec2;
    fn mul(self, rhs: f32) -> Vec2 {
        Vec2::new(self.x * rhs, self.y * rhs)
    }
}

/// Linear interpolation, `t` unclamped.
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Holder {
        value: Rgb,
    }

    #[test]
    fn a_neutral_triple_is_written_as_one_number() {
        let text = toml::to_string(&Holder {
            value: Rgb::splat(0.11),
        })
        .unwrap();
        assert_eq!(text.trim(), "value = 0.11");
    }

    #[test]
    fn a_bare_number_parses_into_every_channel() {
        let holder: Holder = toml::from_str("value = 0.11").unwrap();
        assert_eq!(holder.value, Rgb::splat(0.11));
    }

    #[test]
    fn an_integer_parses_too() {
        let holder: Holder = toml::from_str("value = 1").unwrap();
        assert_eq!(holder.value, Rgb::ONE);
    }

    #[test]
    fn a_triple_round_trips_per_channel() {
        let holder = Holder {
            value: Rgb::new(0.2, 0.1, 0.05),
        };
        let text = toml::to_string(&holder).unwrap();
        assert_eq!(text.trim(), "value = [0.2, 0.1, 0.05]");
        assert_eq!(toml::from_str::<Holder>(&text).unwrap(), holder);
    }

    #[test]
    fn a_short_list_is_rejected() {
        assert!(toml::from_str::<Holder>("value = [0.2, 0.1]").is_err());
    }

    #[test]
    fn extremes_ignore_sign_where_asked() {
        let v = Rgb::new(-0.3, 0.1, 0.2);
        assert_eq!(v.min_channel(), -0.3);
        assert_eq!(v.max_channel(), 0.2);
        assert_eq!(v.max_abs(), 0.3);
    }
}
