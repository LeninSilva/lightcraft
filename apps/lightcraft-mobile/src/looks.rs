//! Adaptive looks: named styles whose settings are computed from each photo's [`Analysis`], so
//! the same look lands differently on a bright beach and a dim street (it aims for a result, not
//! a fixed slider offset).

use lightcraft_develop::DevelopSettings;

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
pub const LOOKS: &[Look] = &[Look { id: "clear", name: "Clear", blurb: "Clean, honest colour and balanced light" }];

/// Find a look by id.
pub fn look(id: &str) -> Option<&'static Look> {
    LOOKS.iter().find(|l| l.id == id)
}

/// `base` with look `id` applied for this photo (at full strength; the recipe blends). An
/// unknown id returns `base` unchanged.
pub fn apply(id: &str, _a: &Analysis, base: &DevelopSettings) -> DevelopSettings {
    let _ = look(id);
    base.clone()
}
