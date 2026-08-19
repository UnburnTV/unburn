//! Turning a [`Mask`] into pixels.
//!
//! The overlay is a spatially varying alpha over a mostly black image, so the
//! whole renderer is a resample and a quantize. That is cheap enough on the CPU
//! and it keeps the program free of a GPU stack it does not need; the trait
//! exists so a `wgpu` backend can be dropped in without touching the platform
//! code.

use crate::compensation::{mask, Mask};

use super::{EditorView, ShowMode};

/// Presents a mask on one surface.
pub trait MaskRenderer {
    /// The surface changed size; the next render must cover the new extent.
    fn resize(&mut self, width: u32, height: u32);
    /// Replace the alpha field. Called only when the configuration changes.
    fn upload_mask(&mut self, mask: &Mask);
    /// Produce the pixels for the current mask.
    fn render(&mut self);
}

/// A straight-line colour with straight (non-premultiplied) alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    pub const SELECTED: Rgba = Rgba(80, 220, 255, 230);
    pub const UNSELECTED: Rgba = Rgba(255, 255, 255, 70);
    pub const DISABLED: Rgba = Rgba(160, 160, 160, 45);
    pub const MODEL_TINT: Rgba = Rgba(255, 90, 200, 255);
}

/// Software renderer producing a premultiplied ARGB8888 framebuffer.
///
/// The byte order is the little-endian layout both `wl_shm`'s `Argb8888` and an
/// X11 32-bit TrueColor visual expect: blue, green, red, alpha.
pub struct CpuMaskRenderer {
    width: u32,
    height: u32,
    dither: bool,
    mask: Mask,
    /// The modelled defect field, when the editor asks to see it.
    model: Option<Mask>,
    editor: Option<EditorView>,
    framebuffer: Vec<u8>,
    dirty: bool,
    generation: u64,
}

impl CpuMaskRenderer {
    pub fn new(width: u32, height: u32, dither: bool) -> Self {
        let mut renderer = Self {
            width: 0,
            height: 0,
            dither,
            mask: Mask::transparent(2, 2),
            model: None,
            editor: None,
            framebuffer: Vec::new(),
            dirty: true,
            generation: 0,
        };
        renderer.resize(width, height);
        renderer
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn set_dither(&mut self, dither: bool) {
        if self.dither != dither {
            self.dither = dither;
            self.dirty = true;
        }
    }

    /// Attach or clear the on-screen editing annotations.
    pub fn set_editor(&mut self, editor: Option<EditorView>) {
        if self.editor != editor {
            self.editor = editor;
            self.dirty = true;
        }
    }

    pub fn set_model(&mut self, model: Option<Mask>) {
        if self.model != model {
            self.model = model;
            self.dirty = true;
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Increments every time [`MaskRenderer::render`] produces new pixels, so
    /// backends can tell whether they still need to copy the framebuffer out.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer
    }

    /// Render if anything changed and hand back the pixels, or `None` if the
    /// previous frame is still current.
    pub fn frame(&mut self) -> Option<&[u8]> {
        if !self.dirty {
            return None;
        }
        self.render();
        Some(&self.framebuffer)
    }

    fn show_mode(&self) -> ShowMode {
        self.editor
            .as_ref()
            .map(|e| e.show)
            .unwrap_or(ShowMode::Correction)
    }

    fn draw_editor(&mut self) {
        let Some(editor) = self.editor.take() else {
            return;
        };

        for defect in &editor.defects {
            let selected = editor.selected == Some(defect.id);
            let colour = if !defect.enabled {
                Rgba::DISABLED
            } else if selected {
                Rgba::SELECTED
            } else {
                Rgba::UNSELECTED
            };

            self.stroke_ellipse(defect, 1.0, colour, if selected { 3 } else { 2 });
            if selected {
                // A faint outer ring marks where the Gaussian has essentially
                // died out, which is what the eye is actually matching.
                self.stroke_ellipse(defect, 2.0, Rgba(80, 220, 255, 70), 1);
                self.draw_crosshair(defect.center, 14, 3, colour);
                for handle in defect.handles() {
                    self.fill_square(handle, 5, colour);
                }
            } else {
                self.draw_crosshair(defect.center, 7, 1, colour);
            }
        }

        self.editor = Some(editor);
    }

    fn stroke_ellipse(
        &mut self,
        defect: &super::EditorDefect,
        scale: f32,
        colour: Rgba,
        thickness: i32,
    ) {
        let (sin, cos) = defect.rotation.sin_cos();
        let rx = defect.radius.x * scale * self.width as f32;
        let ry = defect.radius.y * scale * self.height as f32;
        let cx = defect.center.x * self.width as f32;
        let cy = defect.center.y * self.height as f32;

        // One sample per pixel of perimeter keeps the contour unbroken.
        let perimeter = std::f32::consts::PI * 2.0 * rx.abs().max(ry.abs());
        let steps = (perimeter.ceil() as i32).clamp(64, 8192);

        let mut previous: Option<(i32, i32)> = None;
        for i in 0..=steps {
            let t = (i as f32 / steps as f32) * std::f32::consts::TAU;
            // Rotation happens in pixel space so the ellipse is not sheared by
            // a non-square aspect ratio.
            let (st, ct) = t.sin_cos();
            let ux = ct * defect.radius.x * scale;
            let uy = st * defect.radius.y * scale;
            let x = cx + (ux * cos - uy * sin) * self.width as f32;
            let y = cy + (ux * sin + uy * cos) * self.height as f32;
            let point = (x.round() as i32, y.round() as i32);
            if let Some(previous) = previous {
                self.draw_line(previous, point, colour, thickness);
            }
            previous = Some(point);
        }
    }

    fn draw_crosshair(
        &mut self,
        center: crate::compensation::Vec2,
        arm: i32,
        thickness: i32,
        colour: Rgba,
    ) {
        let cx = (center.x * self.width as f32).round() as i32;
        let cy = (center.y * self.height as f32).round() as i32;
        self.draw_line((cx - arm, cy), (cx + arm, cy), colour, thickness);
        self.draw_line((cx, cy - arm), (cx, cy + arm), colour, thickness);
    }

    fn fill_square(&mut self, center: crate::compensation::Vec2, half: i32, colour: Rgba) {
        let cx = (center.x * self.width as f32).round() as i32;
        let cy = (center.y * self.height as f32).round() as i32;
        for y in (cy - half)..=(cy + half) {
            for x in (cx - half)..=(cx + half) {
                self.blend(x, y, colour);
            }
        }
    }

    fn draw_line(&mut self, from: (i32, i32), to: (i32, i32), colour: Rgba, thickness: i32) {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let steps = dx.abs().max(dy.abs()).max(1);
        let half = (thickness - 1) / 2;
        for i in 0..=steps {
            let x = from.0 + dx * i / steps;
            let y = from.1 + dy * i / steps;
            for oy in -half..=half {
                for ox in -half..=half {
                    self.blend(x + ox, y + oy, colour);
                }
            }
        }
    }

    /// Source-over one straight-alpha colour onto the premultiplied buffer.
    fn blend(&mut self, x: i32, y: i32, colour: Rgba) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let index = (y as usize * self.width as usize + x as usize) * 4;
        let sa = colour.3 as u32;
        if sa == 0 {
            return;
        }
        let inv = 255 - sa;
        // Premultiply the source, then the usual over operator.
        let src = [
            colour.2 as u32 * sa,
            colour.1 as u32 * sa,
            colour.0 as u32 * sa,
            sa * 255,
        ];
        for (out, src) in self.framebuffer[index..index + 4].iter_mut().zip(src) {
            let dst = *out as u32 * 255;
            *out = ((src + dst * inv / 255) / 255).min(255) as u8;
        }
    }

    fn tint_model(&mut self, model: &Mask) {
        let colour = Rgba::MODEL_TINT;
        for y in 0..self.height {
            let v = (y as f32 + 0.5) / self.height as f32;
            for x in 0..self.width {
                let u = (x as f32 + 0.5) / self.width as f32;
                let deviation = model.sample(crate::compensation::Vec2::new(u, v));
                if deviation <= 0.001 {
                    continue;
                }
                // Amplified so a 10 % defect is clearly visible rather than a
                // barely-there wash.
                let alpha = (deviation * 4.0).clamp(0.0, 0.85);
                self.blend(
                    x as i32,
                    y as i32,
                    Rgba(colour.0, colour.1, colour.2, (alpha * 255.0) as u8),
                );
            }
        }
    }
}

impl MaskRenderer for CpuMaskRenderer {
    fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if self.width == width && self.height == height {
            return;
        }
        self.width = width;
        self.height = height;
        self.framebuffer = vec![0u8; (width as usize) * (height as usize) * 4];
        self.dirty = true;
    }

    fn upload_mask(&mut self, mask: &Mask) {
        if self.mask != *mask {
            self.mask = mask.clone();
            self.dirty = true;
        }
    }

    fn render(&mut self) {
        let show = self.show_mode();

        if show.draws_correction() {
            let (w, h) = (self.width, self.height);
            let dither = self.dither;
            let mask = std::mem::replace(&mut self.mask, Mask::transparent(2, 2));
            mask::rasterize_argb8888(&mask, &mut self.framebuffer, w, h, dither);
            self.mask = mask;
        } else {
            self.framebuffer.fill(0);
        }

        if show.draws_model() {
            if let Some(model) = self.model.take() {
                self.tint_model(&model);
                self.model = Some(model);
            }
        }

        self.draw_editor();

        self.dirty = false;
        self.generation += 1;
    }
}

/// Fill a premultiplied ARGB8888 buffer with an opaque colour, for the
/// calibration patterns.
pub fn fill_opaque(buffer: &mut [u8], rgb: [u8; 3]) {
    for pixel in buffer.chunks_exact_mut(4) {
        pixel[0] = rgb[2];
        pixel[1] = rgb[1];
        pixel[2] = rgb[0];
        pixel[3] = 0xFF;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compensation::{mask::generate_at, Defect, MaskParams, RadialDefect, Rgb, Vec2};
    use crate::overlay::EditorDefect;
    use uuid::Uuid;

    fn alpha_at(renderer: &CpuMaskRenderer, x: u32, y: u32) -> u8 {
        renderer.framebuffer()[(y as usize * renderer.width() as usize + x as usize) * 4 + 3]
    }

    fn pixel(renderer: &CpuMaskRenderer, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * renderer.width() as usize + x as usize) * 4;
        renderer.framebuffer()[i..i + 4].try_into().unwrap()
    }

    fn spot_mask() -> Mask {
        spot_mask_with(Rgb::splat(0.15))
    }

    fn spot_mask_with(strength: Rgb) -> Mask {
        let defect = Defect::Radial(RadialDefect {
            center: Vec2::splat(0.5),
            radius: Vec2::splat(0.1),
            strength,
            ..Default::default()
        });
        let params = MaskParams {
            dither: false,
            ..Default::default()
        };
        generate_at(&[defect], &params, 33, 33)
    }

    #[test]
    fn a_fresh_renderer_is_fully_transparent() {
        let mut renderer = CpuMaskRenderer::new(16, 16, false);
        renderer.render();
        assert!(renderer.framebuffer().iter().all(|b| *b == 0));
    }

    #[test]
    fn rendering_a_mask_produces_black_with_alpha() {
        let mask = spot_mask();
        let mut renderer = CpuMaskRenderer::new(64, 64, false);
        renderer.upload_mask(&mask);
        renderer.render();

        assert_eq!(pixel(&renderer, 32, 32)[0..3], [0, 0, 0]);
        // The centre of the spot is the deepest point of the mask, and the
        // renderer's job is to reproduce it rather than to invent a value.
        let peak = (mask.peak_alpha() * 255.0).round() as i32;
        assert!(peak > 0, "the bright spot must be darkened");
        let got = alpha_at(&renderer, 32, 32) as i32;
        assert!((got - peak).abs() <= 1, "centre {got}, mask peak {peak}");
        assert!(
            alpha_at(&renderer, 0, 0) <= 2,
            "a healthy corner keeps all its light"
        );
    }

    #[test]
    fn a_tinted_defect_renders_a_tinted_surface() {
        let mut renderer = CpuMaskRenderer::new(64, 64, false);
        renderer.upload_mask(&spot_mask_with(Rgb::new(0.2, 0.0, 0.0)));
        renderer.render();

        // Little-endian ARGB8888: the red channel took all the attenuation, so
        // blue and green get light handed back to them.
        let [b, g, r, a] = pixel(&renderer, 32, 32);
        assert_eq!(r, 0);
        assert!(g > 0 && b > 0, "green and blue must be lifted back up");
        assert!(g.max(b) <= a, "premultiplied colour cannot exceed alpha");
    }

    #[test]
    fn resizing_reallocates_and_forces_a_redraw() {
        let mut renderer = CpuMaskRenderer::new(8, 8, false);
        renderer.render();
        assert!(!renderer.is_dirty());
        renderer.resize(32, 16);
        assert!(renderer.is_dirty());
        assert_eq!(renderer.framebuffer().len(), 32 * 16 * 4);
    }

    #[test]
    fn nothing_is_redrawn_when_nothing_changed() {
        let mut renderer = CpuMaskRenderer::new(16, 16, false);
        assert!(renderer.frame().is_some());
        assert!(renderer.frame().is_none());

        // Uploading an identical mask is not a change.
        let mask = spot_mask();
        renderer.upload_mask(&mask);
        assert!(renderer.frame().is_some());
        renderer.upload_mask(&mask);
        assert!(renderer.frame().is_none());
    }

    #[test]
    fn editor_outlines_are_drawn_over_the_mask() {
        let mut renderer = CpuMaskRenderer::new(64, 64, false);
        renderer.upload_mask(&spot_mask());
        let id = Uuid::new_v4();
        renderer.set_editor(Some(EditorView {
            defects: vec![EditorDefect {
                id,
                center: Vec2::splat(0.5),
                radius: Vec2::splat(0.1),
                rotation: 0.0,
                strength: Rgb::splat(0.15),
                enabled: true,
            }],
            selected: Some(id),
            show: ShowMode::Correction,
        }));
        renderer.render();

        // The crosshair sits on the defect centre, which the mask left black.
        let center = pixel(&renderer, 32, 32);
        assert!(
            center[0] > 0 || center[1] > 0 || center[2] > 0,
            "outline must be coloured"
        );
    }

    #[test]
    fn model_mode_hides_the_correction() {
        let model = Mask {
            width: 4,
            height: 4,
            texels: vec![[0.0, 0.0, 0.0, 0.2]; 16],
            min_gain: Rgb::splat(0.8),
            max_gain: Rgb::ONE,
            target: Rgb::splat(0.8),
        };
        let mut renderer = CpuMaskRenderer::new(16, 16, false);
        renderer.upload_mask(&spot_mask());
        renderer.set_model(Some(model));
        renderer.set_editor(Some(EditorView {
            defects: Vec::new(),
            selected: None,
            show: ShowMode::Model,
        }));
        renderer.render();

        // Tinted, not the black-with-alpha of the correction.
        let px = pixel(&renderer, 8, 8);
        assert!(px[2] > px[1], "the model tint is not black");
    }

    #[test]
    fn leaving_the_editor_restores_the_plain_correction() {
        let mut renderer = CpuMaskRenderer::new(64, 64, false);
        renderer.upload_mask(&spot_mask());
        renderer.render();
        let plain: Vec<u8> = renderer.framebuffer().to_vec();

        let id = Uuid::new_v4();
        renderer.set_editor(Some(EditorView {
            defects: vec![EditorDefect {
                id,
                center: Vec2::splat(0.5),
                radius: Vec2::splat(0.1),
                rotation: 0.0,
                strength: Rgb::splat(0.15),
                enabled: true,
            }],
            selected: Some(id),
            show: ShowMode::Correction,
        }));
        renderer.render();
        assert_ne!(renderer.framebuffer(), plain.as_slice());

        renderer.set_editor(None);
        renderer.render();
        assert_eq!(renderer.framebuffer(), plain.as_slice());
    }

    #[test]
    fn opaque_fill_writes_the_expected_byte_order() {
        let mut buffer = vec![0u8; 4];
        fill_opaque(&mut buffer, [10, 20, 30]);
        assert_eq!(buffer, vec![30, 20, 10, 255]);
    }
}
