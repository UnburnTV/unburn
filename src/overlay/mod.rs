//! What an overlay surface should contain, independent of how it gets there.

pub mod renderer;
pub mod window;

use uuid::Uuid;

use crate::{
    compensation::{ellipse::to_square, Defect, Ellipse, Vec2},
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

/// Express a panel-space ellipse in the coordinates of a rotated surface.
///
/// Turning a screen is a rigid motion of its pixels, so on the glass nothing
/// happens to the shape at all. In normalized terms it is not so quiet: a
/// transform that swaps the axes leaves the surface with the reciprocal of the
/// panel's aspect ratio, so both radii and the angle have to be restated
/// against it.
pub fn ellipse_to_surface(ellipse: Ellipse, transform: Transform) -> Ellipse {
    if transform == Transform::Normal {
        return ellipse;
    }
    let aspect = if transform.swaps_axes() {
        1.0 / ellipse.aspect
    } else {
        ellipse.aspect
    };
    let (along, across) = ellipse.axes();
    // In square space the transform is a turn (possibly a reflection), so the
    // axes keep their lengths and the first of them still gives the angle.
    let along = to_square(transform.direction_to_surface(along), aspect);
    let across = to_square(transform.direction_to_surface(across), aspect);
    Ellipse::new(
        transform.panel_to_surface(ellipse.center),
        Vec2::new(along.length() / aspect, across.length()),
        along.y.atan2(along.x),
        aspect,
    )
}

/// A defect as the on-screen editor sees it: already mapped into the
/// surface's own coordinate space.
#[derive(Debug, Clone, PartialEq)]
pub struct EditorDefect {
    pub id: Uuid,
    pub center: Vec2,
    pub radius: Vec2,
    pub rotation: f32,
    /// Width over height of the surface, so that [`Self::rotation`] means the
    /// same turn on the glass here as it does in the stored profile.
    pub aspect: f32,
    pub enabled: bool,
}

impl EditorDefect {
    /// Half-extent of a resize handle, in overlay pixels. Drawing and
    /// hit-testing share this so the square the pointer grabs is the square
    /// that is drawn.
    pub const HANDLE_HALF_PX: i32 = 5;

    /// Gap between a spot's contour and its rotation handle, in overlay
    /// pixels. Constant on screen, so the handle is reachable on a spot of any
    /// size instead of sitting inside the outline of a small one.
    pub const ROTATION_ARM_PX: f32 = 22.0;

    /// Project a stored defect into the coordinates of a possibly rotated
    /// surface. `panel_aspect` is the panel's width over its height, unrotated.
    pub fn from_defect(
        defect: &Defect,
        transform: Transform,
        panel_aspect: f32,
    ) -> Option<EditorDefect> {
        let radial = defect.as_radial()?;
        let shape = ellipse_to_surface(radial.ellipse(panel_aspect), transform);
        Some(EditorDefect {
            id: radial.id,
            center: shape.center,
            radius: shape.radius,
            rotation: shape.rotation,
            aspect: shape.aspect,
            enabled: radial.enabled,
        })
    }

    /// The shape this defect draws on the surface.
    pub fn ellipse(&self) -> Ellipse {
        Ellipse::new(self.center, self.radius, self.rotation, self.aspect)
    }

    /// Normalized elliptical distance of `uv` from the centre, `1.0` on the
    /// nominal contour.
    pub fn distance(&self, uv: Vec2) -> f32 {
        self.ellipse().distance(uv)
    }

    /// Positions of the width and height drag handles, in surface coordinates.
    pub fn handles(&self) -> [Vec2; 4] {
        let (along, across) = self.ellipse().axes();
        [
            self.center + along,
            self.center - along,
            self.center + across,
            self.center - across,
        ]
    }

    /// Where the rotation handle sits: on the width axis, a fixed distance
    /// beyond the contour. `height` is the surface's height in pixels, which is
    /// what one unit of the isotropic space measures.
    pub fn rotation_handle(&self, height: u32) -> Vec2 {
        let (along, _) = self.ellipse().axes();
        let arm = to_square(along, self.aspect).length();
        let reach = Self::ROTATION_ARM_PX / height.max(1) as f32;
        self.center + along * ((arm + reach) / arm.max(1e-6))
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
    Rotate,
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
    ///
    /// Resize handles are tested as axis-aligned pixel squares of
    /// [`EditorDefect::HANDLE_HALF_PX`], matching what the overlay draws.
    /// Only the selected defect has handles.
    pub fn hit_test(&self, uv: Vec2, width: u32, height: u32) -> Option<(Uuid, Grab)> {
        let mut ordered: Vec<&EditorDefect> = self.defects.iter().collect();
        ordered.sort_by_key(|d| Some(d.id) != self.selected);

        for defect in ordered {
            if self.selected == Some(defect.id) {
                // The rotation handle is outside the contour, so it is tested
                // first: a press there is never meant for the body.
                if handle_contains(defect.rotation_handle(height), uv, width, height) {
                    return Some((defect.id, Grab::Rotate));
                }
                for (index, handle) in defect.handles().iter().enumerate() {
                    if handle_contains(*handle, uv, width, height) {
                        let grab = if index < 2 { Grab::Width } else { Grab::Height };
                        return Some((defect.id, grab));
                    }
                }
            }
            if defect.distance(uv) <= 1.0 {
                return Some((defect.id, Grab::Center));
            }
        }
        None
    }

    /// The selected handle under the pointer, if any, as a surface UV.
    pub fn hovered_handle(&self, uv: Vec2, width: u32, height: u32) -> Option<Vec2> {
        let defect = self.selected_defect()?;
        defect
            .handles()
            .into_iter()
            .chain([defect.rotation_handle(height)])
            .find(|handle| handle_contains(*handle, uv, width, height))
    }
}

/// Overlay pixel of a surface UV, using the same rounding as the renderer.
pub fn overlay_pixel(uv: Vec2, width: u32, height: u32) -> (i32, i32) {
    (
        (uv.x * width as f32).round() as i32,
        (uv.y * height as f32).round() as i32,
    )
}

fn handle_contains(handle: Vec2, uv: Vec2, width: u32, height: u32) -> bool {
    let (hx, hy) = overlay_pixel(handle, width, height);
    let (px, py) = overlay_pixel(uv, width, height);
    let half = EditorDefect::HANDLE_HALF_PX;
    (px - hx).abs() <= half && (py - hy).abs() <= half
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
            aspect: 1.0,
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
            view.hit_test(Vec2::new(0.5, 0.5), 1000, 1000),
            Some((id, Grab::Center))
        );
        assert_eq!(
            view.hit_test(Vec2::new(0.6, 0.5), 1000, 1000),
            Some((id, Grab::Width))
        );
        assert_eq!(
            view.hit_test(Vec2::new(0.5, 0.6), 1000, 1000),
            Some((id, Grab::Height))
        );
        assert_eq!(view.hit_test(Vec2::new(0.9, 0.9), 1000, 1000), None);
    }

    #[test]
    fn handle_hitboxes_match_the_drawn_squares() {
        let d = defect(Vec2::new(0.5, 0.5), Vec2::new(0.1, 0.1));
        let id = d.id;
        let view = EditorView {
            defects: vec![d],
            selected: Some(id),
            ..Default::default()
        };
        let (w, h) = (1000u32, 1000u32);
        let half = EditorDefect::HANDLE_HALF_PX;
        // Right-hand width handle sits at (600, 500).
        let on_edge = Vec2::new((600 - half) as f32 / 1000.0, 0.5);
        let past_edge = Vec2::new((600 - half - 1) as f32 / 1000.0, 0.5);
        assert_eq!(view.hit_test(on_edge, w, h), Some((id, Grab::Width)));
        // Six pixels inward is still well inside the old 0.012 UV circle, but
        // it is outside the drawn square, so the body takes the press.
        assert_eq!(view.hit_test(past_edge, w, h), Some((id, Grab::Center)));
        let diagonal = Vec2::new(608.0 / 1000.0, 508.0 / 1000.0);
        assert_eq!(
            view.hit_test(diagonal, w, h),
            None,
            "a square must not keep the old circular slack"
        );
    }

    #[test]
    fn unselected_spots_have_no_handle_hitboxes() {
        let d = defect(Vec2::new(0.5, 0.5), Vec2::new(0.1, 0.1));
        let id = d.id;
        let view = EditorView {
            defects: vec![d],
            selected: None,
            ..Default::default()
        };
        // Handles are only drawn on the selection, so they must not steal a
        // press on an unselected contour.
        let hit = view.hit_test(Vec2::new(0.6, 0.5), 1000, 1000);
        assert_ne!(hit, Some((id, Grab::Width)));
        assert_ne!(hit, Some((id, Grab::Height)));
        assert_eq!(
            view.hit_test(Vec2::new(0.5, 0.5), 1000, 1000),
            Some((id, Grab::Center))
        );
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
        assert_eq!(
            view.hit_test(Vec2::new(0.5, 0.5), 1000, 1000).unwrap().0,
            b_id
        );
        assert_ne!(
            view.hit_test(Vec2::new(0.5, 0.5), 1000, 1000).unwrap().0,
            a_id
        );
    }

    #[test]
    fn editor_defects_follow_a_rotated_output() {
        let aspect = 16.0 / 9.0;
        let defect = Defect::Radial(RadialDefect {
            center: Vec2::new(0.25, 0.5),
            radius: Vec2::new(0.1, 0.05),
            rotation: 0.0,
            ..Default::default()
        });
        let view = EditorDefect::from_defect(&defect, Transform::Rotate90, aspect).unwrap();
        // The panel's left edge becomes the surface's bottom edge.
        assert!((view.center.x - 0.5).abs() < 1e-5);
        assert!((view.center.y - 0.75).abs() < 1e-5);
        // The long axis turns with it.
        assert!((view.rotation.abs() - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        // The surface is as tall as the panel is wide.
        assert!((view.aspect - 9.0 / 16.0).abs() < 1e-5);
    }

    /// A turn of the screen must not change how big the spot is on the glass,
    /// only which way round the numbers describing it are.
    #[test]
    fn a_rotated_output_keeps_a_spot_the_same_size_in_pixels() {
        let (panel_w, panel_h) = (1920.0f32, 1080.0f32);
        let defect = Defect::Radial(RadialDefect {
            center: Vec2::new(0.25, 0.5),
            radius: Vec2::new(0.1, 0.05),
            rotation: 0.0,
            ..Default::default()
        });
        let view =
            EditorDefect::from_defect(&defect, Transform::Rotate90, panel_w / panel_h).unwrap();
        // The surface is the panel stood on its side.
        let (surface_w, surface_h) = (panel_h, panel_w);
        let (along, across) = view.ellipse().axes();
        let pixels = |v: Vec2| Vec2::new(v.x * surface_w, v.y * surface_h);
        let (along, across) = (pixels(along), pixels(across));

        // The 192 px axis now runs up the surface and the 54 px one across it,
        // both still the length they were on the panel.
        assert!(along.x.abs() < 0.5, "{along:?}");
        assert!((along.y.abs() - 0.1 * panel_w).abs() < 0.5, "{along:?}");
        assert!(across.y.abs() < 0.5, "{across:?}");
        assert!((across.x.abs() - 0.05 * panel_h).abs() < 0.5, "{across:?}");
    }

    #[test]
    fn the_rotation_handle_sits_a_fixed_reach_beyond_the_contour() {
        let mut spot = defect(Vec2::splat(0.5), Vec2::splat(0.1));
        spot.aspect = 1.0;
        let handle = spot.rotation_handle(1000);
        // 0.1 of a 1000 px surface plus the arm.
        let expected = 0.5 + (100.0 + EditorDefect::ROTATION_ARM_PX) / 1000.0;
        assert!((handle.x - expected).abs() < 1e-5, "{handle:?}");
        assert!((handle.y - 0.5).abs() < 1e-6);

        // A tiny spot still gets a reachable handle.
        let small = defect(Vec2::splat(0.5), Vec2::splat(0.002));
        let handle = small.rotation_handle(1000);
        assert!(handle.x - small.center.x > 0.02, "{handle:?}");
    }

    #[test]
    fn the_rotation_handle_can_be_grabbed_and_the_body_cannot_steal_it() {
        let spot = defect(Vec2::splat(0.5), Vec2::splat(0.1));
        let id = spot.id;
        let handle = spot.rotation_handle(1000);
        let view = EditorView {
            defects: vec![spot],
            selected: Some(id),
            ..Default::default()
        };
        assert_eq!(view.hit_test(handle, 1000, 1000), Some((id, Grab::Rotate)));
        assert_eq!(view.hovered_handle(handle, 1000, 1000), Some(handle));
        // Still the width handle where the width handle is.
        assert_eq!(
            view.hit_test(Vec2::new(0.6, 0.5), 1000, 1000),
            Some((id, Grab::Width))
        );
    }
}
