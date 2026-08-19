//! Turning a set of defects into the overlay's pixels.
//!
//! The overlay can only remove light, so the whole job is: model the panel's
//! per-channel response `D(x, y)`, pick a target brightness `T ≤ min D`, and
//! attenuate every pixel by `C = T / D` so the panel ends up uniform at `T`.
//!
//! # Why the correction has the shape it does
//!
//! A compositor blends the overlay as `out_c = colour_c + dst_c · (1 - alpha)`:
//! one multiplier shared by all three channels, plus three per-channel offsets.
//! Stacking surfaces does not widen that — composing those maps multiplies the
//! shared factors and leaves the offsets per channel — so no arrangement of
//! overlays can reach a genuine per-channel multiply.
//!
//! That matters because a shared multiplier scales every channel equally and so
//! leaves the ratios between them exactly as it found them. Alpha alone can make
//! a burnt patch dimmer; it can never make it less blue. The only lever that can
//! move a colour cast is the surface's own colour, used to hand back the light
//! the shared alpha over-removed.
//!
//! Two facts then pin the rest down. The offsets cannot be negative, so alpha has
//! to satisfy the channel needing the most attenuation; and setting it exactly
//! there leaves that channel correct at every desktop level for free. What
//! remains is a single scalar, [`REFERENCE`]: the per-channel error is linear in
//! desktop brightness, so it crosses zero once, and that scalar decides where.
//! There is no second degree of freedom to look for.

use serde::{Deserialize, Serialize};

use super::{
    defect::{Defect, DefectModel},
    lerp, Rgb, Vec2,
};

/// Transfer response assumed when converting a wanted luminance ratio into the
/// encoded value the compositor will multiply by.
///
/// Deliberately not a calibration knob. It only decides how a ratio is spelled
/// in encoded terms, and being out by a couple of tenths costs far less than the
/// residual colour cast the overlay cannot remove at all. 2.2 sits close enough
/// to both sRGB and BT.1886 across the range where defects are visible.
pub const GAMMA: f32 = 2.2;

/// Desktop level, encoded `0.0..=1.0`, at which per-channel correction is exact.
///
/// The colour offsets are constants, while the defect they correct is
/// proportional to content. So the correction is right at one brightness and
/// drifts either side: under-corrected above, over-corrected below, and on a
/// black desktop the offsets show up directly as a faint tinted glow — see
/// [`Mask::black_lift`].
///
/// 0.35 minimises the worst visible error over the whole brightness range under
/// a `ΔL/√L` sensitivity model, which is how the eye behaves in the dim scenes
/// where that glow is the risk. The optimum is broad and flat between roughly
/// 0.20 and 0.45. Weighting purely by nits would prefer 0.85 and wreck dark
/// scenes; weighting purely by contrast ratio would refuse any offset at all,
/// and give up colour correction along with it. Irrelevant while every defect is
/// neutral, which is why a neutral mask stays exactly black.
pub const REFERENCE: f32 = 0.35;

/// How finely the alpha field is sampled before the GPU or the CPU upsampler
/// stretches it over the output.
///
/// Panel defects are extremely low-frequency, so a fraction of native
/// resolution is normally indistinguishable from the real thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaskQuality {
    /// One eighth of native resolution in each axis.
    Low,
    /// One quarter of native resolution in each axis.
    #[default]
    Normal,
    /// Full native resolution.
    Native,
}

impl MaskQuality {
    pub const ALL: [MaskQuality; 3] = [MaskQuality::Low, MaskQuality::Normal, MaskQuality::Native];

    pub fn label(self) -> &'static str {
        match self {
            MaskQuality::Low => "Low",
            MaskQuality::Normal => "Normal",
            MaskQuality::Native => "Native",
        }
    }

    pub fn divisor(self) -> u32 {
        match self {
            MaskQuality::Low => 8,
            MaskQuality::Normal => 4,
            MaskQuality::Native => 1,
        }
    }

    /// Mask dimensions for an output of the given pixel size.
    pub fn resolution_for(self, width: u32, height: u32) -> (u32, u32) {
        let d = self.divisor();
        (width.div_ceil(d).max(2), height.div_ceil(d).max(2))
    }
}

/// Everything except the defects themselves that influences the alpha field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaskParams {
    /// Fraction of the computed correction to apply, `0.0..=1.0`.
    ///
    /// At `1.0` the target is the darkest modelled point; lower values trade
    /// uniformity for retained brightness.
    pub compensation: f32,
    pub quality: MaskQuality,
    /// Apply a fixed ordered dither when the mask is quantized to 8 bit.
    pub dither: bool,
}

impl Default for MaskParams {
    fn default() -> Self {
        Self {
            compensation: 1.0,
            quality: MaskQuality::default(),
            dither: true,
        }
    }
}

/// One sampled overlay pixel: premultiplied colour plus alpha, each `0.0..=1.0`.
///
/// Premultiplied is not a storage detail here but the natural form: the colour
/// channels *are* the light being added back, already scaled by the coverage
/// they are added at.
pub type Texel = [f32; 4];

/// A sampled overlay image, transparent where nothing needs correcting.
#[derive(Debug, Clone, PartialEq)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    /// Row-major, `width * height` premultiplied `[r, g, b, a]` entries.
    pub texels: Vec<Texel>,
    /// Dimmest modelled panel response found while generating this mask.
    pub min_gain: Rgb,
    /// Brightest modelled panel response found while generating this mask.
    pub max_gain: Rgb,
    /// Uniform brightness the correction aims for, relative to a healthy pixel.
    pub target: Rgb,
}

impl Mask {
    /// A fully transparent mask: what bypass and "no defects" both look like.
    pub fn transparent(width: u32, height: u32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        Self {
            width,
            height,
            texels: vec![[0.0; 4]; (width as usize) * (height as usize)],
            min_gain: Rgb::ONE,
            max_gain: Rgb::ONE,
            target: Rgb::ONE,
        }
    }

    pub fn is_transparent(&self) -> bool {
        self.texels.iter().all(|t| t.iter().all(|v| *v <= 0.0))
    }

    /// Largest attenuation anywhere, i.e. the worst-case brightness loss.
    pub fn peak_alpha(&self) -> f32 {
        self.texels.iter().map(|t| t[3]).fold(0.0f32, f32::max)
    }

    /// How much light the overlay adds to a pixel that should be pure black.
    ///
    /// This is the price of per-channel correction: the surface can only lower
    /// every channel by the same factor, so the channels that needed less
    /// attenuation get their light handed back as a faint constant glow. Zero
    /// whenever every defect is neutral.
    pub fn black_lift(&self) -> f32 {
        self.texels
            .iter()
            .map(|t| t[0].max(t[1]).max(t[2]))
            .fold(0.0f32, f32::max)
    }

    pub fn texel_at(&self, x: u32, y: u32) -> Texel {
        let x = x.min(self.width - 1) as usize;
        let y = y.min(self.height - 1) as usize;
        self.texels[y * self.width as usize + x]
    }

    pub fn alpha_at(&self, x: u32, y: u32) -> f32 {
        self.texel_at(x, y)[3]
    }

    /// Bilinear sample in normalized coordinates.
    pub fn sample_texel(&self, uv: Vec2) -> Texel {
        let fx = (uv.x * self.width as f32 - 0.5).clamp(0.0, (self.width - 1) as f32);
        let fy = (uv.y * self.height as f32 - 0.5).clamp(0.0, (self.height - 1) as f32);
        let x0 = fx.floor() as u32;
        let y0 = fy.floor() as u32;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;

        let (t00, t10) = (self.texel_at(x0, y0), self.texel_at(x1, y0));
        let (t01, t11) = (self.texel_at(x0, y1), self.texel_at(x1, y1));
        std::array::from_fn(|c| lerp(lerp(t00[c], t10[c], tx), lerp(t01[c], t11[c], tx), ty))
    }

    /// Bilinear sample of the alpha channel alone.
    pub fn sample(&self, uv: Vec2) -> f32 {
        self.sample_texel(uv)[3]
    }
}

/// Build the alpha field for an output of `output_width × output_height`
/// pixels. The returned mask is at the resolution implied by
/// [`MaskParams::quality`], not necessarily at the output's own resolution.
pub fn generate(
    defects: &[Defect],
    params: &MaskParams,
    output_width: u32,
    output_height: u32,
) -> Mask {
    let (width, height) = params.quality.resolution_for(output_width, output_height);
    generate_at(defects, params, width, height)
}

/// Build the alpha field at an explicit mask resolution.
pub fn generate_at(defects: &[Defect], params: &MaskParams, width: u32, height: u32) -> Mask {
    let width = width.max(1);
    let height = height.max(1);
    let count = (width as usize) * (height as usize);

    let active: Vec<&Defect> = defects.iter().filter(|d| d.enabled()).collect();
    if active.is_empty() {
        return Mask::transparent(width, height);
    }

    // First pass: the modelled panel response and its extremes.
    let panel_gain = model_field(&active, width, height, true);
    let (min_gain, max_gain) = extremes(&panel_gain);

    // Second pass: bring every pixel down to the per-channel target.
    let compensation = params.compensation.clamp(0.0, 1.0);
    let target = max_gain.zip(min_gain, |hi, lo| lerp(hi, lo, compensation));
    let inv_gamma = 1.0 / GAMMA;

    let mut texels = vec![[0.0f32; 4]; count];
    for (texel, gain) in texels.iter_mut().zip(panel_gain.iter()) {
        // Encoded attenuation this channel wants, never above 1: the overlay
        // can only remove light, and a dead pixel cannot be matched at all.
        let attenuation = target.zip(*gain, |t, g| {
            let ratio = if g > 1.0e-6 { (t / g).min(1.0) } else { 1.0 };
            ratio.powf(inv_gamma)
        });

        // The shared alpha has to satisfy the channel that needs the most
        // attenuation; the rest get their light handed back as surface colour.
        let deepest = attenuation.min_channel();
        let colour = attenuation.map(|c| REFERENCE * (c - deepest));
        *texel = [
            colour.r,
            colour.g,
            colour.b,
            (1.0 - deepest).clamp(0.0, 1.0),
        ];
    }

    Mask {
        width,
        height,
        texels,
        min_gain,
        max_gain,
        target,
    }
}

/// The modelled panel response `D(x, y)` on a grid, optionally restricted to
/// the area each defect can actually reach.
///
/// Overlapping defects do not stack; the strongest at each point wins. Two
/// shapes that overlap in a profile are normally describing one blemish between
/// them, and multiplying their responses would count the same damage twice.
fn model_field(defects: &[&Defect], width: u32, height: u32, use_bounds: bool) -> Vec<Rgb> {
    let mut gain = vec![Rgb::ONE; (width as usize) * (height as usize)];

    for defect in defects {
        let (x0, x1, y0, y1) = if use_bounds {
            let (lo, hi) = defect.bounds();
            // Only the rows and columns the defect can measurably reach.
            (
                ((lo.x * width as f32).floor().max(0.0)) as u32,
                ((hi.x * width as f32).ceil().min(width as f32)) as u32,
                ((lo.y * height as f32).floor().max(0.0)) as u32,
                ((hi.y * height as f32).ceil().min(height as f32)) as u32,
            )
        } else {
            (0, width, 0, height)
        };

        for y in y0..y1 {
            let v = (y as f32 + 0.5) / height as f32;
            let row = y as usize * width as usize;
            for x in x0..x1 {
                let u = (x as f32 + 0.5) / width as f32;
                let slot = &mut gain[row + x as usize];
                *slot = strongest(*slot, defect.gain_at(Vec2::new(u, v)));
            }
        }
    }

    gain
}

/// The larger departure from a healthy `1.0`, per channel, in either direction.
///
/// A defect can be bright or dim, so "worst" has to mean the same thing for both
/// signs rather than simply the larger or smaller number.
fn strongest(accumulated: Rgb, next: Rgb) -> Rgb {
    accumulated.zip(next, |a, b| {
        if (b - 1.0).abs() > (a - 1.0).abs() {
            b
        } else {
            a
        }
    })
}

/// Dimmest and brightest response in a modelled field, per channel.
///
/// Both come from the samples rather than from an assumed healthy `1.0`: a
/// panel that is uniformly off-brightness needs no correction at all, and
/// pinning either end to `1.0` would have us dim the whole screen to reach a
/// uniformity it already has.
fn extremes(field: &[Rgb]) -> (Rgb, Rgb) {
    let mut min = Rgb::splat(f32::INFINITY);
    let mut max = Rgb::splat(f32::NEG_INFINITY);
    for g in field {
        min = min.zip(*g, f32::min);
        max = max.zip(*g, f32::max);
    }
    if field.is_empty() {
        (Rgb::ONE, Rgb::ONE)
    } else {
        (min, max)
    }
}

/// How far the modelled panel departs from healthy, `|D(x, y) - 1|`, sampled on
/// a grid and stored in the alpha channel.
///
/// This is the model itself rather than the correction derived from it, which
/// is what the on-screen editor shows in "Show model" mode.
pub fn generate_model_field(defects: &[Defect], width: u32, height: u32) -> Mask {
    let width = width.max(1);
    let height = height.max(1);
    let active: Vec<&Defect> = defects.iter().filter(|d| d.enabled()).collect();
    let gain = model_field(&active, width, height, false);
    let (min_gain, max_gain) = extremes(&gain);

    let texels = gain
        .iter()
        .map(|g| {
            [
                0.0,
                0.0,
                0.0,
                g.map(|v| (v - 1.0).abs()).max_channel().min(1.0),
            ]
        })
        .collect();

    Mask {
        width,
        height,
        texels,
        min_gain,
        max_gain,
        target: min_gain,
    }
}

/// The 8×8 ordered dither matrix, in `0..64`.
const BAYER8: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// Sub-LSB offset in `-0.5..0.5` for the pixel at `(x, y)`.
///
/// The pattern is a pure function of position, so it never shimmers, never
/// animates, and averages to zero across every 8×8 tile.
#[inline]
fn dither_offset(x: u32, y: u32) -> f32 {
    let m = BAYER8[(y & 7) as usize][(x & 7) as usize] as f32;
    (m + 0.5) / 64.0 - 0.5
}

#[inline]
fn quantize(alpha: f32, x: u32, y: u32, dither: bool) -> u8 {
    let scaled = alpha.clamp(0.0, 1.0) * 255.0;
    let v = if dither {
        scaled + dither_offset(x, y)
    } else {
        scaled
    };
    v.round().clamp(0.0, 255.0) as u8
}

/// Write one texel into a premultiplied ARGB8888 pixel.
///
/// The byte order is the little-endian layout both `wl_shm`'s `Argb8888` and an
/// X11 32-bit TrueColor visual expect: blue, green, red, alpha. Colour is
/// clamped to the alpha so quantization can never produce the impossible
/// premultiplied pixel that some compositors render as a bright fringe.
#[inline]
fn write_texel(out: &mut [u8], texel: Texel, x: u32, y: u32, dither: bool) {
    let a = quantize(texel[3], x, y, dither);
    out[0] = quantize(texel[2], x, y, dither).min(a);
    out[1] = quantize(texel[1], x, y, dither).min(a);
    out[2] = quantize(texel[0], x, y, dither).min(a);
    out[3] = a;
}

/// Write the mask into a premultiplied ARGB8888 buffer, upsampling as needed.
pub fn rasterize_argb8888(mask: &Mask, out: &mut [u8], out_w: u32, out_h: u32, dither: bool) {
    debug_assert!(out.len() >= (out_w as usize) * (out_h as usize) * 4);
    if out_w == 0 || out_h == 0 {
        return;
    }

    if mask.width == out_w && mask.height == out_h {
        for y in 0..out_h {
            let src = y as usize * out_w as usize;
            let dst = src * 4;
            for x in 0..out_w {
                let p = dst + x as usize * 4;
                write_texel(
                    &mut out[p..p + 4],
                    mask.texels[src + x as usize],
                    x,
                    y,
                    dither,
                );
            }
        }
        return;
    }

    // Precompute the horizontal interpolation so the inner loop is cheap.
    let mut xw: Vec<(u32, u32, f32)> = Vec::with_capacity(out_w as usize);
    for x in 0..out_w {
        let fx = (((x as f32 + 0.5) / out_w as f32) * mask.width as f32 - 0.5)
            .clamp(0.0, (mask.width - 1) as f32);
        let x0 = fx.floor() as u32;
        xw.push((x0, (x0 + 1).min(mask.width - 1), fx - x0 as f32));
    }

    for y in 0..out_h {
        let fy = (((y as f32 + 0.5) / out_h as f32) * mask.height as f32 - 0.5)
            .clamp(0.0, (mask.height - 1) as f32);
        let y0 = fy.floor() as u32;
        let y1 = (y0 + 1).min(mask.height - 1);
        let ty = fy - y0 as f32;
        let row0 = y0 as usize * mask.width as usize;
        let row1 = y1 as usize * mask.width as usize;
        let dst = y as usize * out_w as usize * 4;

        for (x, &(x0, x1, tx)) in xw.iter().enumerate() {
            let t00 = mask.texels[row0 + x0 as usize];
            let t10 = mask.texels[row0 + x1 as usize];
            let t01 = mask.texels[row1 + x0 as usize];
            let t11 = mask.texels[row1 + x1 as usize];
            let texel: Texel = std::array::from_fn(|c| {
                lerp(lerp(t00[c], t10[c], tx), lerp(t01[c], t11[c], tx), ty)
            });
            let p = dst + x * 4;
            write_texel(&mut out[p..p + 4], texel, x as u32, y, dither);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compensation::radial::RadialDefect;
    use approx::assert_relative_eq;

    fn spot(center: Vec2, strength: f32) -> Defect {
        tinted(center, Rgb::splat(strength))
    }

    fn tinted(center: Vec2, strength: Rgb) -> Defect {
        Defect::Radial(RadialDefect {
            center,
            radius: Vec2::splat(0.1),
            strength,
            ..Default::default()
        })
    }

    /// What the compositor will put on screen for a uniform desktop of `level`.
    fn composited(texel: Texel, level: f32) -> Rgb {
        Rgb::new(
            texel[0] + level * (1.0 - texel[3]),
            texel[1] + level * (1.0 - texel[3]),
            texel[2] + level * (1.0 - texel[3]),
        )
    }

    /// The light a defective patch actually emits: the compositor's encoded
    /// output driven through the defect's gain and the display's response.
    fn emitted(texel: Texel, level: f32, gain: Rgb) -> Rgb {
        composited(texel, level).zip(gain, |v, g| g * v.powf(GAMMA))
    }

    /// The encoded attenuation a wanted luminance ratio implies, so expectations
    /// can be written in the luminance terms the model reasons in.
    fn encoded(ratio: f32) -> f32 {
        ratio.powf(1.0 / GAMMA)
    }

    #[test]
    fn no_active_defects_means_no_attenuation() {
        let disabled = Defect::Radial(RadialDefect {
            enabled: false,
            strength: Rgb::splat(0.5),
            ..Default::default()
        });
        for defects in [Vec::new(), vec![disabled]] {
            let mask = generate_at(&defects, &MaskParams::default(), 32, 32);
            assert!(mask.is_transparent());
            assert_eq!(mask.min_gain, Rgb::ONE);
        }
    }

    #[test]
    fn a_bright_spot_is_darkened_where_it_sits() {
        // A spot emitting 15 % too much light, corrected at full strength: the
        // spot drops to 1/1.15 of its own output and the rest is left alone.
        let params = MaskParams {
            compensation: 1.0,
            dither: false,
            ..Default::default()
        };
        let mask = generate_at(&[spot(Vec2::splat(0.5), 0.15)], &params, 65, 65);

        assert_relative_eq!(mask.max_gain.r, 1.15, epsilon = 1e-3);
        assert_relative_eq!(mask.min_gain.r, 1.0, epsilon = 1e-3);
        assert_relative_eq!(mask.target.r, 1.0, epsilon = 1e-3);

        // Centre of the spot: attenuated back to a healthy pixel's brightness.
        assert_relative_eq!(
            mask.alpha_at(32, 32),
            1.0 - encoded(1.0 / 1.15),
            epsilon = 1e-3
        );
        assert!(
            mask.alpha_at(32, 32) < 1.0 - 1.0 / 1.15,
            "gamma encoding must use less coverage than linear attenuation"
        );
        // Far corner, already healthy: untouched.
        assert_relative_eq!(mask.alpha_at(0, 0), 0.0, epsilon = 1e-3);
    }

    #[test]
    fn a_dim_patch_still_dims_everything_around_it() {
        // The other sign: a 15 % dark patch can only be matched by bringing the
        // healthy majority of the panel down to it.
        let params = MaskParams {
            compensation: 1.0,
            dither: false,
            ..Default::default()
        };
        let mask = generate_at(&[spot(Vec2::splat(0.5), -0.15)], &params, 65, 65);

        assert_relative_eq!(mask.min_gain.r, 0.85, epsilon = 1e-3);
        assert_relative_eq!(mask.target.r, 0.85, epsilon = 1e-3);
        assert_relative_eq!(mask.alpha_at(32, 32), 0.0, epsilon = 1e-3);
        assert_relative_eq!(mask.alpha_at(0, 0), 1.0 - encoded(0.85), epsilon = 1e-3);
        let mut pixels = vec![0xAA; 65 * 65 * 4];
        rasterize_argb8888(&mask, &mut pixels, 65, 65, false);
        assert!(
            pixels.chunks_exact(4).all(|pixel| pixel[..3] == [0, 0, 0]),
            "neutral correction must rasterize as black with varying alpha"
        );
    }

    #[test]
    fn a_uniformly_off_panel_needs_no_correction() {
        // Every pixel equally bright is already uniform; dimming it would cost
        // brightness and buy nothing.
        let params = MaskParams {
            dither: false,
            ..Default::default()
        };
        let huge = Defect::Radial(RadialDefect {
            center: Vec2::splat(0.5),
            radius: Vec2::splat(50.0),
            strength: Rgb::splat(0.2),
            ..Default::default()
        });
        let mask = generate_at(&[huge], &params, 16, 16);
        assert!(mask.peak_alpha() < 1e-3, "peak alpha {}", mask.peak_alpha());
    }

    #[test]
    fn compensation_scales_the_correction() {
        let mut params = MaskParams {
            dither: false,
            ..Default::default()
        };
        params.compensation = 0.5;
        let mask = generate_at(&[spot(Vec2::splat(0.5), 0.2)], &params, 65, 65);

        // Half way between no correction (T = 1.2) and full (T = 1.0).
        assert_relative_eq!(mask.target.r, 1.1, epsilon = 1e-3);
        assert_relative_eq!(
            mask.alpha_at(32, 32),
            1.0 - encoded(1.1 / 1.2),
            epsilon = 1e-3
        );
        // A healthy pixel is already at or below the partial target.
        assert_relative_eq!(mask.alpha_at(0, 0), 0.0, epsilon = 1e-6);

        params.compensation = 0.0;
        let off = generate_at(&[spot(Vec2::splat(0.5), 0.2)], &params, 65, 65);
        assert!(off.is_transparent());
    }

    #[test]
    fn overlapping_defects_take_the_strongest_rather_than_stacking() {
        // Two coincident 10 % spots describe one blemish, not a 1.1 * 1.1 = 1.21
        // one, so the model must not compound them.
        let params = MaskParams {
            compensation: 1.0,
            dither: false,
            ..Default::default()
        };
        let defects = [spot(Vec2::splat(0.5), 0.1), spot(Vec2::splat(0.5), 0.1)];
        let mask = generate_at(&defects, &params, 65, 65);
        assert_relative_eq!(mask.max_gain.r, 1.1, epsilon = 1e-3);
    }

    #[test]
    fn the_strongest_rule_reads_both_signs_of_damage() {
        assert_relative_eq!(strongest(Rgb::splat(1.1), Rgb::splat(1.2)).r, 1.2);
        assert_relative_eq!(strongest(Rgb::splat(0.9), Rgb::splat(0.8)).r, 0.8);
        // A dim patch outranks a milder bright one, and vice versa.
        assert_relative_eq!(strongest(Rgb::splat(1.1), Rgb::splat(0.7)).r, 0.7);
        // Channels are independent.
        assert_eq!(
            strongest(Rgb::new(1.2, 1.0, 1.0), Rgb::new(1.0, 1.5, 1.0)),
            Rgb::new(1.2, 1.5, 1.0)
        );
    }

    #[test]
    fn every_channel_is_corrected_independently() {
        // A spot that is 20 % too red and correct elsewhere.
        let params = MaskParams {
            compensation: 1.0,
            dither: false,
            ..Default::default()
        };
        let mask = generate_at(
            &[tinted(Vec2::splat(0.5), Rgb::new(0.2, 0.0, 0.0))],
            &params,
            65,
            65,
        );

        // On the reference grey the red channel comes down by exactly the ratio
        // it needs and the other two are left where they were.
        let out = composited(mask.texel_at(32, 32), REFERENCE);
        assert_relative_eq!(out.r, REFERENCE * encoded(1.0 / 1.2), epsilon = 1e-4);
        assert_relative_eq!(out.g, REFERENCE, epsilon = 1e-4);
        assert_relative_eq!(out.b, REFERENCE, epsilon = 1e-4);

        // Away from the spot nothing is touched at all.
        assert_eq!(mask.texel_at(0, 0), [0.0; 4]);
    }

    #[test]
    fn the_shared_alpha_cannot_move_a_colour_cast() {
        // Alpha scales all three channels by one factor, so on its own it can
        // only make a patch dimmer -- the cast survives untouched. Correcting a
        // tint is the entire reason the colour channels are there.
        let params = MaskParams {
            dither: false,
            ..Default::default()
        };
        let gain = Rgb::new(1.0, 1.1, 1.2);
        let mask = generate_at(
            &[tinted(Vec2::splat(0.5), Rgb::new(0.0, 0.1, 0.2))],
            &params,
            65,
            65,
        );
        let texel = mask.texel_at(32, 32);

        let alpha_only = emitted([0.0, 0.0, 0.0, texel[3]], REFERENCE, gain);
        assert_relative_eq!(alpha_only.b / alpha_only.r, 1.2, epsilon = 1e-3);

        let corrected = emitted(texel, REFERENCE, gain);
        assert_relative_eq!(corrected.b / corrected.r, 1.0, epsilon = 1e-3);
    }

    #[test]
    fn a_neutral_defect_stays_pure_black() {
        // The colour channels only ever carry the per-channel difference, so a
        // neutral correction must reduce exactly to the black-with-alpha case.
        let params = MaskParams {
            dither: false,
            ..Default::default()
        };
        let mask = generate_at(&[spot(Vec2::splat(0.5), 0.15)], &params, 33, 33);
        assert!(mask
            .texels
            .iter()
            .all(|t| t[0] == 0.0 && t[1] == 0.0 && t[2] == 0.0));
        assert_eq!(mask.black_lift(), 0.0);
    }

    #[test]
    fn per_channel_correction_lifts_black_by_a_reported_amount() {
        let params = MaskParams {
            dither: false,
            ..Default::default()
        };
        let mask = generate_at(
            &[tinted(Vec2::splat(0.5), Rgb::new(0.2, 0.0, 0.0))],
            &params,
            65,
            65,
        );

        // Green and blue keep the light red had to give up.
        let expected = REFERENCE * (1.0 - encoded(1.0 / 1.2));
        assert_relative_eq!(mask.black_lift(), expected, epsilon = 1e-4);
        assert_relative_eq!(
            composited(mask.texel_at(32, 32), 0.0).g,
            expected,
            epsilon = 1e-4
        );
        // Where one channel needs no attenuation at all, the glow is exactly the
        // reference fraction of the alpha beside it. That is the whole trade in
        // one line: the offsets buy colour accuracy and are paid for on black.
        assert_relative_eq!(
            mask.black_lift(),
            REFERENCE * mask.peak_alpha(),
            epsilon = 1e-6
        );
    }

    #[test]
    fn colour_is_exact_at_the_reference_level_and_drifts_either_side() {
        let params = MaskParams {
            dither: false,
            ..Default::default()
        };
        let gain = Rgb::new(1.2, 1.1, 1.0);
        let mask = generate_at(
            &[tinted(Vec2::splat(0.5), Rgb::new(0.2, 0.1, 0.0))],
            &params,
            65,
            65,
        );
        let texel = mask.texel_at(32, 32);

        // At the reference level every channel lands on the healthy target, so
        // the patch is indistinguishable from the panel around it.
        let out = composited(texel, REFERENCE);
        assert_relative_eq!(out.r, REFERENCE * encoded(1.0 / 1.2), epsilon = 1e-4);
        assert_relative_eq!(out.g, REFERENCE * encoded(1.0 / 1.1), epsilon = 1e-4);
        assert_relative_eq!(out.b, REFERENCE, epsilon = 1e-4);

        // Red needed the most attenuation, so it carries no offset and alpha
        // alone corrects it exactly at every level. That is precisely why alpha
        // is pinned to the deepest channel rather than anywhere else.
        let healthy = |level: f32| level.powf(GAMMA);
        for level in [0.05f32, 0.35, 0.8, 1.0] {
            assert_relative_eq!(
                emitted(texel, level, gain).r,
                healthy(level),
                epsilon = 1e-6
            );
        }

        // The other two drift, because their offsets are constant while the
        // defect is proportional to content: above the reference level the
        // offset is too small and the patch is left slightly dark, below it the
        // offset is too large and the patch is left slightly bright.
        assert!(emitted(texel, 0.9, gain).g < healthy(0.9));
        assert!(emitted(texel, 0.1, gain).g > healthy(0.1));
    }

    #[test]
    fn texels_stay_valid_premultiplied_values() {
        let params = MaskParams {
            compensation: 1.0,
            ..Default::default()
        };
        let defects = [
            tinted(Vec2::new(0.2, 0.3), Rgb::new(0.9, 0.1, 0.5)),
            tinted(Vec2::new(0.25, 0.3), Rgb::new(0.0, 0.9, -0.4)),
        ];
        let mask = generate_at(&defects, &params, 48, 48);
        for texel in &mask.texels {
            assert!(texel.iter().all(|v| (0.0..=1.0).contains(v)), "{texel:?}");
            assert!(
                texel[0].max(texel[1]).max(texel[2]) <= texel[3] + 1e-6,
                "{texel:?}"
            );
        }
    }

    #[test]
    fn quality_controls_the_sampled_resolution() {
        let params = MaskParams {
            quality: MaskQuality::Normal,
            ..Default::default()
        };
        let mask = generate(&[spot(Vec2::splat(0.5), 0.1)], &params, 3840, 2160);
        assert_eq!((3840 / mask.width, 2160 / mask.height), (4, 4));

        let native = MaskParams {
            quality: MaskQuality::Native,
            ..params
        };
        let mask = generate(&[spot(Vec2::splat(0.5), 0.1)], &native, 1920, 1080);
        assert_eq!((mask.width, mask.height), (1920, 1080));
    }

    #[test]
    fn low_resolution_masks_stay_close_to_native() {
        // The whole reduced-resolution idea only holds if the error is small.
        let defects = [tinted(Vec2::new(0.63, 0.41), Rgb::new(0.12, 0.08, 0.08))];
        let params = MaskParams {
            dither: false,
            ..Default::default()
        };
        let native = generate_at(&defects, &params, 480, 270);
        let low = generate_at(&defects, &params, 60, 34);

        let mut worst = 0.0f32;
        for y in 0..native.height {
            for x in 0..native.width {
                let uv = Vec2::new(
                    (x as f32 + 0.5) / native.width as f32,
                    (y as f32 + 0.5) / native.height as f32,
                );
                let (a, b) = (native.texel_at(x, y), low.sample_texel(uv));
                for c in 0..4 {
                    worst = worst.max((a[c] - b[c]).abs());
                }
            }
        }
        // Well under one 8-bit step.
        assert!(worst < 1.0 / 255.0, "worst error {worst}");
    }

    #[test]
    fn rasterizes_the_colour_channels_premultiplied() {
        let params = MaskParams {
            dither: false,
            ..Default::default()
        };
        let mask = generate_at(
            &[tinted(Vec2::splat(0.5), Rgb::new(0.2, 0.0, 0.0))],
            &params,
            17,
            17,
        );
        let mut buf = vec![0u8; 17 * 17 * 4];
        rasterize_argb8888(&mask, &mut buf, 17, 17, false);

        // Little-endian ARGB8888: blue, green, red, alpha.
        let centre = &buf[(8 * 17 + 8) * 4..][..4];
        let texel = mask.texel_at(8, 8);
        assert_eq!(centre[3], (texel[3] * 255.0).round() as u8);
        assert_eq!(
            centre[2], 0,
            "the red channel needed all of the attenuation"
        );
        assert_eq!(centre[1], (texel[1] * 255.0).round() as u8);
        assert!(centre[1] > 0 && centre[1] <= centre[3]);
    }

    #[test]
    fn rasterizer_upsamples_bilinearly() {
        let params = MaskParams {
            compensation: 1.0,
            dither: false,
            ..Default::default()
        };
        let mask = generate_at(
            &[tinted(Vec2::splat(0.5), Rgb::new(0.2, 0.1, 0.1))],
            &params,
            32,
            32,
        );
        let mut buf = vec![0u8; 128 * 128 * 4];
        rasterize_argb8888(&mask, &mut buf, 128, 128, false);

        for y in 0..128u32 {
            for x in 0..128u32 {
                let uv = Vec2::new((x as f32 + 0.5) / 128.0, (y as f32 + 0.5) / 128.0);
                let texel = mask.sample_texel(uv);
                let got = &buf[(y as usize * 128 + x as usize) * 4..][..4];
                for (channel, expected) in [(3, texel[3]), (1, texel[1]), (2, texel[0])] {
                    let expected = (expected * 255.0).round() as i32;
                    assert!(
                        (got[channel] as i32 - expected).abs() <= 1,
                        "{} vs {expected} in channel {channel} at {x},{y}",
                        got[channel]
                    );
                }
            }
        }
    }

    #[test]
    fn dither_has_zero_mean_and_is_stable() {
        let mut sum = 0.0f32;
        for y in 0..8 {
            for x in 0..8 {
                sum += dither_offset(x, y);
            }
        }
        assert_relative_eq!(sum, 0.0, epsilon = 1e-5);
        // Purely positional: the same pixel always gets the same offset.
        assert_eq!(dither_offset(3, 5), dither_offset(11, 13));
        // And it never shifts a value by a whole step.
        assert!((0..64).all(|i| dither_offset(i % 8, i / 8).abs() <= 0.5));
    }

    #[test]
    fn dither_never_moves_a_flat_field_by_more_than_one_step() {
        let mask = Mask {
            width: 8,
            height: 8,
            texels: vec![[0.0, 0.0, 0.0, 0.2]; 64],
            min_gain: Rgb::splat(0.8),
            max_gain: Rgb::ONE,
            target: Rgb::splat(0.8),
        };
        let mut buf = vec![0u8; 8 * 8 * 4];
        rasterize_argb8888(&mask, &mut buf, 8, 8, true);
        let exact = (0.2f32 * 255.0).round() as i32;
        for px in buf.chunks_exact(4) {
            assert!((px[3] as i32 - exact).abs() <= 1);
        }
    }
}
