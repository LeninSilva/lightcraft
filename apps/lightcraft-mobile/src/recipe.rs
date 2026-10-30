//! The edit of one photo, as the user sees it, and its undo history.
//!
//! A [`Recipe`] holds the choices made in the app (Auto strength, a look and its strength, the
//! Tune sliders, subject focus, framing). [`compose`] turns a recipe plus the photo's
//! [`Analysis`] into the pipeline's [`DevelopSettings`]. Composition is a pure function, so the
//! preview, the look thumbnails and the full-resolution export all agree, and a recipe stored in
//! the library re-renders identically.

use serde::{Deserialize, Serialize};

use lightcraft_develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskShape, Treatment, WbMode};
use lightcraft_geom::{CropGeometry, Orientation, Point, Rect, max_inscribed_scale};

use crate::analysis::Analysis;
use crate::{advisor, looks};

/// Tune sliders. Each is 0 when untouched; ranges are on [`TuneControl`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tune {
    /// Stops (−4..4).
    pub exposure: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
    /// HDR look 0..100: opens shadows, recovers highlights and adds local contrast.
    pub hdr: f64,
    pub warmth: f64,
    pub tint: f64,
    pub vibrance: f64,
    pub saturation: f64,
    pub clarity: f64,
    pub texture: f64,
    pub dehaze: f64,
    /// 0..100: lifts the blacks for a matte finish.
    pub fade: f64,
    /// −100..100: negative darkens the edges.
    pub vignette: f64,
    /// 0..100.
    pub grain: f64,
    /// 0..100.
    pub grain_size: f64,
    /// 0..150.
    pub sharpen: f64,
    /// 0..100.
    pub noise_reduction: f64,
}

/// One Tune slider: how the UI shows it and where its value lives.
#[derive(Clone, Copy, Debug)]
pub struct TuneControl {
    pub id: &'static str,
    pub label: &'static str,
    pub group: TuneGroup,
    pub min: f64,
    pub max: f64,
    pub get: fn(&Tune) -> f64,
    pub set: fn(&mut Tune, f64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuneGroup {
    Light,
    Color,
    Effects,
    Detail,
}

impl TuneGroup {
    pub const ALL: [TuneGroup; 4] = [TuneGroup::Light, TuneGroup::Color, TuneGroup::Effects, TuneGroup::Detail];
    pub fn label(self) -> &'static str {
        match self {
            TuneGroup::Light => "Light",
            TuneGroup::Color => "Color",
            TuneGroup::Effects => "Effects",
            TuneGroup::Detail => "Detail",
        }
    }
}

macro_rules! tune_controls {
    ($($field:ident, $label:literal, $group:ident, $min:literal, $max:literal;)*) => {
        /// Every Tune slider, in display order.
        pub const TUNE_CONTROLS: &[TuneControl] = &[$(
            TuneControl {
                id: stringify!($field),
                label: $label,
                group: TuneGroup::$group,
                min: $min,
                max: $max,
                get: |t| t.$field,
                set: |t, v| t.$field = v,
            },
        )*];
    };
}

tune_controls! {
    exposure, "Exposure", Light, -4.0, 4.0;
    contrast, "Contrast", Light, -100.0, 100.0;
    highlights, "Highlights", Light, -100.0, 100.0;
    shadows, "Shadows", Light, -100.0, 100.0;
    whites, "Whites", Light, -100.0, 100.0;
    blacks, "Blacks", Light, -100.0, 100.0;
    hdr, "HDR", Light, 0.0, 100.0;
    warmth, "Warmth", Color, -100.0, 100.0;
    tint, "Tint", Color, -100.0, 100.0;
    vibrance, "Vibrance", Color, -100.0, 100.0;
    saturation, "Saturation", Color, -100.0, 100.0;
    clarity, "Clarity", Effects, -100.0, 100.0;
    texture, "Texture", Effects, -100.0, 100.0;
    dehaze, "Dehaze", Effects, -100.0, 100.0;
    fade, "Fade", Effects, 0.0, 100.0;
    vignette, "Vignette", Effects, -100.0, 100.0;
    grain, "Grain", Effects, 0.0, 100.0;
    grain_size, "Grain size", Effects, 0.0, 100.0;
    sharpen, "Sharpen", Detail, 0.0, 150.0;
    noise_reduction, "Noise reduction", Detail, 0.0, 100.0;
}

/// Find a Tune control by id.
pub fn tune_control(id: &str) -> Option<&'static TuneControl> {
    TUNE_CONTROLS.iter().find(|c| c.id == id)
}

/// Subject focus: brighten and crisp the subject, quiet the background.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Focus {
    /// 0..100; 0 is off.
    pub amount: f64,
    /// Centre and radius in normalized image coordinates; `None` uses the detected subject.
    pub center: Option<(f64, f64)>,
    /// Radius as a fraction of the long edge (0.05..0.9); `None` uses the detected subject.
    pub radius: Option<f64>,
    /// 0..100: how much the background is quieted (darker, softer, less saturated).
    pub background: f64,
}

impl Default for Focus {
    fn default() -> Self {
        Focus { amount: 0.0, center: None, radius: None, background: 50.0 }
    }
}

/// Crop aspect ratios offered in the Crop tool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Aspect {
    #[default]
    Original,
    Square,
    Portrait4x5,
    Classic3x2,
    Wide16x9,
    Story9x16,
}

impl Aspect {
    pub const ALL: [Aspect; 6] = [Aspect::Original, Aspect::Square, Aspect::Portrait4x5, Aspect::Classic3x2, Aspect::Wide16x9, Aspect::Story9x16];
    pub fn label(self) -> &'static str {
        match self {
            Aspect::Original => "Original",
            Aspect::Square => "1:1",
            Aspect::Portrait4x5 => "4:5",
            Aspect::Classic3x2 => "3:2",
            Aspect::Wide16x9 => "16:9",
            Aspect::Story9x16 => "9:16",
        }
    }
    /// Width / height, `None` for the photo's own.
    pub fn ratio(self) -> Option<f64> {
        match self {
            Aspect::Original => None,
            Aspect::Square => Some(1.0),
            Aspect::Portrait4x5 => Some(4.0 / 5.0),
            Aspect::Classic3x2 => Some(3.0 / 2.0),
            Aspect::Wide16x9 => Some(16.0 / 9.0),
            Aspect::Story9x16 => Some(9.0 / 16.0),
        }
    }
}

/// Framing: rotation, flips, straighten and an aspect crop placed around the subject.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Frame {
    pub aspect: Aspect,
    /// Clockwise quarter turns 0..3.
    pub turns: u8,
    pub flip_h: bool,
    /// Degrees, −45..45.
    pub straighten: f64,
    /// Place the crop by the rule of thirds around the subject (true) or centre it (false).
    pub smart: bool,
}

/// The whole edit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Recipe {
    /// Auto enhancement strength 0..100 (0 = off).
    pub auto: f64,
    /// Look id ([`looks::LOOKS`]) and strength 0..100.
    pub look: Option<String>,
    pub look_strength: f64,
    pub tune: Tune,
    pub focus: Focus,
    pub frame: Frame,
}

impl Default for Recipe {
    fn default() -> Self {
        Recipe { auto: 0.0, look: None, look_strength: 100.0, tune: Tune::default(), focus: Focus::default(), frame: Frame::default() }
    }
}

impl Recipe {
    pub fn is_original(&self) -> bool {
        *self == Recipe::default()
    }
}

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_finite() { v.clamp(lo, hi) } else { 0.0_f64.clamp(lo, hi) }
}

/// Blend two hues along the shortest arc of the colour circle, `t` of the way from `a`
/// to `b`. A straight linear blend would sweep through unrelated hues (for example from
/// 350 degrees to 10 degrees through 180); the shortest arc never moves further than
/// the target hue. Non-finite inputs fall back to `b` sanitised into range.
fn blend_hue(a: f64, b: f64, t: f64) -> f64 {
    if !a.is_finite() || !b.is_finite() {
        return if b.is_finite() { b.rem_euclid(360.0) } else { 0.0 };
    }
    let delta = (b - a + 540.0).rem_euclid(360.0) - 180.0;
    (a + delta * t).rem_euclid(360.0)
}

/// Linear blend of the numeric develop sliders a recipe can touch, `t` of the way from `a` to `b`.
///
/// Every field a look or the Auto enhancement sets is interpolated, so a look at
/// strength 0 is the base settings and at strength 1 is the look's settings, with no
/// field snapping early. Grading hues blend along the shortest arc (see
/// [`blend_hue`]); `treatment` is the one exception, it snaps to `b` at `t >= 0.5`,
/// because there is no halfway between colour and black and white. Fields no look
/// or enhancement sets (mixer, calibration, optics, masks) are identical in `a` and
/// `b` at every call site, so they come from `b` unchanged.
pub fn blend(a: &DevelopSettings, b: &DevelopSettings, t: f64) -> DevelopSettings {
    let t = clamp(t, 0.0, 1.0);
    let l = |x: f64, y: f64| x + (y - x) * t;
    let mut s = b.clone();
    s.light.exposure = l(a.light.exposure, b.light.exposure);
    s.light.contrast = l(a.light.contrast, b.light.contrast);
    s.light.highlights = l(a.light.highlights, b.light.highlights);
    s.light.shadows = l(a.light.shadows, b.light.shadows);
    s.light.whites = l(a.light.whites, b.light.whites);
    s.light.blacks = l(a.light.blacks, b.light.blacks);
    s.wb.temp = l(a.wb.temp, b.wb.temp);
    s.wb.tint = l(a.wb.tint, b.wb.tint);
    s.color.vibrance = l(a.color.vibrance, b.color.vibrance);
    s.color.saturation = l(a.color.saturation, b.color.saturation);
    s.effects.clarity = l(a.effects.clarity, b.effects.clarity);
    s.effects.texture = l(a.effects.texture, b.effects.texture);
    s.effects.dehaze = l(a.effects.dehaze, b.effects.dehaze);
    s.vignette.amount = l(a.vignette.amount, b.vignette.amount);
    s.grain.amount = l(a.grain.amount, b.grain.amount);
    s.detail.sharpen_amount = l(a.detail.sharpen_amount, b.detail.sharpen_amount);
    s.detail.nr_luminance = l(a.detail.nr_luminance, b.detail.nr_luminance);
    s.detail.nr_color = l(a.detail.nr_color, b.detail.nr_color);
    s.curve.highlights = l(a.curve.highlights, b.curve.highlights);
    s.curve.lights = l(a.curve.lights, b.curve.lights);
    s.curve.darks = l(a.curve.darks, b.curve.darks);
    s.curve.shadows = l(a.curve.shadows, b.curve.shadows);
    for (w, (wa, wb)) in [
        (&mut s.grading.shadows, (a.grading.shadows, b.grading.shadows)),
        (&mut s.grading.midtones, (a.grading.midtones, b.grading.midtones)),
        (&mut s.grading.highlights, (a.grading.highlights, b.grading.highlights)),
        (&mut s.grading.global, (a.grading.global, b.grading.global)),
    ] {
        w.hue = blend_hue(wa.hue, wb.hue, t);
        w.sat = l(wa.sat, wb.sat);
        w.lum = l(wa.lum, wb.lum);
    }
    s.grading.blending = l(a.grading.blending, b.grading.blending);
    s.grading.balance = l(a.grading.balance, b.grading.balance);
    s.grain.size = l(a.grain.size, b.grain.size);
    s.grain.roughness = l(a.grain.roughness, b.grain.roughness);
    s.vignette.midpoint = l(a.vignette.midpoint, b.vignette.midpoint);
    s.vignette.roundness = l(a.vignette.roundness, b.vignette.roundness);
    s.vignette.feather = l(a.vignette.feather, b.vignette.feather);
    s.vignette.highlights = l(a.vignette.highlights, b.vignette.highlights);
    s.detail.sharpen_radius = l(a.detail.sharpen_radius, b.detail.sharpen_radius);
    s.detail.sharpen_detail = l(a.detail.sharpen_detail, b.detail.sharpen_detail);
    s.detail.sharpen_masking = l(a.detail.sharpen_masking, b.detail.sharpen_masking);
    s.detail.nr_detail = l(a.detail.nr_detail, b.detail.nr_detail);
    s.detail.nr_contrast = l(a.detail.nr_contrast, b.detail.nr_contrast);
    s.detail.nr_color_detail = l(a.detail.nr_color_detail, b.detail.nr_color_detail);
    s.detail.nr_color_smoothness = l(a.detail.nr_color_smoothness, b.detail.nr_color_smoothness);
    s.curve.master = blend_curve(&a.curve.master, &b.curve.master, t);
    if t < 0.5 {
        s.treatment = a.treatment;
    }
    s
}

/// Blend two point curves by sampling both at fixed inputs.
fn blend_curve(a: &[Point], b: &[Point], t: f64) -> Vec<Point> {
    if a.is_empty() && b.is_empty() {
        return Vec::new();
    }
    (0..=8)
        .map(|i| {
            let x = f64::from(i) / 8.0;
            let y = eval_curve(a, x) + (eval_curve(b, x) - eval_curve(a, x)) * t;
            Point::new(x, y.clamp(0.0, 1.0))
        })
        .collect()
}

/// Piecewise-linear evaluation of a point curve (identity when empty).
pub fn eval_curve(pts: &[Point], x: f64) -> f64 {
    let Some(first) = pts.first() else { return x };
    if x <= first.x {
        return first.y;
    }
    for w in pts.windows(2) {
        if let [p, q] = w
            && x <= q.x
        {
            let span = q.x - p.x;
            return if span <= 1e-12 { q.y } else { p.y + (q.y - p.y) * (x - p.x) / span };
        }
    }
    pts.last().map_or(x, |p| p.y)
}

/// Relative white balance slider (−100..100, mireds) to the pipeline's temperature for rendered
/// files (6500 K = as shot); the same mapping as the desktop app's relative Temp slider.
pub fn relative_temp(r: f64) -> f64 {
    1e6 / (1e6 / 6500.0 - clamp(r, -100.0, 100.0) * 0.8)
}

/// Inverse of [`relative_temp`]. Unclamped: clamping to -100..100 destroys
/// information for base temperatures below 4276 K (the relative scale's
/// floor), so a warm base can never round-trip. The non-positive and
/// non-finite guard stays: those temperatures have no relative value.
pub fn temp_to_relative(k: f64) -> f64 {
    if k <= 0.0 || !k.is_finite() {
        return 0.0;
    }
    (1e6 / 6500.0 - 1e6 / k) / 0.8
}

fn apply_tune(s: &mut DevelopSettings, t: &Tune) {
    let add = |v: &mut f64, d: f64| *v = clamp(*v + d, -100.0, 100.0);
    s.light.exposure = clamp(s.light.exposure + t.exposure, -5.0, 5.0);
    add(&mut s.light.contrast, t.contrast);
    add(&mut s.light.highlights, t.highlights - t.hdr * 0.7);
    add(&mut s.light.shadows, t.shadows + t.hdr * 0.6);
    add(&mut s.light.whites, t.whites - t.hdr * 0.15);
    add(&mut s.light.blacks, t.blacks + t.hdr * 0.1);
    add(&mut s.effects.clarity, t.clarity + t.hdr * 0.35);
    add(&mut s.effects.texture, t.texture + t.hdr * 0.1);
    add(&mut s.effects.dehaze, t.dehaze);
    s.wb.temp = relative_temp(temp_to_relative(s.wb.temp) + t.warmth);
    s.wb.tint = clamp(s.wb.tint + t.tint, -150.0, 150.0);
    // A nonzero warmth/tint change only renders with an explicit mode: the
    // pipeline returns the as-shot white whenever the mode is `AsShot`.
    if t.warmth != 0.0 || t.tint != 0.0 {
        s.wb.mode = WbMode::Custom;
    }
    add(&mut s.color.vibrance, t.vibrance + t.hdr * 0.1);
    add(&mut s.color.saturation, t.saturation);
    if t.fade > 0.0 {
        let lift = clamp(t.fade, 0.0, 100.0) / 100.0 * 0.18;
        let base = std::mem::take(&mut s.curve.master);
        s.curve.master = (0..=8)
            .map(|i| {
                let x = f64::from(i) / 8.0;
                let y = eval_curve(&base, x);
                // The lift pushes the white end above 1.0 without this clamp
                // (`Point::new` does not clamp); a curve point above white is
                // outside the tone curve's range.
                Point::new(x, (lift + y * (1.0 - lift * 0.6)).clamp(0.0, 1.0))
            })
            .collect();
    }
    add(&mut s.vignette.amount, t.vignette);
    s.grain.amount = clamp(s.grain.amount + t.grain, 0.0, 100.0);
    // The size override applies whenever grain is present (from a look or
    // the Grain slider) and the user set a size, or added grain through the
    // slider. Gating on the Grain slider alone left the size slider dead
    // when the grain came only from a look; gating on grain alone would
    // move the size from its default whenever a look adds grain and the
    // user never touched the size slider.
    if s.grain.amount > 0.0 && (t.grain > 0.0 || t.grain_size != 0.0) {
        s.grain.size = clamp(10.0 + t.grain_size * 0.8, 0.0, 100.0);
    }
    s.detail.sharpen_amount = clamp(s.detail.sharpen_amount + t.sharpen, 0.0, 150.0);
    let nr = clamp(t.noise_reduction, 0.0, 100.0);
    s.detail.nr_luminance = clamp(s.detail.nr_luminance + nr, 0.0, 100.0);
    s.detail.nr_color = clamp(s.detail.nr_color + nr * 0.5, 0.0, 100.0);
}

/// Move a point from the analysis frame (the EXIF-oriented image the subject
/// was detected in) into the turned/flipped frame the crop rectangle and the
/// mask shapes live in: the flip first, then the clockwise quarter turns, the
/// same mapping [`Frame::sample`] applies to the image itself.
fn into_turned_frame(p: (f64, f64), flip_h: bool, turns: u8) -> (f64, f64) {
    let (x, y) = if flip_h { (1.0 - p.0, p.1) } else { p };
    match turns % 4 {
        1 => (1.0 - y, x),
        2 => (1.0 - x, 1.0 - y),
        3 => (y, 1.0 - x),
        _ => (x, y),
    }
}

/// The focus centre and radius a recipe uses on a photo.
pub fn focus_geometry(f: &Focus, a: &Analysis) -> ((f64, f64), f64) {
    let c = f.center.unwrap_or((a.subject.cx, a.subject.cy));
    let c = (clamp(c.0, 0.0, 1.0), clamp(c.1, 0.0, 1.0));
    let aspect = if a.height > 0 { a.width as f64 / a.height as f64 } else { 1.0 };
    let detected = if aspect >= 1.0 { a.subject.rx.max(a.subject.ry / aspect) } else { (a.subject.rx * aspect).max(a.subject.ry) };
    let r = f.radius.unwrap_or(detected * 1.25);
    (c, clamp(r, 0.05, 0.9))
}

fn apply_focus(s: &mut DevelopSettings, f: &Focus, a: &Analysis, fr: &Frame) {
    let amount = clamp(f.amount, 0.0, 100.0) / 100.0;
    if amount <= 0.0 {
        return;
    }
    let ((cx, cy), r) = focus_geometry(f, a);
    // The centre arrives in the analysis frame; masks are sampled in the
    // turned/flipped frame, so it moves with the framing like the crop's
    // subject point does.
    let (cx, cy) = into_turned_frame((cx, cy), fr.flip_h, fr.turns);
    // `r` is a fraction of the long edge, and radial radii are normalized to
    // the long edge too (see `MaskShape::Radial`): equal radii are already a
    // circle in pixels on any aspect, so no width/height conversion.
    let r = clamp(r, 0.0, 1.0);
    let radial = |invert: bool| MaskShape::Radial { center: Point::new(cx, cy), rx: r, ry: r, angle: 0.0, feather: 70.0, invert };
    let subject = LocalAdjustments {
        exposure: 0.25 * amount,
        clarity: 25.0 * amount,
        texture: 15.0 * amount,
        saturation: 10.0 * amount,
        shadows: 15.0 * amount,
        ..LocalAdjustments::default()
    };
    let bg = clamp(f.background, 0.0, 100.0) / 100.0 * amount;
    let background = LocalAdjustments {
        exposure: -0.45 * bg,
        clarity: -30.0 * bg,
        texture: -20.0 * bg,
        saturation: -18.0 * bg,
        highlights: -15.0 * bg,
        ..LocalAdjustments::default()
    };
    let next_id = s.masks.iter().map(|m| m.id).max().unwrap_or(0);
    let mask = |id: u32, name: &str, invert: bool, adjust: LocalAdjustments| Mask {
        id,
        name: name.into(),
        components: vec![MaskComponent { name: None, op: Default::default(), invert: false, shape: radial(invert) }],
        adjust,
        ..Mask::default()
    };
    s.masks.push(mask(next_id + 1, "Subject", false, subject));
    if bg > 0.0 {
        s.masks.push(mask(next_id + 2, "Background", true, background));
    }
}

/// The crop rectangle (normalized to the rotated, oriented frame) for an aspect around a point.
pub fn aspect_crop(aspect: Option<f64>, frame_w: f64, frame_h: f64, around: Option<(f64, f64)>) -> Rect {
    let Some(ratio) = aspect.filter(|r| r.is_finite() && *r > 0.0) else { return Rect::UNIT };
    if frame_w <= 0.0 || frame_h <= 0.0 {
        return Rect::UNIT;
    }
    let frame = frame_w / frame_h;
    let (w, h) = if ratio > frame { (1.0, frame / ratio) } else { (ratio / frame, 1.0) };
    // rule of thirds: put the point on the nearest third line when there's room to move
    let place = |size: f64, p: Option<f64>| -> f64 {
        let slack = 1.0 - size;
        if slack <= 1e-9 {
            return 0.0;
        }
        let Some(p) = p else { return slack / 2.0 };
        let thirds = [size / 3.0, size * 2.0 / 3.0];
        let target = thirds.iter().map(|t| p - t).min_by(|x, y| (x - slack / 2.0).abs().total_cmp(&(y - slack / 2.0).abs())).unwrap_or(slack / 2.0);
        target.clamp(0.0, slack)
    };
    let x0 = place(w, around.map(|a| a.0));
    let y0 = place(h, around.map(|a| a.1));
    Rect::new(x0, y0, x0 + w, y0 + h)
}

fn apply_frame(s: &mut DevelopSettings, fr: &Frame, a: &Analysis) {
    s.orientation = Orientation::from_parts(fr.flip_h, fr.turns % 4);
    let angle = clamp(fr.straighten, -45.0, 45.0);
    // the crop rect lives in the oriented frame (after the quarter turns)
    let (mut w, mut h) = (a.width as f64, a.height as f64);
    let mut around = fr.smart.then_some((a.subject.cx, a.subject.cy));
    if fr.turns % 2 == 1 {
        std::mem::swap(&mut w, &mut h);
    }
    if let Some(p) = around {
        // move the subject into the turned/flipped frame
        around = Some(into_turned_frame(p, fr.flip_h, fr.turns));
    }
    let rect = aspect_crop(fr.aspect.ratio(), w, h, around);
    let mut geometry = CropGeometry { rect, angle };
    if angle != 0.0 && w > 0.0 && h > 0.0 {
        // A straightened crop must stay inside the rotated image, or the
        // sampler smears edge pixels into the corners. Shrink it about its
        // own centre, keeping its aspect, to the largest size that fits at
        // this angle; an off-centre crop the centred scale does not cover
        // falls back to the engine's own angle handling.
        let px = rect.scale(w, h);
        let scale = max_inscribed_scale(w, h, angle, px.width(), px.height()).min(1.0);
        if scale.is_finite() && scale < 1.0 {
            geometry.rect = Rect::from_center(px.center(), px.width() * scale, px.height() * scale).scale(1.0 / w, 1.0 / h);
        }
        if !geometry.is_within_image(w, h) {
            geometry = geometry.with_angle(angle, w, h);
        }
    }
    s.crop.geometry = geometry;
}

/// Turn a recipe into develop settings for this photo, in one fixed order, used by
/// the preview, the look thumbnails and the full-resolution export alike: Auto is
/// blended in first, then the look (blended by its strength, over the Auto result),
/// then the Tune sliders (which always win over Auto and the look), then Focus
/// masks, then framing. A look's aim fields overwrite the Auto result at full
/// strength; at partial strength the two are blended, never added.
pub fn compose(r: &Recipe, a: &Analysis) -> DevelopSettings {
    let neutral = DevelopSettings::default();
    let mut s = if r.auto > 0.0 { blend(&neutral, &advisor::enhance(a), clamp(r.auto, 0.0, 100.0) / 100.0) } else { neutral };
    if let Some(id) = &r.look {
        let styled = looks::apply(id, a, &s);
        s = blend(&s, &styled, clamp(r.look_strength, 0.0, 100.0) / 100.0);
    }
    apply_tune(&mut s, &r.tune);
    apply_focus(&mut s, &r.focus, a, &r.frame);
    apply_frame(&mut s, &r.frame, a);
    if s.treatment == Treatment::Bw {
        // A Tune slider applied after the look must not resurrect colour: both
        // saturation and vibrance stay at or below zero under black and white.
        s.color.saturation = s.color.saturation.min(0.0);
        s.color.vibrance = s.color.vibrance.min(0.0);
    }
    s
}

/// Undo/redo history of recipes. Consecutive pushes with the same `key` (one slider drag)
/// coalesce into one step.
#[derive(Clone, Debug, Default)]
pub struct History {
    past: Vec<Recipe>,
    future: Vec<Recipe>,
    last_key: Option<String>,
}

/// Longest undo history kept.
pub const MAX_HISTORY: usize = 100;

impl History {
    /// Record `before` (the recipe as it was) ahead of a change. `key` names the gesture.
    pub fn record(&mut self, before: &Recipe, key: &str) {
        if self.last_key.as_deref() == Some(key) && !key.is_empty() {
            return;
        }
        // A snapshot identical to the most recent one would restore the same
        // state, so it adds a no-op undo step (e.g. a drag that returns a
        // slider to its starting value). Skip the push, but keep the key so
        // the rest of this gesture still coalesces.
        if self.past.last() == Some(before) {
            self.last_key = Some(key.to_string());
            return;
        }
        self.past.push(before.clone());
        if self.past.len() > MAX_HISTORY {
            self.past.remove(0);
        }
        self.future.clear();
        self.last_key = Some(key.to_string());
    }
    /// End the current gesture so the next change starts a new step.
    pub fn seal(&mut self) {
        self.last_key = None;
    }
    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }
    pub fn undo(&mut self, current: &Recipe) -> Option<Recipe> {
        let prev = self.past.pop()?;
        self.future.push(current.clone());
        self.last_key = None;
        Some(prev)
    }
    pub fn redo(&mut self, current: &Recipe) -> Option<Recipe> {
        let next = self.future.pop()?;
        self.past.push(current.clone());
        self.last_key = None;
        Some(next)
    }
    pub fn clear(&mut self) {
        *self = History::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analysis() -> Analysis {
        Analysis { width: 300, height: 200, ..Analysis::default() }
    }

    #[test]
    fn original_recipe_is_neutral_settings() {
        assert_eq!(compose(&Recipe::default(), &analysis()), DevelopSettings::default());
    }

    #[test]
    fn tune_maps_to_settings_and_clamps() {
        let mut r = Recipe::default();
        r.tune.exposure = 1.0;
        r.tune.contrast = 500.0;
        r.tune.warmth = 30.0;
        r.tune.saturation = f64::NAN;
        let s = compose(&r, &analysis());
        assert_eq!(s.light.exposure, 1.0);
        assert_eq!(s.light.contrast, 100.0);
        assert!(s.wb.temp > 6500.0);
        assert!(s.color.saturation.is_finite());
    }

    #[test]
    fn every_tune_control_round_trips() {
        let mut t = Tune::default();
        for c in TUNE_CONTROLS {
            (c.set)(&mut t, c.max);
            assert_eq!((c.get)(&t), c.max, "{}", c.id);
            assert!(c.min < c.max);
            assert!(tune_control(c.id).is_some());
        }
    }

    #[test]
    fn relative_temp_inverts() {
        for r in [-100.0, -40.0, 0.0, 25.0, 100.0] {
            assert!((temp_to_relative(relative_temp(r)) - r).abs() < 1e-9);
        }
        assert_eq!(relative_temp(0.0), 6500.0);
        // Below the relative scale's floor the value is not clamped away:
        // 3000 K maps below -100, and the guard still holds for bad input.
        assert!(temp_to_relative(3000.0) < -100.0);
        assert_eq!(temp_to_relative(0.0), 0.0);
        assert_eq!(temp_to_relative(f64::NAN), 0.0);
    }

    #[test]
    fn grain_size_slider_works_when_grain_comes_from_a_look() {
        let mut r = Recipe::default();
        r.look = Some("matte".to_string());
        r.look_strength = 100.0;
        // Grain from the look alone, size untouched: the default size stays.
        assert_eq!(compose(&r, &analysis()).grain.size, 25.0);
        // Size set, Grain slider untouched: the size override applies.
        r.tune.grain_size = 50.0;
        assert_eq!(compose(&r, &analysis()).grain.size, 50.0);
        // Grain added through the slider still sizes the grain as before.
        let mut r2 = Recipe::default();
        r2.tune.grain = 10.0;
        assert_eq!(compose(&r2, &analysis()).grain.size, 10.0);
        r2.tune.grain_size = 50.0;
        assert_eq!(compose(&r2, &analysis()).grain.size, 50.0);
    }

    #[test]
    fn nonzero_warmth_or_tint_sets_custom_white_balance() {
        let mut r = Recipe::default();
        r.tune.warmth = 20.0;
        assert_eq!(compose(&r, &analysis()).wb.mode, WbMode::Custom);
        let mut r = Recipe::default();
        r.tune.tint = -20.0;
        assert_eq!(compose(&r, &analysis()).wb.mode, WbMode::Custom);
        assert_eq!(compose(&Recipe::default(), &analysis()).wb.mode, DevelopSettings::default().wb.mode);
    }

    #[test]
    fn aspect_crop_shapes() {
        assert_eq!(aspect_crop(None, 3.0, 2.0, None), Rect::UNIT);
        let sq = aspect_crop(Some(1.0), 300.0, 200.0, None);
        assert!((sq.width() * 300.0 - sq.height() * 200.0).abs() < 1e-6);
        assert!((sq.x0 - (1.0 - sq.width()) / 2.0).abs() < 1e-9);
        // a subject on the right moves the crop right, inside the frame
        let r = aspect_crop(Some(1.0), 300.0, 200.0, Some((0.9, 0.5)));
        assert!(r.x0 > sq.x0 && r.x1 <= 1.0 + 1e-12);
        assert_eq!(aspect_crop(Some(1.0), 0.0, 0.0, None), Rect::UNIT);
    }

    #[test]
    fn focus_adds_subject_and_background_masks() {
        let mut r = Recipe::default();
        r.focus.amount = 80.0;
        let s = compose(&r, &analysis());
        assert_eq!(s.masks.len(), 2);
        assert!(s.masks[0].adjust.exposure > 0.0 && s.masks[1].adjust.exposure < 0.0);
        r.focus.background = 0.0;
        assert_eq!(compose(&r, &analysis()).masks.len(), 1);
    }

    #[test]
    fn frame_turns_and_flips() {
        let mut r = Recipe::default();
        r.frame.turns = 1;
        r.frame.flip_h = true;
        r.frame.aspect = Aspect::Square;
        r.frame.smart = true;
        let s = compose(&r, &analysis());
        assert_eq!(s.orientation, Orientation::from_parts(true, 1));
        assert!(s.crop.geometry.rect.width() <= 1.0 && s.crop.geometry.rect.height() <= 1.0);
    }

    #[test]
    fn history_coalesces_and_undoes() {
        let mut h = History::default();
        let mut r = Recipe::default();
        h.record(&r, "exposure");
        r.tune.exposure = 0.5;
        h.record(&r, "exposure");
        r.tune.exposure = 1.0;
        h.seal();
        h.record(&r, "contrast");
        r.tune.contrast = 20.0;
        let back = h.undo(&r).unwrap();
        assert_eq!(back.tune.exposure, 1.0);
        assert_eq!(back.tune.contrast, 0.0);
        let back2 = h.undo(&back).unwrap();
        assert_eq!(back2, Recipe::default());
        assert!(!h.can_undo());
        let fwd = h.redo(&back2).unwrap();
        assert_eq!(fwd.tune.exposure, 1.0);
    }

    #[test]
    fn history_skips_identical_consecutive_snapshots() {
        let mut h = History::default();
        let r = Recipe::default();
        h.record(&r, "a");
        h.seal();
        // Same state, different key: pushing it would add a no-op undo step.
        h.record(&r, "b");
        let back = h.undo(&r).unwrap();
        assert_eq!(back, Recipe::default());
        assert!(!h.can_undo());
    }

    #[test]
    fn blend_hue_takes_the_shortest_arc_and_follows_strength() {
        assert_eq!(blend_hue(350.0, 10.0, 0.0), 350.0);
        assert!((blend_hue(350.0, 10.0, 0.5) - 0.0).abs() < 1e-9);
        assert_eq!(blend_hue(350.0, 10.0, 1.0), 10.0);
        assert!((blend_hue(30.0, 350.0, 0.5) - 10.0).abs() < 1e-9);
        assert_eq!(blend_hue(10.0, 350.0, 0.5), blend_hue(350.0, 10.0, 0.5));
    }

    #[test]
    fn blend_grading_hues_and_blending_follow_strength() {
        let base = DevelopSettings::default();
        let mut styled = looks::apply("golden", &analysis(), &base);
        styled.grading.blending = 80.0;
        styled.grading.balance = 20.0;
        let none = blend(&base, &styled, 0.0);
        assert_eq!(none.grading.shadows.hue, base.grading.shadows.hue);
        assert_eq!(none.grading.blending, base.grading.blending);
        let half = blend(&base, &styled, 0.5);
        assert!((half.grading.shadows.hue - 15.0).abs() < 1e-9, "{}", half.grading.shadows.hue);
        assert!((half.grading.blending - (base.grading.blending + 80.0) / 2.0).abs() < 1e-9);
        assert!((half.grading.balance - 10.0).abs() < 1e-9);
        let full = blend(&base, &styled, 1.0);
        assert_eq!(full.grading.shadows.hue, 30.0);
        assert_eq!(full.grading.blending, 80.0);
    }

    #[test]
    fn blend_interpolates_grain_size_and_vignette_shape() {
        let mut a = DevelopSettings::default();
        let mut b = DevelopSettings::default();
        a.grain.size = 10.0;
        b.grain.size = 90.0;
        a.vignette.feather = 20.0;
        b.vignette.feather = 80.0;
        let half = blend(&a, &b, 0.5);
        assert_eq!(half.grain.size, 50.0);
        assert_eq!(half.vignette.feather, 50.0);
        assert_eq!(blend(&a, &b, 0.0).grain.size, 10.0);
    }

    #[test]
    fn focus_mask_is_round_in_pixels_on_a_wide_photo() {
        let mut r = Recipe::default();
        r.focus.amount = 80.0;
        let s = compose(&r, &analysis());
        let shape = &s.masks[0].components[0].shape;
        let MaskShape::Radial { rx, ry, .. } = shape else {
            panic!("focus mask is not radial: {shape:?}");
        };
        // Radial radii are normalized to the long edge, so equal radii are
        // already a circle in pixels on a 300x200 photo.
        assert_eq!(*rx, *ry, "rx={rx} ry={ry}");
        assert!((*rx - 0.3125).abs() < 1e-9, "rx={rx}");
    }

    #[test]
    fn focus_centre_moves_with_turns_and_flips() {
        use crate::analysis::Subject;
        let a = Analysis {
            width: 300,
            height: 200,
            subject: Subject { cx: 0.8, cy: 0.3, ..Subject::default() },
            ..Analysis::default()
        };
        let mut r = Recipe::default();
        r.focus.amount = 80.0;
        r.frame.turns = 1;
        r.frame.flip_h = true;
        let s = compose(&r, &a);
        let shape = &s.masks[0].components[0].shape;
        let MaskShape::Radial { center, .. } = shape else {
            panic!("focus mask is not radial: {shape:?}");
        };
        // Flip first: (0.8, 0.3) -> (0.2, 0.3); one clockwise turn: -> (0.7, 0.2).
        assert!((center.x - 0.7).abs() < 1e-9 && (center.y - 0.2).abs() < 1e-9, "centre={center:?}");
    }

    #[test]
    fn straightened_crop_stays_inside_the_rotated_image() {
        let mut r = Recipe::default();
        r.frame.straighten = 30.0;
        let s = compose(&r, &analysis());
        let geometry = s.crop.geometry;
        assert!(geometry.is_within_image(300.0, 200.0), "rect={:?}", geometry.rect);
        assert!(geometry.rect.width() < 1.0 && geometry.rect.height() < 1.0);
        // No straighten: the full frame is untouched.
        let plain = compose(&Recipe::default(), &analysis());
        assert_eq!(plain.crop.geometry.rect, Rect::UNIT);
    }

    #[test]
    fn fade_curve_stays_inside_the_tone_range() {
        let mut r = Recipe::default();
        r.tune.fade = 100.0;
        let matte = Recipe { look: Some("matte".to_string()), look_strength: 100.0, tune: r.tune, ..Recipe::default() };
        for recipe in [&r, &matte] {
            let s = compose(recipe, &analysis());
            for p in &s.curve.master {
                assert!(p.y >= 0.0 && p.y <= 1.0, "fade point {p:?}");
            }
        }
    }

    #[test]
    fn black_and_white_look_plus_tune_stays_black_and_white() {
        let mut r = Recipe::default();
        r.look = Some("noir".to_string());
        r.look_strength = 100.0;
        r.tune.saturation = 50.0;
        r.tune.vibrance = 50.0;
        let s = compose(&r, &analysis());
        assert_eq!(s.treatment, Treatment::Bw);
        assert!(s.color.saturation <= 0.0 && s.color.vibrance <= 0.0);
    }

    #[test]
    fn auto_look_and_tune_apply_in_order() {
        let mut r = Recipe::default();
        r.auto = 100.0;
        r.look = Some("clear".to_string());
        r.look_strength = 100.0;
        let styled = looks::apply("clear", &analysis(), &DevelopSettings::default());
        assert_eq!(compose(&r, &analysis()).light.exposure, styled.light.exposure);
        r.tune.exposure = 1.0;
        assert_eq!(compose(&r, &analysis()).light.exposure, styled.light.exposure + 1.0);
    }

    #[test]
    fn curve_eval_edges() {
        assert_eq!(eval_curve(&[], 0.3), 0.3);
        let pts = [Point::new(0.0, 0.1), Point::new(1.0, 0.9)];
        assert!((eval_curve(&pts, 0.5) - 0.5).abs() < 1e-12);
        assert_eq!(eval_curve(&[Point::new(0.5, 0.5), Point::new(0.5, 0.7)], 0.5), 0.5);
    }
}
