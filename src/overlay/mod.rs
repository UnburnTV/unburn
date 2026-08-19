//! What an overlay surface should contain, independent of how it gets there.

pub mod renderer;
pub mod window;

use uuid::Uuid;

use crate::{
    compensation::{Defect, Rgb, Vec2},
    display::Transform,
};

pub use renderer::{CpuMaskRenderer, MaskRenderer};
pub use window::{transform_defect, OverlaySurface};

/// What the overlay draws while the user is editing on screen.
///
/// The compensation is not one of the choices. Regenerating the mask and
/// resampling it over the surface takes far longer than a drag or a wheel notch
/// leaves between events, so on-screen editing shows the geometry and leaves
/// the correction to reappear the moment editing ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShowMode {
    /// Just the outlines, over an otherwise untouched desktop.
    #[default]
    Outlines,
    /// The modelled defect field as well, so its shape can be compared with
    /// the blemish underneath it.
    Model,
}

impl ShowMode {
    pub const ALL: [ShowMode; 2] = [ShowMode::Outlines, ShowMode::Model];

    pub fn label(self) -> &'static str {
        match self {
            ShowMode::Outlines => "Show outlines",
            ShowMode::Model => "Show model",
        }
    }

    pub fn next(self) -> ShowMode {
        match self {
            ShowMode::Outlines => ShowMode::Model,
            ShowMode::Model => ShowMode::Outlines,
        }
    }

    pub fn draws_model(self) -> bool {
        matches!(self, ShowMode::Model)
    }
}

/// A colour the user can put on the rotating calibration disc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscSwatch {
    pub label: &'static str,
    pub rgb: [u8; 3],
}

impl DiscSwatch {
    pub const ALL: [DiscSwatch; 12] = [
        DiscSwatch {
            label: "Black",
            rgb: [0, 0, 0],
        },
        DiscSwatch {
            label: "Grey 20",
            rgb: [51, 51, 51],
        },
        DiscSwatch {
            label: "Grey 40",
            rgb: [102, 102, 102],
        },
        DiscSwatch {
            label: "Grey 60",
            rgb: [153, 153, 153],
        },
        DiscSwatch {
            label: "Grey 80",
            rgb: [204, 204, 204],
        },
        DiscSwatch {
            label: "White",
            rgb: [255, 255, 255],
        },
        DiscSwatch {
            label: "Red",
            rgb: [255, 0, 0],
        },
        DiscSwatch {
            label: "Green",
            rgb: [0, 255, 0],
        },
        DiscSwatch {
            label: "Blue",
            rgb: [0, 0, 255],
        },
        DiscSwatch {
            label: "Dark red",
            rgb: [128, 0, 0],
        },
        DiscSwatch {
            label: "Dark green",
            rgb: [0, 128, 0],
        },
        DiscSwatch {
            label: "Dark blue",
            rgb: [0, 0, 128],
        },
    ];

    /// Grey 40, a mid grey that shows both a bright blemish and the correction.
    pub fn default_colors() -> Vec<[u8; 3]> {
        vec![Self::ALL[2].rgb]
    }

    /// The selected swatches, still in palette order so the wedges stay put
    /// when the user ticks a box.
    pub fn selected(flags: &[bool]) -> Vec<[u8; 3]> {
        Self::ALL
            .iter()
            .zip(flags)
            .filter(|(_, on)| **on)
            .map(|(swatch, _)| swatch.rgb)
            .collect()
    }
}

/// A defect as the on-screen editor sees it: already mapped into the
/// surface's own coordinate space.
#[derive(Debug, Clone, PartialEq)]
pub struct EditorDefect {
    pub id: Uuid,
    pub center: Vec2,
    pub radius: Vec2,
    pub rotation: f32,
    pub strength: Rgb,
    pub enabled: bool,
}

impl EditorDefect {
    /// Project a stored defect into the coordinates of a possibly rotated
    /// surface.
    pub fn from_defect(defect: &Defect, transform: Transform) -> Option<EditorDefect> {
        let radial = defect.as_radial()?;
        let axis = Vec2::new(radial.rotation.cos(), radial.rotation.sin());
        let mapped = transform.direction_to_surface(axis);
        Some(EditorDefect {
            id: radial.id,
            center: transform.panel_to_surface(radial.center),
            radius: radial.radius,
            rotation: mapped.y.atan2(mapped.x),
            strength: radial.strength,
            enabled: radial.enabled,
        })
    }

    /// Normalized elliptical distance of `uv` from the centre, `1.0` on the
    /// nominal contour.
    pub fn distance(&self, uv: Vec2) -> f32 {
        let d = uv - self.center;
        let (sin, cos) = self.rotation.sin_cos();
        let x = (d.x * cos + d.y * sin) / self.radius.x.max(1e-4);
        let y = (-d.x * sin + d.y * cos) / self.radius.y.max(1e-4);
        (x * x + y * y).sqrt()
    }

    /// Positions of the width and height drag handles, in surface coordinates.
    pub fn handles(&self) -> [Vec2; 4] {
        let (sin, cos) = self.rotation.sin_cos();
        let along = Vec2::new(cos * self.radius.x, sin * self.radius.x);
        let across = Vec2::new(-sin * self.radius.y, cos * self.radius.y);
        [
            self.center + along,
            self.center - along,
            self.center + across,
            self.center - across,
        ]
    }
}

/// The rotating calibration disc shown while a spot's Edit panel is open.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationDisc {
    pub defect: EditorDefect,
    /// Opaque sRGB wedges, split equally around the disc. Empty means no fill.
    pub colors: Vec<[u8; 3]>,
}

/// Which part of a defect the pointer has grabbed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grab {
    Center,
    Width,
    Height,
}

/// The editor's view of one surface.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EditorView {
    pub defects: Vec<EditorDefect>,
    pub selected: Option<Uuid>,
    pub show: ShowMode,
}

impl EditorView {
    pub fn selected_defect(&self) -> Option<&EditorDefect> {
        let id = self.selected?;
        self.defects.iter().find(|d| d.id == id)
    }

    /// Find what the pointer is over, preferring the current selection so a
    /// stack of overlapping defects stays workable.
    pub fn hit_test(&self, uv: Vec2, tolerance: f32) -> Option<(Uuid, Grab)> {
        let mut ordered: Vec<&EditorDefect> = self.defects.iter().collect();
        ordered.sort_by_key(|d| Some(d.id) != self.selected);

        for defect in ordered {
            for (index, handle) in defect.handles().iter().enumerate() {
                if (*handle - uv).length() <= tolerance {
                    let grab = if index < 2 { Grab::Width } else { Grab::Height };
                    return Some((defect.id, grab));
                }
            }
            if defect.distance(uv) <= 1.0 {
                return Some((defect.id, Grab::Center));
            }
        }
        None
    }
}

/// A fullscreen calibration pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestPattern {
    /// Uniform grey at the given percentage of full code value.
    Grey(u8),
    Red,
    Green,
    Blue,
}

impl TestPattern {
    /// The patterns offered in the GUI, in the order the arrow keys walk them.
    pub const ALL: [TestPattern; 10] = [
        TestPattern::Grey(0),
        TestPattern::Grey(5),
        TestPattern::Grey(10),
        TestPattern::Grey(25),
        TestPattern::Grey(50),
        TestPattern::Grey(75),
        TestPattern::Grey(100),
        TestPattern::Red,
        TestPattern::Green,
        TestPattern::Blue,
    ];

    /// Just the greys, for the cycling mode.
    pub const GREYS: [TestPattern; 7] = [
        TestPattern::Grey(0),
        TestPattern::Grey(5),
        TestPattern::Grey(10),
        TestPattern::Grey(25),
        TestPattern::Grey(50),
        TestPattern::Grey(75),
        TestPattern::Grey(100),
    ];

    pub fn label(self) -> String {
        match self {
            TestPattern::Grey(0) => "Black".into(),
            TestPattern::Grey(100) => "100% white".into(),
            TestPattern::Grey(p) => format!("{p}% gray"),
            TestPattern::Red => "Red".into(),
            TestPattern::Green => "Green".into(),
            TestPattern::Blue => "Blue".into(),
        }
    }

    /// Opaque sRGB code values. Percentages are of the encoded value, which is
    /// what "50 % gray" conventionally means on a calibration pattern.
    pub fn rgb(self) -> [u8; 3] {
        match self {
            TestPattern::Grey(p) => {
                let v = ((p.min(100) as f32 / 100.0) * 255.0).round() as u8;
                [v, v, v]
            }
            TestPattern::Red => [255, 0, 0],
            TestPattern::Green => [0, 255, 0],
            TestPattern::Blue => [0, 0, 255],
        }
    }

    /// Parse the `--test-pattern` argument.
    pub fn parse(text: &str) -> Option<TestPattern> {
        let text = text.trim().trim_end_matches('%');
        match text.to_ascii_lowercase().as_str() {
            "black" => Some(TestPattern::Grey(0)),
            "white" => Some(TestPattern::Grey(100)),
            "red" => Some(TestPattern::Red),
            "green" => Some(TestPattern::Green),
            "blue" => Some(TestPattern::Blue),
            "grey" | "gray" => Some(TestPattern::Grey(50)),
            other => other
                .parse::<u8>()
                .ok()
                .map(|p| TestPattern::Grey(p.min(100))),
        }
    }

    pub fn step(self, delta: i32) -> TestPattern {
        let index = TestPattern::ALL
            .iter()
            .position(|p| *p == self)
            .unwrap_or(0) as i32;
        let count = TestPattern::ALL.len() as i32;
        TestPattern::ALL[(index + delta).rem_euclid(count) as usize]
    }
}

/// The test pattern surface's full state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TestPatternState {
    pub pattern: TestPattern,
    /// Whether the compensation overlay stays on top of the pattern.
    pub compensated: bool,
    /// Walk the grey ramp automatically.
    pub cycling: bool,
}

impl Default for TestPatternState {
    fn default() -> Self {
        Self {
            pattern: TestPattern::Grey(50),
            compensated: true,
            cycling: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compensation::RadialDefect;

    fn defect(center: Vec2, radius: Vec2) -> EditorDefect {
        EditorDefect {
            id: Uuid::new_v4(),
            center,
            radius,
            rotation: 0.0,
            strength: Rgb::splat(0.1),
            enabled: true,
        }
    }

    #[test]
    fn disc_swatches_keep_palette_order_when_several_are_ticked() {
        let mut flags = [false; DiscSwatch::ALL.len()];
        flags[8] = true; // Blue
        flags[6] = true; // Red
        flags[7] = true; // Green
        assert_eq!(
            DiscSwatch::selected(&flags),
            vec![[255, 0, 0], [0, 255, 0], [0, 0, 255]]
        );
    }

    #[test]
    fn test_pattern_parsing_accepts_what_the_help_promises() {
        assert_eq!(TestPattern::parse("50"), Some(TestPattern::Grey(50)));
        assert_eq!(TestPattern::parse("50%"), Some(TestPattern::Grey(50)));
        assert_eq!(TestPattern::parse("Black"), Some(TestPattern::Grey(0)));
        assert_eq!(TestPattern::parse("blue"), Some(TestPattern::Blue));
        assert_eq!(TestPattern::parse("nonsense"), None);
    }

    #[test]
    fn arrow_keys_wrap_around_the_pattern_list() {
        assert_eq!(TestPattern::Grey(0).step(-1), TestPattern::Blue);
        assert_eq!(TestPattern::Blue.step(1), TestPattern::Grey(0));
        assert_eq!(TestPattern::Grey(50).step(1), TestPattern::Grey(75));
    }

    #[test]
    fn hit_test_finds_the_body_and_the_handles() {
        let d = defect(Vec2::new(0.5, 0.5), Vec2::new(0.1, 0.1));
        let id = d.id;
        let view = EditorView {
            defects: vec![d],
            selected: Some(id),
            ..Default::default()
        };

        assert_eq!(
            view.hit_test(Vec2::new(0.5, 0.5), 0.01),
            Some((id, Grab::Center))
        );
        assert_eq!(
            view.hit_test(Vec2::new(0.6, 0.5), 0.02),
            Some((id, Grab::Width))
        );
        assert_eq!(
            view.hit_test(Vec2::new(0.5, 0.6), 0.02),
            Some((id, Grab::Height))
        );
        assert_eq!(view.hit_test(Vec2::new(0.9, 0.9), 0.01), None);
    }

    #[test]
    fn hit_test_prefers_the_selected_defect_when_they_overlap() {
        let a = defect(Vec2::new(0.5, 0.5), Vec2::new(0.2, 0.2));
        let b = defect(Vec2::new(0.5, 0.5), Vec2::new(0.2, 0.2));
        let (a_id, b_id) = (a.id, b.id);
        let view = EditorView {
            defects: vec![a, b],
            selected: Some(b_id),
            ..Default::default()
        };
        assert_eq!(view.hit_test(Vec2::new(0.5, 0.5), 0.01).unwrap().0, b_id);
        assert_ne!(view.hit_test(Vec2::new(0.5, 0.5), 0.01).unwrap().0, a_id);
    }

    #[test]
    fn editor_defects_follow_a_rotated_output() {
        let defect = Defect::Radial(RadialDefect {
            center: Vec2::new(0.25, 0.5),
            radius: Vec2::new(0.1, 0.05),
            rotation: 0.0,
            ..Default::default()
        });
        let view = EditorDefect::from_defect(&defect, Transform::Rotate90).unwrap();
        // The panel's left edge becomes the surface's bottom edge.
        assert!((view.center.x - 0.5).abs() < 1e-5);
        assert!((view.center.y - 0.75).abs() < 1e-5);
        // The long axis turns with it.
        assert!((view.rotation.abs() - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
    }
}
