//! The Auto assistant: from a photo's [`Analysis`], an enhancement (develop settings) and
//! plain-language guidance grounded in photography practice.

use serde::{Deserialize, Serialize};

use lightcraft_develop::DevelopSettings;

use crate::analysis::Analysis;
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

/// The Auto enhancement for this photo (what Auto at 100 % applies).
pub fn enhance(_a: &Analysis) -> DevelopSettings {
    DevelopSettings::default()
}

/// Guidance for this photo.
pub fn advise(a: &Analysis) -> Advice {
    Advice { summary: format!("{} photo", a.scene.label()), tags: Vec::new(), tips: Vec::new(), suggested_look: None }
}
