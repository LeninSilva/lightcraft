//! The Auto assistant: from a photo's [`Analysis`], an enhancement (develop settings) and
//! plain-language guidance grounded in photography practice.
//!
//! The enhancement aims for a result, not a fixed recipe: exposure follows the analysis'
//! exposure correction, recovery follows measured clipping, and white balance follows the
//! measured cast. Every tip's action is a real edit (a Tune slider, subject focus, a smart
//! crop, or a look), so tapping it changes the photo in the way the tip describes.

use serde::{Deserialize, Serialize};

use lightcraft_develop::DevelopSettings;

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

/// The Auto enhancement for this photo (what Auto at 100 % applies).
pub fn enhance(a: &Analysis) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.light.exposure = clamp(a.exposure_correction, -2.0, 2.0);
    s.light.contrast = clamp((0.22 - a.contrast) * 250.0, -40.0, 50.0);
    let range_bonus = if a.dynamic_range_ev > 10.0 { 15.0 } else { 0.0 };
    s.light.highlights = clamp(-a.clipped_highlights * 400.0 - range_bonus, -85.0, 0.0);
    s.light.shadows = clamp(a.clipped_shadows * 400.0 + range_bonus, 0.0, 85.0);
    s.light.whites = clamp(-a.clipped_highlights * 200.0, -50.0, 0.0);
    s.light.blacks = clamp(a.clipped_shadows * 150.0 - 10.0, -30.0, 40.0);
    // Neutralise the measured cast: warm photos cool down, cool photos warm up.
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

fn suggested_look(a: &Analysis) -> Option<&'static str> {
    Some(match a.scene {
        Scene::Portrait => "soft",
        Scene::Landscape | Scene::Backlit => "golden",
        Scene::LowKey | Scene::Monochrome => "noir",
        Scene::Food => "vivid",
        Scene::Night | Scene::HighKey => "clear",
        Scene::General => {
            if a.colorfulness >= 0.35 {
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
    let mut clauses: Vec<&str> = Vec::new();
    if a.exposure_correction >= 0.5 {
        clauses.push("underexposed");
    } else if a.exposure_correction <= -0.5 {
        clauses.push("overexposed");
    }
    if a.clipped_highlights >= 0.05 {
        clauses.push("clipped highlights");
    }
    if a.clipped_shadows >= 0.05 {
        clauses.push("crushed shadows");
    }
    if a.warmth >= 0.15 {
        clauses.push("a warm cast");
    } else if a.warmth <= -0.15 {
        clauses.push("a cool cast");
    }
    if a.haze >= 0.4 {
        clauses.push("haze");
    }
    if a.noise >= 0.3 {
        clauses.push("visible noise");
    }
    if a.subject.confidence >= 0.4 {
        clauses.push("a clear subject");
    }
    if a.contrast < 0.12 {
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
    if a.warmth >= 0.15 {
        tags.push("Warm".to_string());
    } else if a.warmth <= -0.15 {
        tags.push("Cool".to_string());
    }
    if a.mean < 0.25 {
        tags.push("Low light".to_string());
    } else if a.mean > 0.65 {
        tags.push("Bright".to_string());
    }
    if a.contrast >= 0.28 {
        tags.push("High contrast".to_string());
    } else if a.contrast < 0.12 {
        tags.push("Flat".to_string());
    }
    if a.haze >= 0.4 {
        tags.push("Hazy".to_string());
    }
    if a.noise >= 0.3 {
        tags.push("Noisy".to_string());
    }
    if a.colorfulness >= 0.35 {
        tags.push("Colourful".to_string());
    } else if a.colorfulness < 0.12 {
        tags.push("Muted".to_string());
    }
    if a.clipped_highlights >= 0.05 {
        tags.push("Blown highlights".to_string());
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
    if ev.abs() >= 0.3 {
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
    if a.clipped_highlights >= 0.05 {
        push_tip(
            &mut tips,
            tip(
                "highlights",
                "Recover the highlights",
                "Pull the brightest tones back so detail in the highlights comes back.".to_string(),
                TipAction::Tune("highlights", -60.0),
            ),
        );
    }
    if a.clipped_shadows >= 0.05 {
        push_tip(
            &mut tips,
            tip(
                "shadows",
                "Open the shadows",
                "Lift the darkest tones so detail in the shadows comes back.".to_string(),
                TipAction::Tune("shadows", 60.0),
            ),
        );
    }
    if a.haze >= 0.35 {
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
    if a.warmth.abs() >= 0.15 {
        push_tip(
            &mut tips,
            tip(
                "warmth",
                "Neutralise the colour cast",
                "Shift the white balance back toward neutral.".to_string(),
                TipAction::Tune("warmth", clamp(-a.warmth * 80.0, -100.0, 100.0)),
            ),
        );
    }
    if a.noise >= 0.25 {
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
    if a.subject.confidence >= 0.35 {
        push_tip(
            &mut tips,
            tip(
                "focus",
                "Bring out the subject",
                "Brighten and sharpen the subject, and quiet the background.".to_string(),
                TipAction::Focus(60.0),
            ),
        );
    }
    let off_centre = a.subject.cx < 0.32 || a.subject.cx > 0.68 || a.subject.cy < 0.32 || a.subject.cy > 0.68;
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
    if tips.is_empty() {
        push_tip(
            &mut tips,
            tip(
                "clarity",
                "Add a little clarity",
                "A touch of local contrast makes texture stand out.".to_string(),
                TipAction::Tune("clarity", 15.0),
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
        assert!(s.light.exposure > 0.0);
        assert!(s.light.highlights < 0.0 && s.light.shadows > 0.0);
        let advice = advise(&a);
        assert!(advice.summary.contains("clipped highlights"), "{}", advice.summary);
        assert_eq!(advice.tips[0].id, "exposure");
        assert!(matches!(advice.tips[0].action, Some(TipAction::Tune("exposure", v)) if (v - 1.2).abs() < 1e-9));
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
