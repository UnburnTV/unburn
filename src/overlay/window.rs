//! Per-surface bookkeeping shared by the Wayland and X11 backends.
//!
//! Both platforms need the same thing: hold the current alpha field, notice
//! when it actually changed, and produce pixels only then. What differs is how
//! those bytes reach the screen.

use crate::{
    compensation::{Defect, Mask, Vec2},
    display::Transform,
};

use super::{renderer::MaskRenderer, CpuMaskRenderer, EditorView};

/// One overlay surface's contents, independent of the windowing system.
pub struct OverlaySurface {
    renderer: CpuMaskRenderer,
    visible: bool,
    interactive: bool,
}

impl OverlaySurface {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            renderer: CpuMaskRenderer::new(width, height, true),
            visible: true,
            interactive: false,
        }
    }

    pub fn width(&self) -> u32 {
        self.renderer.width()
    }

    pub fn height(&self) -> u32 {
        self.renderer.height()
    }

    pub fn set_size(&mut self, width: u32, height: u32) {
        self.renderer.resize(width, height);
    }

    pub fn set_mask(&mut self, mask: &Mask) {
        self.renderer.upload_mask(mask);
    }

    pub fn set_model(&mut self, model: Option<Mask>) {
        self.renderer.set_model(model);
    }

    pub fn set_editor(&mut self, editor: Option<EditorView>) {
        self.renderer.set_editor(editor);
    }

    pub fn set_dither(&mut self, dither: bool) {
        self.renderer.set_dither(dither);
    }

    /// Bypass without touching the mask: nothing is recomputed, the pixels
    /// simply stop being presented.
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn set_interactive(&mut self, interactive: bool) {
        self.interactive = interactive;
    }

    pub fn is_interactive(&self) -> bool {
        self.interactive
    }

    /// The pixels, but only when they differ from what the caller last saw.
    pub fn frame(&mut self) -> Option<&[u8]> {
        self.renderer.frame()
    }

    /// The most recently rendered pixels, whether or not they are new.
    pub fn pixels(&self) -> &[u8] {
        self.renderer.framebuffer()
    }
}

/// Express a panel-space defect in the coordinates of a rotated surface.
pub fn transform_defect(defect: &Defect, transform: Transform) -> Defect {
    if transform == Transform::Normal {
        return defect.clone();
    }
    match defect {
        Defect::Radial(radial) => {
            let mut moved = radial.clone();
            moved.center = transform.panel_to_surface(radial.center);
            let axis = Vec2::new(radial.rotation.cos(), radial.rotation.sin());
            let mapped = transform.direction_to_surface(axis);
            moved.rotation = mapped.y.atan2(mapped.x);
            Defect::Radial(moved)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compensation::{mask::generate_at, MaskParams, RadialDefect, Rgb};

    fn spot() -> Defect {
        Defect::Radial(RadialDefect {
            center: Vec2::new(0.25, 0.5),
            radius: Vec2::new(0.1, 0.05),
            strength: Rgb::splat(0.15),
            ..Default::default()
        })
    }

    fn mask_of(defect: &Defect, width: u32, height: u32) -> Mask {
        generate_at(
            std::slice::from_ref(defect),
            &MaskParams::default(),
            width,
            height,
        )
    }

    #[test]
    fn the_first_frame_is_produced_and_the_second_is_not() {
        let mut surface = OverlaySurface::new(64, 64);
        surface.set_mask(&mask_of(&spot(), 32, 32));
        assert!(surface.frame().is_some());
        assert!(surface.frame().is_none());
    }

    #[test]
    fn a_new_mask_produces_a_new_frame() {
        let mut surface = OverlaySurface::new(64, 64);
        surface.set_mask(&mask_of(&spot(), 32, 32));
        surface.frame();

        let mut moved = spot();
        moved.set_center(Vec2::new(0.75, 0.5));
        surface.set_mask(&mask_of(&moved, 32, 32));
        assert!(surface.frame().is_some());
    }

    #[test]
    fn bypass_does_not_invalidate_the_pixels() {
        let mut surface = OverlaySurface::new(64, 64);
        surface.set_mask(&mask_of(&spot(), 32, 32));
        surface.frame();

        surface.set_visible(false);
        assert!(!surface.is_visible());
        assert!(surface.frame().is_none(), "bypass must not force a redraw");

        surface.set_visible(true);
        assert!(surface.frame().is_none());
    }

    #[test]
    fn resizing_produces_a_frame_at_the_new_size() {
        let mut surface = OverlaySurface::new(64, 64);
        surface.set_mask(&mask_of(&spot(), 32, 32));
        surface.frame();

        surface.set_size(128, 96);
        let frame = surface.frame().expect("a resize must produce a frame");
        assert_eq!(frame.len(), 128 * 96 * 4);
    }

    #[test]
    fn a_rotated_surface_moves_the_defect_with_the_panel() {
        let rotated = transform_defect(&spot(), Transform::Rotate90);
        let radial = rotated.as_radial().unwrap();
        assert!((radial.center.x - 0.5).abs() < 1e-5);
        assert!((radial.center.y - 0.75).abs() < 1e-5);
    }

    #[test]
    fn an_unrotated_surface_leaves_the_defect_alone() {
        let defect = spot();
        assert_eq!(transform_defect(&defect, Transform::Normal), defect);
    }
}
