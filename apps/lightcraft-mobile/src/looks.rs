//! Adaptive looks: named styles whose settings are computed from each photo's [`Analysis`], so
//! the same look lands differently on a bright beach and a dim street (it aims for a result, not
//! a fixed slider offset).
//!
//! A look returns full target settings built on `base`: [`crate::recipe::compose`] blends
//! from the current settings to the look's, so a look mostly moves the sliders the recipe
//! blend knows about (light, white balance, colour, effects, detail, curve, grading,
//! vignette and grain). Grading hues blend along the shortest arc with the strength,
//! so a look at 10% strength does not yet carry the look's full hue shift; `treatment`
//! is the one field that snaps, to the look's value once the strength reaches 50%.

use lightcraft_develop::{DevelopSettings, Treatment, WbMode, Wheel};
use lightcraft_geom::Point;

use crate::analysis::Analysis;

/// A look as the UI lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    pub id: &'static str,
    pub name: &'static str,
    /// One short line on what it does.
    pub blurb: &'static str,
}

/// Every look, in display order.
pub const LOOKS: &[Look] = &[
    Look { id: "clear", name: "Clear", blurb: "Clean, honest colour and balanced light" },
    Look { id: "golden", name: "Golden", blurb: "Warm light, gentle glow, film grain" },
    Look { id: "cool", name: "Cool", blurb: "Crisp blue light for overcast and shade" },
    Look { id: "vivid", name: "Vivid", blurb: "Colour and punch, without blowing out" },
    Look { id: "matte", name: "Matte", blurb: "Lifted blacks and muted colour, like film" },
    Look { id: "noir", name: "Noir", blurb: "Black and white, deep shadows, grain" },
    Look { id: "soft", name: "Soft", blurb: "Low contrast and warm light for faces" },
];

/// Find a look by id.
pub fn look(id: &str) -> Option<&'static Look> {
    LOOKS.iter().find(|l| l.id == id)
}

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_finite() { v.clamp(lo, hi) } else { 0.0_f64.clamp(lo, hi) }
}

/// sRGB EOTF inverse: a display-referred value to linear light. `Analysis::p50`
/// is a display (gamma-encoded) median, so an exposure ratio computed from it
/// directly understates the correction in stops. Decoding the median is an
/// approximation of the median of the linear values (the exact linear median
/// is not stored on `Analysis`); see the same helper in `advisor.rs`.
fn srgb_decode(c: f64) -> f64 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Exposure that moves the photo's median luma to `target` (stops, capped),
/// computed in linear light.
fn aim_exposure(a: &Analysis, target: f64) -> f64 {
    let linear_p50 = srgb_decode(a.p50).max(1e-4);
    (srgb_decode(target) / linear_p50).log2().clamp(-1.5, 1.5)
}

/// Contrast slider that moves the photo's measured contrast toward `target`.
fn aim_contrast(a: &Analysis, target: f64) -> f64 {
    ((target - a.contrast) * 300.0).clamp(-60.0, 60.0)
}

/// Highlights/shadows/whites/blacks that recover what the analysis found clipped.
fn recover(a: &Analysis) -> (f64, f64, f64, f64) {
    let highlights = (-a.clipped_highlights * 400.0).clamp(-80.0, 0.0);
    let shadows = (a.clipped_shadows * 400.0).clamp(0.0, 80.0);
    let whites = (-a.clipped_highlights * 200.0).clamp(-50.0, 0.0);
    let blacks = (a.clipped_shadows * 150.0).clamp(-30.0, 40.0);
    (highlights, shadows, whites, blacks)
}

fn set_light(s: &mut DevelopSettings, a: &Analysis, key: f64, contrast: f64, hi_extra: f64, sh_extra: f64) {
    let (hi, sh, wh, bl) = recover(a);
    s.light.exposure = aim_exposure(a, key);
    s.light.contrast = aim_contrast(a, contrast);
    s.light.highlights = clamp(hi + hi_extra, -100.0, 100.0);
    s.light.shadows = clamp(sh + sh_extra, -100.0, 100.0);
    s.light.whites = wh;
    s.light.blacks = bl;
}

/// White balance that moves the photo's measured cast toward `warm_target`/`tint_target`
/// (analysis units, -1..1). Temperature is anchored on neutral (6500 K), not
/// added to the incoming settings' temperature: the incoming settings may
/// already carry the Auto enhancement's correction, and adding the delta again
/// would double-count it. Tint is likewise absolute, not relative to the
/// incoming tint. The mode must be `Custom`: with the default `AsShot` mode the
/// pipeline returns the as-shot white and ignores temp/tint entirely.
fn set_white_balance(s: &mut DevelopSettings, a: &Analysis, warm_target: f64, tint_target: f64) {
    s.wb.mode = WbMode::Custom;
    s.wb.temp = clamp(6500.0 + (warm_target - a.warmth) * 2000.0, 2500.0, 12_000.0);
    s.wb.tint = clamp((tint_target - a.tint) * 100.0, -100.0, 100.0);
}

/// Vibrance and saturation that move measured saturation toward `target`.
fn set_colour(s: &mut DevelopSettings, a: &Analysis, target: f64, extra_vibrance: f64) {
    s.color.vibrance = clamp((target - a.saturation) * 120.0 + extra_vibrance, -30.0, 40.0);
    s.color.saturation = clamp((target - a.saturation) * 60.0, -20.0, 30.0);
}

fn set_effects(s: &mut DevelopSettings, _a: &Analysis, clarity: f64, texture: f64, dehaze: f64) {
    s.effects.clarity = clamp(clarity, -100.0, 100.0);
    s.effects.texture = clamp(texture, -100.0, 100.0);
    // No hidden haze term here: a caller passing 0.0 means no dehaze. Each
    // caller folds its own haze term into the argument it passes.
    s.effects.dehaze = clamp(dehaze, -100.0, 100.0);
}

fn set_detail(s: &mut DevelopSettings, a: &Analysis, sharpen_base: f64) {
    s.detail.sharpen_amount = clamp(sharpen_base - a.noise * 120.0, 0.0, 70.0);
    s.detail.nr_luminance = clamp(a.noise * 70.0, 0.0, 60.0);
    s.detail.nr_color = clamp(a.noise * 35.0, 0.0, 30.0);
}

fn wheel(hue: f64, sat: f64, lum: f64) -> Wheel {
    Wheel { hue, sat, lum }
}

fn set_grading(s: &mut DevelopSettings, shadows: Wheel, midtones: Wheel, highlights: Wheel, global: Wheel) {
    s.grading.shadows = shadows;
    s.grading.midtones = midtones;
    s.grading.highlights = highlights;
    s.grading.global = global;
    // Blending and balance are the base's: a look sets hues, it does not
    // reset the caller's grading balance (the engine default is 50/0, and
    // `blend` interpolates these fields from the base at partial strength).
}

fn set_curve(s: &mut DevelopSettings, highlights: f64, lights: f64, darks: f64, shadows: f64) {
    s.curve.highlights = highlights;
    s.curve.lights = lights;
    s.curve.darks = darks;
    s.curve.shadows = shadows;
    // The point curve is left alone: clearing it here would silently drop a
    // caller's master curve on every look. Matte writes its own master curve
    // explicitly, after calling this.
}

/// A look's vignette and grain, shape included: `blend` interpolates the
/// size, roughness and vignette shape fields from the base, so leaving them
/// unset would make every look share the base's grain and vignette shape.
/// Looks without a character reason for a field pass the engine default
/// (grain 25/50, vignette midpoint 50, roundness 0, feather 50, highlights 0).
#[allow(clippy::too_many_arguments)]
fn set_finish(
    s: &mut DevelopSettings,
    vignette_amount: f64,
    grain_amount: f64,
    grain_size: f64,
    grain_roughness: f64,
    midpoint: f64,
    roundness: f64,
    feather: f64,
    highlights: f64,
) {
    s.vignette.amount = vignette_amount;
    s.vignette.midpoint = midpoint;
    s.vignette.roundness = roundness;
    s.vignette.feather = feather;
    s.vignette.highlights = highlights;
    s.grain.amount = grain_amount;
    s.grain.size = grain_size;
    s.grain.roughness = grain_roughness;
}

fn neutral_grading(s: &mut DevelopSettings) {
    set_grading(s, wheel(0.0, 0.0, 0.0), wheel(0.0, 0.0, 0.0), wheel(0.0, 0.0, 0.0), wheel(0.0, 0.0, 0.0));
}

fn clear_look(a: &Analysis, base: &DevelopSettings) -> DevelopSettings {
    let mut s = base.clone();
    set_light(&mut s, a, 0.45, 0.21, 0.0, 0.0);
    set_white_balance(&mut s, a, 0.0, 0.0);
    set_colour(&mut s, a, 0.32, 0.0);
    set_effects(&mut s, a, clamp(12.0 - a.detail * 30.0, 0.0, 18.0), 6.0, a.haze * 55.0);
    set_detail(&mut s, a, 30.0);
    neutral_grading(&mut s);
    set_curve(&mut s, 4.0, 2.0, -2.0, -4.0);
    set_finish(&mut s, 0.0, 0.0, 25.0, 50.0, 50.0, 0.0, 50.0, 0.0);
    s
}

fn golden_look(a: &Analysis, base: &DevelopSettings) -> DevelopSettings {
    let mut s = base.clone();
    set_light(&mut s, a, 0.47, 0.23, -5.0, 5.0);
    set_white_balance(&mut s, a, 0.30, 0.05);
    set_colour(&mut s, a, 0.38, 5.0);
    set_effects(&mut s, a, 12.0, 8.0, a.haze * 45.0);
    set_detail(&mut s, a, 28.0);
    set_grading(&mut s, wheel(30.0, 10.0, 4.0), wheel(35.0, 8.0, 2.0), wheel(40.0, 18.0, 5.0), wheel(35.0, 8.0, 0.0));
    set_curve(&mut s, 8.0, 4.0, -4.0, -8.0);
    set_finish(&mut s, -8.0, 12.0, 30.0, 60.0, 45.0, 10.0, 60.0, 10.0);
    s
}

fn cool_look(a: &Analysis, base: &DevelopSettings) -> DevelopSettings {
    let mut s = base.clone();
    set_light(&mut s, a, 0.44, 0.23, -5.0, 0.0);
    set_white_balance(&mut s, a, -0.25, 0.0);
    set_colour(&mut s, a, 0.30, 0.0);
    set_effects(&mut s, a, 16.0, 6.0, a.haze * 45.0);
    set_detail(&mut s, a, 34.0);
    set_grading(&mut s, wheel(220.0, 12.0, 2.0), wheel(210.0, 6.0, 0.0), wheel(200.0, 14.0, 4.0), wheel(210.0, 6.0, 0.0));
    set_curve(&mut s, 6.0, 3.0, -3.0, -6.0);
    set_finish(&mut s, -4.0, 0.0, 25.0, 50.0, 50.0, 0.0, 60.0, 0.0);
    s
}

fn vivid_look(a: &Analysis, base: &DevelopSettings) -> DevelopSettings {
    let mut s = base.clone();
    set_light(&mut s, a, 0.45, 0.26, -10.0, 5.0);
    set_white_balance(&mut s, a, 0.05, 0.0);
    set_colour(&mut s, a, 0.55, 10.0);
    set_effects(&mut s, a, 20.0, 12.0, a.haze * 50.0 + 5.0);
    set_detail(&mut s, a, 40.0);
    neutral_grading(&mut s);
    set_curve(&mut s, 10.0, 5.0, -5.0, -10.0);
    set_finish(&mut s, -6.0, 0.0, 25.0, 50.0, 50.0, 0.0, 50.0, 0.0);
    s
}

fn matte_look(a: &Analysis, base: &DevelopSettings) -> DevelopSettings {
    let mut s = base.clone();
    set_light(&mut s, a, 0.47, 0.16, 0.0, 10.0);
    set_white_balance(&mut s, a, 0.12, 0.0);
    set_colour(&mut s, a, 0.22, 0.0);
    set_effects(&mut s, a, -10.0, -4.0, 0.0);
    set_detail(&mut s, a, 20.0);
    set_grading(&mut s, wheel(35.0, 8.0, 6.0), wheel(30.0, 4.0, 2.0), wheel(40.0, 6.0, 4.0), wheel(0.0, 0.0, 0.0));
    set_curve(&mut s, 0.0, 0.0, 0.0, 0.0);
    // Lifted blacks: the curve starts above black and ends just below white.
    s.curve.master = vec![
        Point::new(0.0, 0.08),
        Point::new(0.25, 0.31),
        Point::new(0.5, 0.55),
        Point::new(0.75, 0.79),
        Point::new(1.0, 0.97),
    ];
    set_finish(&mut s, -6.0, 18.0, 25.0, 65.0, 55.0, 0.0, 70.0, 0.0);
    s
}

fn noir_look(a: &Analysis, base: &DevelopSettings) -> DevelopSettings {
    let mut s = base.clone();
    set_light(&mut s, a, 0.46, 0.28, -10.0, 0.0);
    set_white_balance(&mut s, a, 0.0, 0.0);
    s.color.vibrance = 0.0;
    s.color.saturation = 0.0;
    set_effects(&mut s, a, 25.0, 15.0, a.haze * 40.0);
    set_detail(&mut s, a, 35.0);
    neutral_grading(&mut s);
    set_curve(&mut s, 15.0, 8.0, -8.0, -15.0);
    set_finish(&mut s, -12.0, 25.0, 40.0, 70.0, 40.0, 20.0, 45.0, 0.0);
    s.treatment = Treatment::Bw;
    s
}

fn soft_look(a: &Analysis, base: &DevelopSettings) -> DevelopSettings {
    let mut s = base.clone();
    set_light(&mut s, a, 0.48, 0.15, -10.0, 15.0);
    set_white_balance(&mut s, a, 0.12, 0.05);
    set_colour(&mut s, a, 0.30, 0.0);
    set_effects(&mut s, a, -18.0, -10.0, 0.0);
    set_detail(&mut s, a, 18.0);
    set_grading(&mut s, wheel(30.0, 6.0, 2.0), wheel(0.0, 0.0, 0.0), wheel(30.0, 8.0, 4.0), wheel(0.0, 0.0, 0.0));
    set_curve(&mut s, 5.0, 3.0, -2.0, 5.0);
    set_finish(&mut s, -8.0, 8.0, 30.0, 55.0, 55.0, 0.0, 65.0, 15.0);
    s
}

/// `base` with look `id` applied for this photo (at full strength; the recipe blends). An
/// unknown id returns `base` unchanged.
pub fn apply(id: &str, a: &Analysis, base: &DevelopSettings) -> DevelopSettings {
    match id {
        "clear" => clear_look(a, base),
        "golden" => golden_look(a, base),
        "cool" => cool_look(a, base),
        "vivid" => vivid_look(a, base),
        "matte" => matte_look(a, base),
        "noir" => noir_look(a, base),
        "soft" => soft_look(a, base),
        _ => base.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analysis(p50: f64) -> Analysis {
        Analysis { p50, mean: p50, ..Analysis::default() }
    }

    #[test]
    fn every_look_resolves_and_is_deterministic() {
        let a = analysis(0.45);
        let base = DevelopSettings::default();
        for l in LOOKS {
            assert!(look(l.id).is_some(), "{}", l.id);
            assert_eq!(apply(l.id, &a, &base), apply(l.id, &a, &base), "{}", l.id);
        }
        assert_eq!(LOOKS.len(), 7);
    }

    #[test]
    fn unknown_look_leaves_base_unchanged() {
        let base = DevelopSettings::default();
        assert_eq!(apply("nope", &analysis(0.45), &base), base);
    }

    #[test]
    fn looks_aim_for_a_result_not_a_fixed_offset() {
        let base = DevelopSettings::default();
        let dark = apply("clear", &analysis(0.2), &base);
        let bright = apply("clear", &analysis(0.7), &base);
        assert!(dark.light.exposure > bright.light.exposure, "{} {}", dark.light.exposure, bright.light.exposure);
        assert!(dark.light.exposure > 0.0 && bright.light.exposure < 0.0);
    }

    #[test]
    fn noir_is_black_and_white_and_matte_lifts_blacks() {
        let base = DevelopSettings::default();
        let noir = apply("noir", &analysis(0.45), &base);
        assert_eq!(noir.treatment, Treatment::Bw);
        assert_eq!(noir.color.saturation, 0.0);
        let matte = apply("matte", &analysis(0.45), &base);
        assert!(matte.curve.master.first().is_some_and(|p| p.y > 0.0), "master {:?}", matte.curve.master);
        let golden = apply("golden", &analysis(0.45), &base);
        assert!(golden.wb.temp > base.wb.temp);
        let cool = apply("cool", &analysis(0.45), &base);
        assert!(cool.wb.temp < base.wb.temp);
    }

    #[test]
    fn looks_set_custom_white_balance_anchored_on_neutral() {
        let base = DevelopSettings::default();
        // The anchor is neutral, not the incoming temperature: applying a
        // look over settings that already carry the Auto correction must
        // not add the correction a second time.
        let mut auto_base = base.clone();
        auto_base.wb.mode = WbMode::Custom;
        auto_base.wb.temp = 5960.0;
        let a = analysis(0.45);
        let over_neutral = apply("golden", &a, &base);
        let over_auto = apply("golden", &a, &auto_base);
        assert_eq!(over_neutral.wb.temp, over_auto.wb.temp);
        assert_eq!(over_neutral.wb.temp, 7100.0);
        assert_eq!(over_neutral.wb.mode, WbMode::Custom);
        for l in LOOKS {
            assert_eq!(apply(l.id, &a, &base).wb.mode, WbMode::Custom, "{}", l.id);
        }
    }

    #[test]
    fn dehaze_has_no_hidden_term() {
        let mut a = analysis(0.45);
        a.haze = 0.5;
        let base = DevelopSettings::default();
        // Matte and Soft pass 0.0 and must apply no dehaze at all.
        assert_eq!(apply("matte", &a, &base).effects.dehaze, 0.0);
        assert_eq!(apply("soft", &a, &base).effects.dehaze, 0.0);
        // Clear folds its own haze term in: 0.5 * 55.
        assert_eq!(apply("clear", &a, &base).effects.dehaze, 27.5);
    }

    #[test]
    fn clipped_photos_get_recovery_in_every_look() {
        let mut a = analysis(0.45);
        a.clipped_highlights = 0.2;
        a.clipped_shadows = 0.2;
        let base = DevelopSettings::default();
        for l in LOOKS {
            let s = apply(l.id, &a, &base);
            assert!(s.light.highlights < 0.0, "{} {}", l.id, s.light.highlights);
            assert!(s.light.shadows > 0.0, "{} {}", l.id, s.light.shadows);
            // Blacks recovery is the clipped fraction times 150, no offset.
            assert_eq!(s.light.blacks, 30.0, "{} {}", l.id, s.light.blacks);
        }
    }
}
