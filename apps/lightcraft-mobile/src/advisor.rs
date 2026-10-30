//! The Auto assistant: from a photo's [`Analysis`], an enhancement (develop settings) and
//! plain-language guidance grounded in photography practice.
//!
//! The enhancement aims for a result, not a fixed recipe: exposure follows the analysis'
//! exposure correction, recovery follows measured clipping, and white balance follows the
//! measured cast. Every tip's action is a real edit (a Tune slider, subject focus, a smart
//! crop, or a look), so tapping it changes the photo in the way the tip describes.

use serde::{Deserialize, Serialize};

use lightcraft_develop::{DevelopSettings, WbMode};

use crate::analysis::{Analysis, Scene};
use crate::recipe::Recipe;

/// One piece of guidance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tip {
    /// Stable id (tests, analytics-free UI state).
    pub id: String,
    pub title: String,
    pub detail: String,
    /// A one-tap fix, applied to the current recipe; `None` for advice only.
    #[serde(skip)]
    pub action: Option<TipAction>,
}

/// What a tip's button does to the recipe.
#[derive(Clone, Debug, PartialEq)]
pub enum TipAction {
    /// Set a Tune slider (`recipe::tune_control` id) to a value.
    Tune(&'static str, f64),
    /// Turn subject focus on at this amount.
    Focus(f64),
    /// Crop to an aspect around the subject (rule of thirds).
    SmartCrop(crate::recipe::Aspect),
    /// Apply a look at a strength.
    Look(&'static str, f64),
}

impl TipAction {
    pub fn apply(&self, r: &mut Recipe) {
        match self {
            TipAction::Tune(id, v) => {
                if let Some(c) = crate::recipe::tune_control(id) {
                    (c.set)(&mut r.tune, v.clamp(c.min, c.max));
                }
            }
            TipAction::Focus(a) => r.focus.amount = a.clamp(0.0, 100.0),
            TipAction::SmartCrop(aspect) => {
                r.frame.aspect = *aspect;
                r.frame.smart = true;
            }
            TipAction::Look(id, s) => {
                r.look = Some((*id).to_string());
                r.look_strength = s.clamp(0.0, 100.0);
            }
        }
    }
}

/// The assistant's read of a photo.
#[derive(Clone, Debug, PartialEq)]
pub struct Advice {
    /// One sentence describing the photo ("A backlit portrait with deep shadows").
    pub summary: String,
    /// Up to three short tags ("Portrait", "Warm", "Low light").
    pub tags: Vec<String>,
    pub tips: Vec<Tip>,
    /// A look that suits the photo.
    pub suggested_look: Option<&'static str>,
}

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_finite() { v.clamp(lo, hi) } else { 0.0_f64.clamp(lo, hi) }
}

/// One shared threshold set for the summary, the tags, and the tips,
/// so the three never disagree about the same photo: the summary
/// must not call a photo underexposed that gets no exposure tip,
/// and a tip must not fire for a condition the summary omits.
const EXPOSURE_THRESHOLD: f64 = 0.3;
const CLIP_THRESHOLD: f64 = 0.05;
const CAST_THRESHOLD: f64 = 0.15;
const HAZE_THRESHOLD: f64 = 0.35;
const NOISE_THRESHOLD: f64 = 0.25;
const SUBJECT_THRESHOLD: f64 = 0.35;
const LOW_CONTRAST: f64 = 0.12;

/// The Auto enhancement for this photo (what Auto at 100 % applies).
pub fn enhance(a: &Analysis) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    // Exposure is the analysis' stored correction, computed in
    // linear light from the stored linear median. Reading it here
    // (rather than recomputing from the gamma-encoded p50) keeps
    // Auto and the exposure tip stating the same value.
    s.light.exposure = clamp(a.exposure_correction, -2.0, 2.0);
    s.light.contrast = clamp((0.22 - a.contrast) * 250.0, -40.0, 50.0);
    let range_bonus = if a.dynamic_range_ev > 10.0 { 15.0 } else { 0.0 };
    s.light.highlights = clamp(-a.clipped_highlights * 400.0 - range_bonus, -85.0, 0.0);
    s.light.shadows = clamp(a.clipped_shadows * 400.0 + range_bonus, 0.0, 85.0);
    s.light.whites = clamp(-a.clipped_highlights * 200.0, -50.0, 0.0);
    s.light.blacks = clamp(a.clipped_shadows * 150.0, -30.0, 40.0);
    // Neutralise the measured cast: warm photos cool down, cool photos warm up.
    // Setting temp/tint only takes effect with an explicit mode: the pipeline
    // returns the as-shot white whenever the mode is `AsShot` (the default),
    // ignoring temp/tint entirely.
    s.wb.mode = WbMode::Custom;
    s.wb.temp = clamp(6500.0 - a.warmth * 1800.0, 3500.0, 10_000.0);
    s.wb.tint = clamp(-a.tint * 120.0, -100.0, 100.0);
    if a.scene != Scene::Monochrome {
        s.color.vibrance = clamp((0.35 - a.saturation) * 150.0, -20.0, 40.0);
        s.color.saturation = clamp((0.32 - a.saturation) * 80.0, -15.0, 25.0);
    }
    s.effects.clarity = clamp(10.0 + (0.20 - a.contrast) * 50.0 - a.noise * 30.0, 0.0, 30.0);
    s.effects.texture = 8.0;
    s.effects.dehaze = clamp(a.haze * 60.0, 0.0, 50.0);
    s.detail.sharpen_amount = clamp(35.0 - a.noise * 150.0, 10.0, 50.0);
    s.detail.nr_luminance = clamp(a.noise * 80.0, 0.0, 60.0);
    s.detail.nr_color = clamp(a.noise * 40.0, 0.0, 30.0);
    let curve = clamp((0.20 - a.contrast) * 40.0, -10.0, 15.0);
    s.curve.highlights = curve;
    s.curve.lights = curve * 0.6;
    s.curve.darks = -curve * 0.6;
    s.curve.shadows = -curve;
    s
}

/// The look that suits this photo. Every look is reachable: the
/// scene arms come first, and for a General scene a cool cast
/// (warmth <= -0.15, overcast/shade) suggests Cool and flat
/// contrast (contrast < 0.12) suggests Matte. No scene returns
/// `None` by design, so the look tip in `build_tips` always fires
/// and the dead fallback tip it used to guard is gone.
fn suggested_look(a: &Analysis) -> Option<&'static str> {
    Some(match a.scene {
        Scene::Portrait => "soft",
        Scene::Landscape | Scene::Backlit => "golden",
        Scene::LowKey | Scene::Monochrome => "noir",
        Scene::Food => "vivid",
        Scene::Night | Scene::HighKey => "clear",
        Scene::General => {
            if a.warmth <= -CAST_THRESHOLD {
                "cool"
            } else if a.contrast < LOW_CONTRAST {
                "matte"
            } else if a.colorfulness >= 0.35 {
                "vivid"
            } else {
                "clear"
            }
        }
    })
}

fn summary_kind(scene: Scene) -> &'static str {
    match scene {
        Scene::Portrait => "portrait",
        Scene::Landscape => "landscape",
        Scene::Night => "night photo",
        Scene::HighKey => "bright photo",
        Scene::LowKey => "moody photo",
        Scene::Backlit => "backlit photo",
        Scene::Food => "food photo",
        Scene::Monochrome => "black and white photo",
        Scene::General => "photo",
    }
}

fn build_summary(a: &Analysis) -> String {
    // Parallel noun phrases throughout: no bare adjectives mixed
    // with noun phrases ("underexposed, a warm cast, haze").
    let mut clauses: Vec<&str> = Vec::new();
    if a.exposure_correction >= EXPOSURE_THRESHOLD {
        clauses.push("underexposure");
    } else if a.exposure_correction <= -EXPOSURE_THRESHOLD {
        clauses.push("overexposure");
    }
    if a.clipped_highlights >= CLIP_THRESHOLD {
        clauses.push("clipped highlights");
    }
    if a.clipped_shadows >= CLIP_THRESHOLD {
        clauses.push("crushed shadows");
    }
    if a.warmth >= CAST_THRESHOLD {
        clauses.push("a warm cast");
    } else if a.warmth <= -CAST_THRESHOLD {
        clauses.push("a cool cast");
    }
    if a.haze >= HAZE_THRESHOLD {
        clauses.push("haze");
    }
    if a.noise >= NOISE_THRESHOLD {
        clauses.push("visible noise");
    }
    if a.subject.confidence >= SUBJECT_THRESHOLD {
        clauses.push("a clear subject");
    }
    if a.contrast < LOW_CONTRAST {
        clauses.push("low contrast");
    }
    let kind = summary_kind(a.scene);
    if clauses.is_empty() {
        return format!("A {kind}.");
    }
    let clauses = &clauses[..clauses.len().min(3)];
    format!("A {kind} with {}.", clauses.join(", "))
}

fn build_tags(a: &Analysis) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    if a.scene != Scene::General {
        tags.push(a.scene.label().to_string());
    }
    if a.warmth >= CAST_THRESHOLD {
        tags.push("Warm".to_string());
    } else if a.warmth <= -CAST_THRESHOLD {
        tags.push("Cool".to_string());
    }
    if a.mean < 0.25 {
        tags.push("Low light".to_string());
    } else if a.mean > 0.65 {
        tags.push("Bright".to_string());
    }
    if a.contrast >= 0.28 {
        tags.push("High contrast".to_string());
    } else if a.contrast < LOW_CONTRAST {
        tags.push("Flat".to_string());
    }
    if a.haze >= HAZE_THRESHOLD {
        tags.push("Hazy".to_string());
    }
    if a.noise >= NOISE_THRESHOLD {
        tags.push("Noisy".to_string());
    }
    if a.colorfulness >= 0.35 {
        tags.push("Colourful".to_string());
    } else if a.colorfulness < 0.12 {
        tags.push("Muted".to_string());
    }
    if a.clipped_highlights >= CLIP_THRESHOLD {
        tags.push("Blown highlights".to_string());
    }
    if a.clipped_shadows >= CLIP_THRESHOLD {
        tags.push("Deep shadows".to_string());
    }
    tags.truncate(3);
    tags
}

fn tip(id: &str, title: &str, detail: String, action: TipAction) -> Tip {
    Tip { id: id.to_string(), title: title.to_string(), detail, action: Some(action) }
}

fn push_tip(tips: &mut Vec<Tip>, t: Tip) {
    if tips.len() < 3 {
        tips.push(t);
    }
}

fn build_tips(a: &Analysis, suggested: Option<&'static str>) -> Vec<Tip> {
    let mut tips: Vec<Tip> = Vec::new();
    let ev = clamp(a.exposure_correction, -2.0, 2.0);
    if ev.abs() >= EXPOSURE_THRESHOLD {
        let direction = if ev > 0.0 { "brighten" } else { "darken" };
        push_tip(
            &mut tips,
            tip(
                "exposure",
                "Fix the exposure",
                format!("Auto measured {ev:+.1} EV. Tap to {direction} the photo by that much."),
                TipAction::Tune("exposure", ev),
            ),
        );
    }
    if a.clipped_highlights >= CLIP_THRESHOLD {
        push_tip(
            &mut tips,
            tip(
                "highlights",
                "Recover the highlights",
                "Pull the brightest tones back so detail in the highlights comes back.".to_string(),
                // Measured recovery, mirroring `enhance()`: a fixed -60
                // would over-recover mild clipping and under-recover
                // severe clipping.
                TipAction::Tune("highlights", clamp(-a.clipped_highlights * 400.0, -85.0, 0.0)),
            ),
        );
    }
    if a.clipped_shadows >= CLIP_THRESHOLD {
        push_tip(
            &mut tips,
            tip(
                "shadows",
                "Open the shadows",
                "Lift the darkest tones so detail in the shadows comes back.".to_string(),
                TipAction::Tune("shadows", clamp(a.clipped_shadows * 400.0, 0.0, 85.0)),
            ),
        );
    }
    if a.contrast < LOW_CONTRAST {
        push_tip(
            &mut tips,
            tip(
                "contrast",
                "Add contrast",
                "Lift the photo out of flatness with more tonal range.".to_string(),
                // Mirrors `enhance()`: the same deficit, the same scale.
                TipAction::Tune("contrast", clamp((0.22 - a.contrast) * 250.0, -40.0, 50.0)),
            ),
        );
    }
    if a.haze >= HAZE_THRESHOLD {
        push_tip(
            &mut tips,
            tip(
                "dehaze",
                "Cut the haze",
                "Add contrast and depth to a flat, hazy photo.".to_string(),
                TipAction::Tune("dehaze", clamp(a.haze * 60.0, 0.0, 60.0)),
            ),
        );
    }
    if a.warmth.abs() >= CAST_THRESHOLD {
        push_tip(
            &mut tips,
            tip(
                "warmth",
                "Fix the colour temperature",
                "Shift the white balance temperature back toward neutral.".to_string(),
                TipAction::Tune("warmth", clamp(-a.warmth * 80.0, -100.0, 100.0)),
            ),
        );
    }
    if a.tint.abs() >= CAST_THRESHOLD {
        push_tip(
            &mut tips,
            tip(
                "tint",
                "Fix the colour tint",
                "Shift the white balance tint back toward neutral.".to_string(),
                TipAction::Tune("tint", clamp(-a.tint * 120.0, -100.0, 100.0)),
            ),
        );
    }
    if a.noise >= NOISE_THRESHOLD {
        push_tip(
            &mut tips,
            tip(
                "noise",
                "Reduce the noise",
                "Smooth the grain without losing the subject.".to_string(),
                TipAction::Tune("noise_reduction", clamp(a.noise * 80.0, 0.0, 80.0)),
            ),
        );
    }
    if a.subject.confidence >= SUBJECT_THRESHOLD {
        push_tip(
            &mut tips,
            tip(
                "focus",
                "Bring out the subject",
                // The focus mask brightens and adds clarity/texture to
                // the subject; it sets no sharpness, and it quiets the
                // background only when background focus is on, so the
                // tip promises neither.
                "Brighten the subject and bring out its texture.".to_string(),
                TipAction::Focus(60.0),
            ),
        );
    }
    let off_centre = a.subject.cx < 0.32 || a.subject.cx > 0.68 || a.subject.cy < 0.32 || a.subject.cy > 0.68;
    // The crop gate keeps its own lower confidence (0.2): placing a
    // subject needs less certainty than calling it clear.
    if off_centre && a.subject.confidence >= 0.2 {
        push_tip(
            &mut tips,
            tip(
                "crop",
                "Crop to the subject",
                "Place the subject on a third line with a 3:2 crop.".to_string(),
                TipAction::SmartCrop(crate::recipe::Aspect::Classic3x2),
            ),
        );
    }
    if let Some(id) = suggested {
        let name = crate::looks::look(id).map_or(id, |l| l.name);
        push_tip(
            &mut tips,
            tip(
                "look",
                "Try a look",
                format!("The {name} look suits this photo. Tap to apply it."),
                TipAction::Look(id, 70.0),
            ),
        );
    }
    tips
}

/// Guidance for this photo.
pub fn advise(a: &Analysis) -> Advice {
    let suggested = suggested_look(a);
    Advice { summary: build_summary(a), tags: build_tags(a), tips: build_tips(a, suggested), suggested_look: suggested }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_analysis_enhances_gently_and_advises_once() {
        let a = Analysis::default();
        let s = enhance(&a);
        assert_eq!(s.light.exposure, 0.0);
        // White balance changes render only with an explicit mode: the
        // pipeline ignores temp/tint while the mode is AsShot.
        assert_eq!(s.wb.mode, WbMode::Custom);
        assert_eq!(s.wb.temp, 6500.0);
        assert!(s.light.contrast.is_finite() && s.effects.clarity.is_finite() && s.detail.sharpen_amount.is_finite());
        let advice = advise(&a);
        assert!(advice.summary.ends_with('.') && advice.summary.matches('.').count() == 1, "{}", advice.summary);
        assert!(advice.tags.len() <= 3 && advice.tips.len() <= 3);
        assert_eq!(advice.tips.len(), 1);
        assert_eq!(advice.tips[0].id, "look");
        assert!(matches!(advice.tips[0].action, Some(TipAction::Look("clear", _))));
        let suggested = advice.suggested_look.expect("a look is suggested");
        assert!(crate::looks::look(suggested).is_some());
    }

    #[test]
    fn dark_clipped_photo_gets_exposure_and_recovery() {
        let mut a = Analysis::default();
        a.p50 = 0.1;
        a.exposure_correction = 1.2;
        a.clipped_highlights = 0.1;
        a.clipped_shadows = 0.1;
        let s = enhance(&a);
        // Exposure is the stored correction (computed in linear light
        // by `analyze`); the blacks recovery has no offset subtracted.
        assert_eq!(s.light.exposure, 1.2);
        assert_eq!(s.light.blacks, 15.0);
        assert!(s.light.highlights < 0.0 && s.light.shadows > 0.0);
        let advice = advise(&a);
        assert!(advice.summary.contains("clipped highlights"), "{}", advice.summary);
        assert_eq!(advice.tips[0].id, "exposure");
        assert!(matches!(advice.tips[0].action, Some(TipAction::Tune("exposure", v)) if (v - 1.2).abs() < 1e-9));
    }

    #[test]
    fn enhance_agrees_with_the_stored_correction_on_a_real_photo() {
        use crate::analysis::analyze;
        use lightcraft_raster::Rgb32f;
        let a = analyze(&Rgb32f::from_fn(16, 16, |_, _| [0.02, 0.02, 0.02]));
        let s = enhance(&a);
        assert!(a.exposure_correction > 0.0, "ev {}", a.exposure_correction);
        assert_eq!(s.light.exposure, a.exposure_correction.clamp(-2.0, 2.0));
    }

    #[test]
    fn highlight_and_shadow_tips_use_measured_clipping() {
        let mut mild = Analysis::default();
        mild.clipped_highlights = 0.05;
        mild.clipped_shadows = 0.05;
        let tips = advise(&mild).tips;
        assert!(matches!(tips[0].action, Some(TipAction::Tune("highlights", v)) if (v + 20.0).abs() < 1e-9));
        assert!(matches!(tips[1].action, Some(TipAction::Tune("shadows", v)) if (v - 20.0).abs() < 1e-9));
        let mut severe = Analysis::default();
        severe.clipped_highlights = 0.25;
        severe.clipped_shadows = 0.25;
        let tips = advise(&severe).tips;
        assert!(matches!(tips[0].action, Some(TipAction::Tune("highlights", v)) if (v + 85.0).abs() < 1e-9));
        assert!(matches!(tips[1].action, Some(TipAction::Tune("shadows", v)) if (v - 85.0).abs() < 1e-9));
    }

    #[test]
    fn contrast_and_tint_tips_fire_on_their_own_gates() {
        let mut a = Analysis::default();
        a.contrast = 0.05;
        a.tint = 0.3;
        let tips = advise(&a).tips;
        let ids: Vec<&str> = tips.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"contrast"), "{ids:?}");
        assert!(ids.contains(&"tint"), "{ids:?}");
        let contrast = tips.iter().find(|t| t.id == "contrast").expect("contrast tip");
        assert!(matches!(contrast.action, Some(TipAction::Tune("contrast", v)) if (v - 42.5).abs() < 1e-9));
        let tint = tips.iter().find(|t| t.id == "tint").expect("tint tip");
        assert!(matches!(tint.action, Some(TipAction::Tune("tint", v)) if (v + 36.0).abs() < 1e-9));
    }

    #[test]
    fn cool_and_matte_are_suggested_for_cast_and_flat_photos() {
        let mut cool = Analysis::default();
        cool.warmth = -0.2;
        assert_eq!(advise(&cool).suggested_look, Some("cool"));
        let mut flat = Analysis::default();
        flat.contrast = 0.05;
        assert_eq!(advise(&flat).suggested_look, Some("matte"));
    }

    #[test]
    fn summary_uses_noun_phrases_and_tags_name_deep_shadows() {
        let mut a = Analysis::default();
        a.exposure_correction = 0.6;
        let summary = advise(&a).summary;
        assert!(summary.contains("underexposure"), "{summary}");
        assert!(!summary.contains("underexposed"), "{summary}");
        let mut b = Analysis::default();
        b.clipped_shadows = 0.1;
        assert!(advise(&b).tags.contains(&"Deep shadows".to_string()), "{:?}", advise(&b).tags);
    }

    #[test]
    fn advisor_is_deterministic() {
        let a = Analysis::default();
        assert_eq!(enhance(&a), enhance(&a));
        assert_eq!(advise(&a), advise(&a));
    }

    #[test]
    fn tip_actions_change_the_recipe() {
        let mut r = Recipe::default();
        TipAction::Tune("exposure", 0.7).apply(&mut r);
        assert_eq!(r.tune.exposure, 0.7);
        TipAction::Focus(60.0).apply(&mut r);
        assert_eq!(r.focus.amount, 60.0);
        TipAction::SmartCrop(crate::recipe::Aspect::Square).apply(&mut r);
        assert_eq!(r.frame.aspect, crate::recipe::Aspect::Square);
        assert!(r.frame.smart);
        TipAction::Look("golden", 70.0).apply(&mut r);
        assert_eq!(r.look.as_deref(), Some("golden"));
        assert_eq!(r.look_strength, 70.0);
    }

    #[test]
    fn suggested_look_follows_the_scene() {
        let mut a = Analysis::default();
        a.scene = Scene::Portrait;
        assert_eq!(advise(&a).suggested_look, Some("soft"));
        a.scene = Scene::Monochrome;
        assert_eq!(advise(&a).suggested_look, Some("noir"));
    }
}
