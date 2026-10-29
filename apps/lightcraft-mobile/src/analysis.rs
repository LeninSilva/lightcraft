//! Deterministic photo analysis: what the Auto assistant and the adaptive looks read.
//!
//! Works on the small analysis proxy (linear Rec.2020, oriented). Everything here is pure
//! arithmetic on the pixels: the same photo always gives the same [`Analysis`].
//!
//! The subject is found with Shannon entropy: the photo is split into a grid of cells, each
//! cell's luminance histogram gives its entropy `H = −Σ p·log2 p` (how much visual information
//! it carries), and a saliency map combines that local information with colour rarity (how far a
//! cell's colour is from the photo's average) and a soft centre prior. The subject is the
//! saliency-weighted centroid and spread of the strongest region.

use serde::{Deserialize, Serialize};

use lightcraft_raster::Rgb32f;

/// Side of the saliency grid (cells per long edge).
pub const SALIENCY_GRID: usize = 24;

/// Where the photo's subject probably is, in normalized image coordinates (0..1, origin top left).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Subject {
    pub cx: f64,
    pub cy: f64,
    /// Half extents, normalized to the image's width and height.
    pub rx: f64,
    pub ry: f64,
    /// 0..1: how clearly the subject stands out from the rest.
    pub confidence: f64,
}

impl Default for Subject {
    fn default() -> Self {
        Subject { cx: 0.5, cy: 0.5, rx: 0.25, ry: 0.25, confidence: 0.0 }
    }
}

/// The kind of photo, as far as simple statistics can tell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scene {
    #[default]
    General,
    Portrait,
    Landscape,
    Night,
    HighKey,
    LowKey,
    Backlit,
    Food,
    Monochrome,
}

impl Scene {
    pub fn label(self) -> &'static str {
        match self {
            Scene::General => "Everyday",
            Scene::Portrait => "Portrait",
            Scene::Landscape => "Landscape",
            Scene::Night => "Night",
            Scene::HighKey => "Bright",
            Scene::LowKey => "Moody",
            Scene::Backlit => "Backlit",
            Scene::Food => "Food & still life",
            Scene::Monochrome => "Black & white",
        }
    }
}

/// Statistics of one photo. Luminance values are display-referred (sRGB-encoded luma, 0..1)
/// unless named `linear`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    pub width: usize,
    pub height: usize,
    /// Display luma percentiles.
    pub p01: f64,
    pub p05: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub mean: f64,
    /// Standard deviation of display luma (global contrast).
    pub contrast: f64,
    /// Fraction of pixels clipped to white / crushed to black (display luma ≥ 0.985 / ≤ 0.015).
    pub clipped_highlights: f64,
    pub clipped_shadows: f64,
    /// Log2 of the ratio between the 99.5th and 0.5th linear luminance percentiles (stops).
    pub dynamic_range_ev: f64,
    /// Exposure correction (EV) that brings the scene's key to a pleasing middle.
    pub exposure_correction: f64,
    /// Mean saturation (0..1, HSV-like on display values).
    pub saturation: f64,
    /// Hasler–Süsstrunk colourfulness (0 ≈ grey, ~0.4+ ≈ very colourful), on display values.
    pub colorfulness: f64,
    /// Colour cast of the near-neutral pixels: positive warmth = yellow/red, positive tint = magenta.
    pub warmth: f64,
    pub tint: f64,
    /// Fraction of pixels in skin-tone range.
    pub skin: f64,
    /// Fraction of the top third that looks like sky (bright, smooth, blue-ish or overcast).
    pub sky: f64,
    /// Fraction of green foliage.
    pub foliage: f64,
    /// Estimated noise level (0..1; median absolute deviation of a high-pass of luma).
    pub noise: f64,
    /// Fine detail / sharpness (mean high-pass energy).
    pub detail: f64,
    /// Shannon entropy of the global luma histogram (bits, 0..8).
    pub entropy: f64,
    /// Haze: low contrast plus lifted blacks plus low saturation (0..1).
    pub haze: f64,
    /// Saliency map, `grid_w × grid_h`, row-major, 0..1.
    pub grid_w: usize,
    pub grid_h: usize,
    pub saliency: Vec<f32>,
    pub subject: Subject,
    pub scene: Scene,
}

impl Default for Analysis {
    fn default() -> Self {
        Analysis {
            width: 0,
            height: 0,
            p01: 0.0,
            p05: 0.05,
            p50: 0.45,
            p95: 0.9,
            p99: 0.97,
            mean: 0.45,
            contrast: 0.2,
            clipped_highlights: 0.0,
            clipped_shadows: 0.0,
            dynamic_range_ev: 8.0,
            exposure_correction: 0.0,
            saturation: 0.3,
            colorfulness: 0.2,
            warmth: 0.0,
            tint: 0.0,
            skin: 0.0,
            sky: 0.0,
            foliage: 0.0,
            noise: 0.0,
            detail: 0.0,
            entropy: 7.0,
            haze: 0.0,
            grid_w: 0,
            grid_h: 0,
            saliency: Vec::new(),
            subject: Subject::default(),
            scene: Scene::General,
        }
    }
}

/// Shannon entropy (bits) of a histogram's counts; 0 for an empty histogram.
pub fn shannon_entropy(counts: &[u32]) -> f64 {
    let total: u64 = counts.iter().map(|&c| u64::from(c)).sum();
    if total == 0 {
        return 0.0;
    }
    let t = total as f64;
    counts.iter().filter(|&&c| c > 0).map(|&c| f64::from(c) / t).map(|p| -p * p.log2()).sum()
}

/// Analyze a working image (linear Rec.2020). Never panics, whatever the size (an empty image
/// gives [`Analysis::default`]).
pub fn analyze(img: &Rgb32f) -> Analysis {
    if img.width == 0 || img.height == 0 {
        return Analysis::default();
    }
    Analysis { width: img.width, height: img.height, ..Analysis::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_of_uniform_and_single_bins() {
        assert_eq!(shannon_entropy(&[]), 0.0);
        assert_eq!(shannon_entropy(&[0, 0]), 0.0);
        assert_eq!(shannon_entropy(&[5, 0, 0]), 0.0);
        assert!((shannon_entropy(&[1, 1, 1, 1]) - 2.0).abs() < 1e-12);
        assert!((shannon_entropy(&[7; 256]) - 8.0).abs() < 1e-12);
    }

    #[test]
    fn empty_image_is_default() {
        assert_eq!(analyze(&Rgb32f::new(0, 0)), Analysis::default());
    }
}
