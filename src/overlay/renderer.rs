//! Turning a [`Mask`] into pixels.
//!
//! The overlay is a spatially varying alpha over a mostly black image, so the
//! whole renderer is a resample and a quantize. That is cheap enough on the CPU
//! and it keeps the program free of a GPU stack it does not need; the trait
//! exists so a `wgpu` backend can be dropped in without touching the platform
//! code.

use std::time::Instant;

use crate::compensation::{mask, Mask, Vec2};

use super::{overlay_pixel, CalibrationDisc, EditorDefect, EditorView};

/// Process-local clock so the disc's angle does not lose precision the way a
/// unix timestamp converted to `f32` would.
fn edit_disc_origin() -> Instant {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

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
    pub const HANDLE_HOVER: Rgba = Rgba(255, 255, 255, 255);
    pub const UNSELECTED: Rgba = Rgba(255, 255, 255, 70);
    pub const DISABLED: Rgba = Rgba(160, 160, 160, 45);
    pub const MODEL_TINT: Rgba = Rgba(255, 90, 200, 255);
    pub const LOCATOR: Rgba = Rgba(255, 32, 32, 240);
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
    /// Spot whose Edit panel is open: the rotating calibration disc.
    disc: Option<CalibrationDisc>,
    /// Centre of the list-hover locator, in surface UV. Independent of the
    /// editor so pointing at a row does not resample the compensation.
    hover: Option<Vec2>,
    /// Resize handle currently under the pointer, in surface UV.
    handle_hover: Option<Vec2>,
    /// Pixels under the last locator, so it can be lifted in place.
    locator_restore: Option<SavedRect>,
    framebuffer: Vec<u8>,
    dirty: bool,
    /// The locator moved; composite it without a full redraw.
    hover_dirty: bool,
    generation: u64,
}

/// A rectangle copied out of the framebuffer so a later overlay can be undone.
struct SavedRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
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
            disc: None,
            hover: None,
            handle_hover: None,
            locator_restore: None,
            framebuffer: Vec::new(),
            dirty: true,
            hover_dirty: false,
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

    /// Attach or clear the rotating calibration disc shown while a spot's
    /// Edit panel is open.
    pub fn set_disc(&mut self, disc: Option<CalibrationDisc>) {
        if self.disc != disc {
            self.disc = disc;
            self.dirty = true;
        }
    }

    /// Attach or clear the on-screen editing annotations.
    pub fn set_editor(&mut self, editor: Option<EditorView>) {
        if editor.is_none() {
            self.handle_hover = None;
        }
        if self.editor != editor {
            self.editor = editor;
            self.dirty = true;
        }
    }

    /// Highlight the resize handle under the pointer. Drawn at the same size
    /// as the rest; only the fill changes.
    pub fn set_handle_hover(&mut self, center: Option<Vec2>) {
        if self.handle_hover != center {
            self.handle_hover = center;
            if self.editor.is_some() {
                self.dirty = true;
            }
        }
    }

    /// Mark a spot with a red cross without resampling the mask. The pixels
    /// under the last cross are put back first, so hovering the list is a
    /// few dozen pixels rather than a full overlay redraw.
    pub fn set_hover(&mut self, center: Option<Vec2>) {
        if self.hover != center {
            self.hover = center;
            self.hover_dirty = true;
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
        if self.dirty {
            self.render();
            return Some(&self.framebuffer);
        }
        if self.hover_dirty {
            self.composite_locator(true);
            self.hover_dirty = false;
            self.generation += 1;
            return Some(&self.framebuffer);
        }
        None
    }

    /// Opaque rotating disc behind the selected spot. Returns whether the
    /// disc is spinning, so the caller can keep presenting frames.
    fn draw_edit_pattern(&mut self) -> bool {
        let Some(disc) = self.disc.clone() else {
            return false;
        };
        if disc.colors.is_empty() {
            return false;
        }

        let cx = disc.defect.center.x * self.width as f32;
        let cy = disc.defect.center.y * self.height as f32;
        let rx = disc.defect.radius.x * self.width as f32;
        let ry = disc.defect.radius.y * self.height as f32;
        let radius = edit_disc_radius_px(rx, ry);
        let angle = edit_disc_angle();
        let (sin, cos) = angle.sin_cos();

        let min_x = ((cx - radius).floor() as i32).max(0);
        let max_x = ((cx + radius).ceil() as i32).min(self.width as i32 - 1);
        let min_y = ((cy - radius).floor() as i32).max(0);
        let max_y = ((cy + radius).ceil() as i32).min(self.height as i32 - 1);

        let apply_correction = disc.defect.enabled;
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                let coverage = (radius - dist + 0.5).clamp(0.0, 1.0);
                if coverage <= 0.0 {
                    continue;
                }
                let u = dx * cos + dy * sin;
                let v = -dx * sin + dy * cos;
                let rgb = editor_disc_color(u, v, &disc.colors);
                self.bake_pattern_pixel(x, y, rgb, coverage, apply_correction);
            }
        }
        disc.colors.len() > 1
    }

    /// Composite the overlay (already in the framebuffer) over an opaque
    /// pattern colour, then write the result back as an opaque pixel so the
    /// disc hides the desktop and the correction still applies.
    ///
    /// When `apply_correction` is false the pattern is written as-is: the spot
    /// is unchecked, so Edit should keep the disc without the compensation.
    fn bake_pattern_pixel(
        &mut self,
        x: i32,
        y: i32,
        rgb: [u8; 3],
        coverage: f32,
        apply_correction: bool,
    ) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let index = (y as usize * self.width as usize + x as usize) * 4;
        let ob = self.framebuffer[index] as u32;
        let og = self.framebuffer[index + 1] as u32;
        let or_ = self.framebuffer[index + 2] as u32;
        let oa = self.framebuffer[index + 3] as u32;
        let (mut b, mut g, mut r, mut a) = if apply_correction {
            let inv = 255 - oa;
            (
                ob + rgb[2] as u32 * inv / 255,
                og + rgb[1] as u32 * inv / 255,
                or_ + rgb[0] as u32 * inv / 255,
                255u32,
            )
        } else {
            (rgb[2] as u32, rgb[1] as u32, rgb[0] as u32, 255u32)
        };
        if coverage < 1.0 {
            let c = (coverage * 255.0) as u32;
            let ic = 255 - c;
            b = (b * c + ob * ic) / 255;
            g = (g * c + og * ic) / 255;
            r = (r * c + or_ * ic) / 255;
            a = (a * c + oa * ic) / 255;
        }
        self.framebuffer[index] = b.min(255) as u8;
        self.framebuffer[index + 1] = g.min(255) as u8;
        self.framebuffer[index + 2] = r.min(255) as u8;
        self.framebuffer[index + 3] = a.min(255) as u8;
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

            // Nothing marks the centre: it sits exactly where the blemish
            // being matched is, and a mark there hides what the contours are
            // being lined up against.
            self.stroke_ellipse(defect, 1.0, colour, if selected { 3 } else { 2 });
            if selected {
                // A faint outer ring marks where the Gaussian has essentially
                // died out, which is what the eye is actually matching.
                self.stroke_ellipse(defect, 2.0, Rgba(80, 220, 255, 70), 1);
                let hover_px = self
                    .handle_hover
                    .map(|h| overlay_pixel(h, self.width, self.height));
                for handle in defect.handles() {
                    let hovered = hover_px == Some(overlay_pixel(handle, self.width, self.height));
                    let fill = if hovered { Rgba::HANDLE_HOVER } else { colour };
                    self.fill_square(handle, EditorDefect::HANDLE_HALF_PX, fill);
                }
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

    fn fill_square(&mut self, center: crate::compensation::Vec2, half: i32, colour: Rgba) {
        let (cx, cy) = overlay_pixel(center, self.width, self.height);
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

    /// Draw or lift the list-hover cross. `restore` puts the previous patch
    /// back; a full render has already replaced the buffer, so it skips that.
    fn composite_locator(&mut self, restore: bool) {
        if restore {
            self.restore_locator();
        } else {
            self.locator_restore = None;
        }
        let Some(center) = self.hover else {
            return;
        };
        let cx = (center.x * self.width as f32).round() as i32;
        let cy = (center.y * self.height as f32).round() as i32;
        let pad = LOCATOR_HALF_PX + (LOCATOR_THICKNESS + 1) / 2 + 1;
        let x0 = (cx - pad).max(0);
        let y0 = (cy - pad).max(0);
        let x1 = (cx + pad).min(self.width as i32 - 1);
        let y1 = (cy + pad).min(self.height as i32 - 1);
        if x1 < x0 || y1 < y0 {
            return;
        }
        self.locator_restore = Some(self.capture_rect(x0, y0, x1, y1));
        let colour = Rgba::LOCATOR;
        let half = LOCATOR_HALF_PX;
        self.draw_line(
            (cx - half, cy - half),
            (cx + half, cy + half),
            colour,
            LOCATOR_THICKNESS,
        );
        self.draw_line(
            (cx - half, cy + half),
            (cx + half, cy - half),
            colour,
            LOCATOR_THICKNESS,
        );
    }

    fn restore_locator(&mut self) {
        let Some(saved) = self.locator_restore.take() else {
            return;
        };
        self.blit_rect(saved);
    }

    fn capture_rect(&self, x0: i32, y0: i32, x1: i32, y1: i32) -> SavedRect {
        let width = (x1 - x0 + 1) as u32;
        let height = (y1 - y0 + 1) as u32;
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in y0..=y1 {
            let start = (y as usize * self.width as usize + x0 as usize) * 4;
            let end = start + width as usize * 4;
            pixels.extend_from_slice(&self.framebuffer[start..end]);
        }
        SavedRect {
            x: x0,
            y: y0,
            width,
            height,
            pixels,
        }
    }

    fn blit_rect(&mut self, saved: SavedRect) {
        let mut offset = 0;
        let row = saved.width as usize * 4;
        for y in 0..saved.height as i32 {
            let start = ((saved.y + y) as usize * self.width as usize + saved.x as usize) * 4;
            self.framebuffer[start..start + row]
                .copy_from_slice(&saved.pixels[offset..offset + row]);
            offset += row;
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
        // The compensation is drawn only when nobody is editing on this
        // surface. Resampling the mask over every pixel is by far the most
        // expensive thing here and there is no way to make it cheap enough to
        // follow a pointer, so editing draws the outlines and nothing else.
        match self.editor.as_ref().map(|editor| editor.show) {
            None => {
                let (w, h) = (self.width, self.height);
                let dither = self.dither;
                let mask = std::mem::replace(&mut self.mask, Mask::transparent(2, 2));
                mask::rasterize_argb8888(&mask, &mut self.framebuffer, w, h, dither);
                self.mask = mask;
            }
            Some(show) => {
                self.framebuffer.fill(0);
                if show.draws_model() {
                    if let Some(model) = self.model.take() {
                        self.tint_model(&model);
                        self.model = Some(model);
                    }
                }
            }
        }

        // The calibration disc sits under the outlines and over the mask, so
        // the spot's correction is judged against a known local pattern.
        let spinning = self.draw_edit_pattern();
        self.draw_editor();
        // Fresh pixels: do not put the previous locator back on top of them.
        self.composite_locator(false);

        self.dirty = spinning;
        self.hover_dirty = false;
        self.generation += 1;
    }
}

/// Extra radius, in overlay pixels, around the selected spot's longest axis.
const EDIT_DISC_PADDING_PX: f32 = 300.0;

/// Half-extent of the list-hover cross, in overlay pixels.
const LOCATOR_HALF_PX: i32 = 14;
const LOCATOR_THICKNESS: i32 = 3;

/// One revolution of the disc, in seconds.
const EDIT_DISC_PERIOD: f32 = 8.0;

pub(crate) fn edit_disc_radius_px(spot_rx: f32, spot_ry: f32) -> f32 {
    spot_rx.abs().max(spot_ry.abs()).max(1.0) + EDIT_DISC_PADDING_PX
}

fn edit_disc_angle() -> f32 {
    let t = edit_disc_origin().elapsed().as_secs_f32();
    (t / EDIT_DISC_PERIOD).fract() * std::f32::consts::TAU
}

/// Colour of the rotating editor disc in its local frame: equal conical
/// wedges of `colors`, starting at +u and running counterclockwise.
pub(crate) fn editor_disc_color(u: f32, v: f32, colors: &[[u8; 3]]) -> [u8; 3] {
    match colors {
        [] => [0, 0, 0],
        [only] => *only,
        many => {
            let turn = (v.atan2(u) / std::f32::consts::TAU).rem_euclid(1.0);
            let i = ((turn * many.len() as f32) as usize).min(many.len() - 1);
            many[i]
        }
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
    use crate::overlay::{CalibrationDisc, EditorDefect, ShowMode};
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
    fn editing_draws_the_outlines_and_not_the_correction() {
        let id = Uuid::new_v4();
        let editor = EditorView {
            defects: vec![EditorDefect {
                id,
                center: Vec2::splat(0.5),
                radius: Vec2::splat(0.1),
                rotation: 0.0,
                enabled: true,
            }],
            selected: Some(id),
            show: ShowMode::Outlines,
        };

        // A 25 px spot in the middle of a 256 px surface.
        let mut renderer = CpuMaskRenderer::new(256, 256, false);
        renderer.upload_mask(&spot_mask());
        renderer.set_editor(Some(editor));
        renderer.render();

        // On the contour at 45 degrees, clear of the axis-aligned handles.
        let on_contour = 128 + (0.1 * 256.0 * std::f32::consts::FRAC_1_SQRT_2) as u32;
        assert!(
            alpha_at(&renderer, on_contour, on_contour) > 0,
            "the outline must be drawn"
        );
        // Inside the contour the correction would be near its deepest, and
        // there is nothing else drawn in there.
        assert_eq!(
            alpha_at(&renderer, 138, 138),
            0,
            "the correction must not be drawn while editing"
        );

        // Leaving brings it back from the mask that was uploaded all along,
        // with nothing to regenerate.
        renderer.set_editor(None);
        renderer.render();
        assert!(
            alpha_at(&renderer, 138, 138) > 0,
            "the correction must come back"
        );
    }

    #[test]
    fn model_mode_tints_the_defect_field() {
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
                enabled: true,
            }],
            selected: Some(id),
            show: ShowMode::Outlines,
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

    fn rgb_disc_colors() -> Vec<[u8; 3]> {
        vec![[255, 0, 0], [0, 255, 0], [0, 0, 255]]
    }

    fn calibration_disc(spot_radius: f32, colors: Vec<[u8; 3]>) -> CalibrationDisc {
        calibration_disc_with(spot_radius, colors, true)
    }

    fn calibration_disc_with(
        spot_radius: f32,
        colors: Vec<[u8; 3]>,
        enabled: bool,
    ) -> CalibrationDisc {
        CalibrationDisc {
            defect: EditorDefect {
                id: Uuid::new_v4(),
                center: Vec2::splat(0.5),
                radius: Vec2::splat(spot_radius),
                rotation: 0.0,
                enabled,
            },
            colors,
        }
    }

    #[test]
    fn the_editor_disc_is_equal_conical_wedges() {
        let colors = rgb_disc_colors();
        let at = |turns: f32| {
            let a = turns * std::f32::consts::TAU;
            editor_disc_color(a.cos(), a.sin(), &colors)
        };
        assert_eq!(at(1.0 / 6.0), [255, 0, 0]);
        assert_eq!(at(3.0 / 6.0), [0, 255, 0]);
        assert_eq!(at(5.0 / 6.0), [0, 0, 255]);
        let inner = editor_disc_color(
            0.2 * (std::f32::consts::TAU / 6.0).cos(),
            0.2 * (std::f32::consts::TAU / 6.0).sin(),
            &colors,
        );
        assert_eq!(inner, [255, 0, 0], "a wedge is constant along its radius");
        assert_eq!(
            editor_disc_color(1.0, 0.0, &[[128, 128, 128]]),
            [128, 128, 128]
        );
        assert_eq!(
            editor_disc_color(-1.0, 0.0, &[[128, 128, 128]]),
            [128, 128, 128],
            "one colour fills the whole disc"
        );
    }

    #[test]
    fn editing_draws_an_opaque_disc_behind_the_selected_spot() {
        let mut renderer = CpuMaskRenderer::new(512, 512, false);
        renderer.upload_mask(&spot_mask());
        renderer.set_disc(Some(calibration_disc(0.03, rgb_disc_colors())));
        renderer.render();

        // Spot is 0.03 * 512 px plus 300, about 315 px, so 40 px off centre is
        // inside and a corner is not.
        assert_eq!(alpha_at(&renderer, 256 + 40, 256), 255);
        assert!(
            alpha_at(&renderer, 0, 0) < 10,
            "the disc must not cover the whole panel"
        );
        renderer.render();
        assert!(
            renderer.frame().is_some(),
            "the disc must keep producing frames so it can rotate"
        );
    }

    #[test]
    fn a_disabled_spot_keeps_the_disc_but_drops_the_correction() {
        let grey = [180, 180, 180];
        let mut renderer = CpuMaskRenderer::new(64, 64, false);
        renderer.upload_mask(&spot_mask());
        renderer.set_disc(Some(calibration_disc_with(0.1, vec![grey], false)));
        renderer.render();

        let [b, g, r, a] = pixel(&renderer, 32, 32);
        assert_eq!(a, 255, "the pattern circle must still be drawn");
        assert_eq!(
            [r, g, b],
            grey,
            "unchecking the spot must not bake the correction onto the disc"
        );
    }

    #[test]
    fn a_single_disc_colour_does_not_keep_spinning() {
        let mut renderer = CpuMaskRenderer::new(64, 64, false);
        renderer.set_disc(Some(calibration_disc(0.03, vec![[255, 0, 0]])));
        renderer.render();
        assert_eq!(alpha_at(&renderer, 32, 32), 255);
        renderer.render();
        assert!(
            renderer.frame().is_none(),
            "a solid disc has nothing to animate"
        );
    }

    #[test]
    fn hovering_a_spot_paints_a_red_cross_without_resampling() {
        let mut renderer = CpuMaskRenderer::new(64, 64, false);
        renderer.upload_mask(&spot_mask());
        renderer.render();
        let plain: Vec<u8> = renderer.framebuffer().to_vec();
        let corner = pixel(&renderer, 0, 0);

        renderer.set_hover(Some(Vec2::splat(0.5)));
        assert!(
            !renderer.is_dirty(),
            "a locator must not resample the compensation"
        );
        assert!(
            renderer.frame().is_some(),
            "the locator still has to produce a frame"
        );

        let [b, g, r, a] = pixel(&renderer, 32, 32);
        assert!(a > 0, "the cross must be visible");
        assert!(r > g && r > b, "the cross is red, got [{r}, {g}, {b}]");
        assert_eq!(
            pixel(&renderer, 0, 0),
            corner,
            "pixels away from the cross must be the untouched mask"
        );

        renderer.set_hover(None);
        assert!(!renderer.is_dirty());
        renderer.frame();
        assert_eq!(
            renderer.framebuffer(),
            plain.as_slice(),
            "lifting the pointer must put the mask back"
        );
    }

    #[test]
    fn moving_the_hover_restores_the_previous_cross() {
        let mut renderer = CpuMaskRenderer::new(64, 64, false);
        renderer.upload_mask(&spot_mask());
        renderer.render();
        let at_quarter = pixel(&renderer, 16, 32);

        renderer.set_hover(Some(Vec2::new(0.25, 0.5)));
        renderer.frame();
        renderer.set_hover(Some(Vec2::new(0.75, 0.5)));
        renderer.frame();

        assert_eq!(
            pixel(&renderer, 16, 32),
            at_quarter,
            "the first cross must come off before the second goes on"
        );
        let [_, _, r, a] = pixel(&renderer, 48, 32);
        assert!(a > 0 && r > 80, "the new cross must be drawn");
    }

    #[test]
    fn hovering_a_handle_fills_it_white() {
        let id = Uuid::new_v4();
        let editor = EditorView {
            defects: vec![EditorDefect {
                id,
                center: Vec2::splat(0.5),
                radius: Vec2::splat(0.1),
                rotation: 0.0,
                enabled: true,
            }],
            selected: Some(id),
            show: ShowMode::Outlines,
        };
        let mut renderer = CpuMaskRenderer::new(256, 256, false);
        renderer.set_editor(Some(editor));
        renderer.render();

        let handle = Vec2::new(0.6, 0.5);
        let hx = (handle.x * 256.0).round() as u32;
        let hy = (handle.y * 256.0).round() as u32;
        let rest = pixel(&renderer, hx, hy);

        renderer.set_handle_hover(Some(handle));
        renderer.render();
        let hovered = pixel(&renderer, hx, hy);
        assert_ne!(rest, hovered, "the hovered handle must change colour");
        // Premultiplied white: b, g, r, a.
        assert_eq!(hovered[0], hovered[1]);
        assert_eq!(hovered[1], hovered[2]);
        assert!(hovered[2] > rest[2], "hover is brighter than the rest fill");
    }
}
