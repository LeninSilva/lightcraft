//! The edit of one photo, as the user sees it, and its undo history.
//!
//! A [`Recipe`] holds the choices made in the app (Auto strength, a look and its strength, the
//! Tune sliders, subject focus, framing). [`compose`] turns a recipe plus the photo's
//! [`Analysis`] into the pipeline's [`DevelopSettings`]. Composition is a pure function, so the
//! preview, the look thumbnails and the full-resolution export all agree, and a recipe stored in
//! the library re-renders identically.

use serde::{Deserialize, Serialize};

use lightcraft_develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskShape, Treatment};
use lightcraft_geom::{Orientation, Point, Rect};

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

/// Linear blend of the numeric develop sliders a recipe can touch, `t` of the way from `a` to `b`.
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
        // hue stays the target's (blending hues through the circle would sweep colours)
        w.sat = l(wa.sat, wb.sat);
        w.lum = l(wa.lum, wb.lum);
    }
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

/// Inverse of [`relative_temp`].
pub fn temp_to_relative(k: f64) -> f64 {
    if k <= 0.0 || !k.is_finite() {
        return 0.0;
    }
    clamp((1e6 / 6500.0 - 1e6 / k) / 0.8, -100.0, 100.0)
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
    add(&mut s.color.vibrance, t.vibrance + t.hdr * 0.1);
    add(&mut s.color.saturation, t.saturation);
    if t.fade > 0.0 {
        let lift = clamp(t.fade, 0.0, 100.0) / 100.0 * 0.18;
        let base = std::mem::take(&mut s.curve.master);
        s.curve.master = (0..=8)
            .map(|i| {
                let x = f64::from(i) / 8.0;
                let y = eval_curve(&base, x);
                Point::new(x, lift + y * (1.0 - lift * 0.6))
            })
            .collect();
    }
    add(&mut s.vignette.amount, t.vignette);
    s.grain.amount = clamp(s.grain.amount + t.grain, 0.0, 100.0);
    if t.grain > 0.0 {
        s.grain.size = clamp(10.0 + t.grain_size * 0.8, 0.0, 100.0);
    }
    s.detail.sharpen_amount = clamp(s.detail.sharpen_amount + t.sharpen, 0.0, 150.0);
    let nr = clamp(t.noise_reduction, 0.0, 100.0);
    s.detail.nr_luminance = clamp(s.detail.nr_luminance + nr, 0.0, 100.0);
    s.detail.nr_color = clamp(s.detail.nr_color + nr * 0.5, 0.0, 100.0);
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

fn apply_focus(s: &mut DevelopSettings, f: &Focus, a: &Analysis) {
    let amount = clamp(f.amount, 0.0, 100.0) / 100.0;
    if amount <= 0.0 {
        return;
    }
    let ((cx, cy), r) = focus_geometry(f, a);
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
    s.crop.geometry.angle = clamp(fr.straighten, -45.0, 45.0);
    // the crop rect lives in the oriented frame (after the quarter turns)
    let (mut w, mut h) = (a.width as f64, a.height as f64);
    let mut around = fr.smart.then_some((a.subject.cx, a.subject.cy));
    if fr.turns % 2 == 1 {
        std::mem::swap(&mut w, &mut h);
    }
    if let Some((x, y)) = around {
        // move the subject into the turned/flipped frame
        let (x, y) = if fr.flip_h { (1.0 - x, y) } else { (x, y) };
        around = Some(match fr.turns % 4 {
            1 => (1.0 - y, x),
            2 => (1.0 - x, 1.0 - y),
            3 => (y, 1.0 - x),
            _ => (x, y),
        });
    }
    s.crop.geometry.rect = aspect_crop(fr.aspect.ratio(), w, h, around);
}

/// Turn a recipe into develop settings for this photo.
pub fn compose(r: &Recipe, a: &Analysis) -> DevelopSettings {
    let neutral = DevelopSettings::default();
    let mut s = if r.auto > 0.0 { blend(&neutral, &advisor::enhance(a), clamp(r.auto, 0.0, 100.0) / 100.0) } else { neutral };
    if let Some(id) = &r.look {
        let styled = looks::apply(id, a, &s);
        s = blend(&s, &styled, clamp(r.look_strength, 0.0, 100.0) / 100.0);
    }
    apply_tune(&mut s, &r.tune);
    apply_focus(&mut s, &r.focus, a);
    apply_frame(&mut s, &r.frame, a);
    if s.treatment == Treatment::Bw {
        s.color.saturation = s.color.saturation.min(0.0);
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
    fn curve_eval_edges() {
        assert_eq!(eval_curve(&[], 0.3), 0.3);
        let pts = [Point::new(0.0, 0.1), Point::new(1.0, 0.9)];
        assert!((eval_curve(&pts, 0.5) - 0.5).abs() < 1e-12);
        assert_eq!(eval_curve(&[Point::new(0.5, 0.5), Point::new(0.5, 0.7)], 0.5), 0.5);
    }
}
