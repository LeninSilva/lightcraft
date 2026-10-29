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
//!
//! Display values are an approximation, not a pipeline render: linear Rec.2020 pixels are
//! converted to linear sRGB with the colour-space matrix from `lightcraft_color` and encoded
//! with the sRGB transfer function. That is enough for the statistics here, and it keeps
//! analysis independent of any develop settings.

use serde::{Deserialize, Serialize};

use lightcraft_color::{Mat3, REC2020, SRGB};
use lightcraft_raster::Rgb32f;

/// Side of the saliency grid (cells per long edge).
pub const SALIENCY_GRID: usize = 24;
/// Bins of a cell's luminance histogram (5 bits of entropy at most).
const CELL_BINS: usize = 32;
/// Bins of the global luminance histogram (8 bits of entropy at most).
const HIST_BINS: usize = 256;

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

fn clamp01(v: f64) -> f64 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

/// A linear channel value a damaged decode can be trusted with: finite, non-negative, bounded.
fn clean_channel(v: f32) -> f64 {
    if v.is_finite() { f64::from(v).clamp(0.0, 64.0) } else { 0.0 }
}

fn srgb_encode(c: f64) -> f64 {
    if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// Linear Rec.2020 pixel to display sRGB (0..1 per channel).
fn to_display(p: [f32; 3], matrix: &Mat3) -> [f64; 3] {
    let linear = matrix.apply([clean_channel(p[0]), clean_channel(p[1]), clean_channel(p[2])]);
    [clamp01(srgb_encode(linear[0])), clamp01(srgb_encode(linear[1])), clamp01(srgb_encode(linear[2]))]
}

fn luma709(rgb: [f64; 3]) -> f64 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

/// HSV saturation and hue in degrees (0 when the pixel is grey).
fn saturation_and_hue(rgb: [f64; 3]) -> (f64, f64) {
    let (mx, mn) = (rgb[0].max(rgb[1]).max(rgb[2]), rgb[0].min(rgb[1]).min(rgb[2]));
    let chroma = mx - mn;
    let sat = if mx <= 0.0 { 0.0 } else { chroma / mx };
    if chroma <= 1e-9 {
        return (sat, 0.0);
    }
    let hue = if mx == rgb[0] {
        60.0 * (rgb[1] - rgb[2]) / chroma
    } else if mx == rgb[1] {
        60.0 * (rgb[2] - rgb[0]) / chroma + 120.0
    } else {
        60.0 * (rgb[0] - rgb[1]) / chroma + 240.0
    };
    (sat, hue.rem_euclid(360.0))
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let pos = clamp01(q) * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let frac = pos - lo as f64;
    let a = sorted.get(lo).copied().unwrap_or_default();
    let b = sorted.get(hi).copied().unwrap_or_default();
    a + (b - a) * frac
}

fn median_of(sorted: &[f64]) -> f64 {
    percentile(sorted, 0.5)
}

/// Per-pixel values plus the accumulators one pass over them fills.
struct Collected {
    display: Vec<[f64; 3]>,
    luma: Vec<f64>,
    linear: Vec<f64>,
    hist: [u32; HIST_BINS],
    mean_rgb: [f64; 3],
    rg_sum: f64,
    rg_sq: f64,
    yb_sum: f64,
    yb_sq: f64,
    sat_sum: f64,
    neutral_rb: f64,
    neutral_tint: f64,
    neutral_n: u64,
    skin_n: u64,
    foliage_n: u64,
}

fn collect(img: &Rgb32f, total: usize, matrix: &Mat3) -> Collected {
    let mut c = Collected {
        display: Vec::with_capacity(total),
        luma: Vec::with_capacity(total),
        linear: Vec::with_capacity(total),
        hist: [0; HIST_BINS],
        mean_rgb: [0.0; 3],
        rg_sum: 0.0,
        rg_sq: 0.0,
        yb_sum: 0.0,
        yb_sq: 0.0,
        sat_sum: 0.0,
        neutral_rb: 0.0,
        neutral_tint: 0.0,
        neutral_n: 0,
        skin_n: 0,
        foliage_n: 0,
    };
    for i in 0..total {
        let raw = img.data.get(i).copied().unwrap_or_default();
        let linear_rgb = [clean_channel(raw[0]), clean_channel(raw[1]), clean_channel(raw[2])];
        let linear = 0.2627 * linear_rgb[0] + 0.6780 * linear_rgb[1] + 0.0593 * linear_rgb[2];
        let rgb = to_display(raw, matrix);
        let luma = luma709(rgb);
        let (sat, hue) = saturation_and_hue(rgb);
        let mx = rgb[0].max(rgb[1]).max(rgb[2]);
        c.hist[(luma.clamp(0.0, 1.0) * (HIST_BINS - 1) as f64) as usize] += 1;
        c.mean_rgb[0] += rgb[0];
        c.mean_rgb[1] += rgb[1];
        c.mean_rgb[2] += rgb[2];
        let rg = rgb[0] - rgb[1];
        let yb = 0.5 * (rgb[0] + rgb[1]) - rgb[2];
        c.rg_sum += rg;
        c.rg_sq += rg * rg;
        c.yb_sum += yb;
        c.yb_sq += yb * yb;
        c.sat_sum += sat;
        if sat < 0.08 && (0.15..0.85).contains(&luma) {
            c.neutral_rb += rgb[0] - rgb[2];
            c.neutral_tint += 0.5 * (rgb[0] + rgb[2]) - rgb[1];
            c.neutral_n += 1;
        }
        if (10.0..45.0).contains(&hue) && (0.2..0.7).contains(&sat) && (0.3..0.95).contains(&mx) && rgb[0] > rgb[1] && rgb[1] >= rgb[2] {
            c.skin_n += 1;
        }
        if (70.0..170.0).contains(&hue) && sat > 0.15 && mx > 0.15 && rgb[1] >= rgb[0] && rgb[1] >= rgb[2] {
            c.foliage_n += 1;
        }
        c.display.push(rgb);
        c.luma.push(luma);
        c.linear.push(linear);
    }
    c
}

/// High-pass of luma (each pixel minus the mean of its existing neighbours), plus the
/// mean absolute high-pass (detail) and its median absolute deviation (noise).
fn high_pass(width: usize, height: usize, luma: &[f64]) -> (Vec<f64>, f64, f64) {
    let total = luma.len();
    let mut hp = vec![0.0; total];
    let mut abs_sum = 0.0;
    for y in 0..height {
        for x in 0..width {
            let i = y.saturating_mul(width).saturating_add(x);
            let here = luma.get(i).copied().unwrap_or_default();
            let mut sum = 0.0;
            let mut n = 0.0;
            for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
                if nx < width && ny < height {
                    sum += luma.get(ny.saturating_mul(width).saturating_add(nx)).copied().unwrap_or_default();
                    n += 1.0;
                }
            }
            let v = if n > 0.0 { here - sum / n } else { 0.0 };
            hp[i] = v;
            abs_sum += v.abs();
        }
    }
    let detail = if total > 0 { abs_sum / total as f64 } else { 0.0 };
    let mut sorted = hp.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let med = median_of(&sorted);
    let mut devs: Vec<f64> = hp.iter().map(|v| (v - med).abs()).collect();
    devs.sort_by(|a, b| a.total_cmp(b));
    (hp, detail, clamp01(median_of(&devs)))
}

#[derive(Clone, Copy, Default)]
struct Cell {
    count: u32,
    hist: [u32; CELL_BINS],
    rgb_sum: [f64; 3],
}

fn grid_dims(width: usize, height: usize) -> (usize, usize) {
    let long = width.max(height).max(1) as f64;
    let gw = ((width as f64 * SALIENCY_GRID as f64 / long).round() as usize).clamp(1, SALIENCY_GRID);
    let gh = ((height as f64 * SALIENCY_GRID as f64 / long).round() as usize).clamp(1, SALIENCY_GRID);
    (gw, gh)
}

/// Saliency per grid cell, normalized so the strongest cell is 1. A cell scores for local
/// luminance entropy, for how rare its colour is, and for being near the centre.
fn saliency_grid(width: usize, height: usize, luma: &[f64], display: &[[f64; 3]], mean_rgb: [f64; 3]) -> (usize, usize, Vec<f32>) {
    let (gw, gh) = grid_dims(width, height);
    let mut cells = vec![Cell::default(); gw.saturating_mul(gh)];
    for y in 0..height {
        for x in 0..width {
            let i = y.saturating_mul(width).saturating_add(x);
            let cell = cells.get_mut((y * gh / height.max(1)).saturating_mul(gw).saturating_add(x * gw / width.max(1)));
            if let Some(cell) = cell {
                let l = luma.get(i).copied().unwrap_or_default();
                cell.hist[(l.clamp(0.0, 1.0) * (CELL_BINS - 1) as f64) as usize] += 1;
                let rgb = display.get(i).copied().unwrap_or([0.0; 3]);
                cell.rgb_sum[0] += rgb[0];
                cell.rgb_sum[1] += rgb[1];
                cell.rgb_sum[2] += rgb[2];
                cell.count += 1;
            }
        }
    }
    let mut raw = vec![0.0; cells.len()];
    let mut peak = 0.0;
    for (idx, cell) in cells.iter().enumerate() {
        let cx = idx % gw;
        let cy = idx / gw;
        let dx = (cx as f64 + 0.5) / gw as f64 - 0.5;
        let dy = (cy as f64 + 0.5) / gh as f64 - 0.45;
        let prior = (-(dx * dx + dy * dy) / (2.0 * 0.42 * 0.42)).exp();
        let (entropy, rarity) = if cell.count == 0 {
            (0.0, 0.0)
        } else {
            let n = f64::from(cell.count);
            let mean = [cell.rgb_sum[0] / n, cell.rgb_sum[1] / n, cell.rgb_sum[2] / n];
            let dist = ((mean[0] - mean_rgb[0]).powi(2) + (mean[1] - mean_rgb[1]).powi(2) + (mean[2] - mean_rgb[2]).powi(2)).sqrt() / 3.0f64.sqrt();
            (shannon_entropy(&cell.hist) / (CELL_BINS as f64).log2(), clamp01(dist))
        };
        let v = 0.45 * entropy + 0.35 * rarity + 0.20 * prior;
        raw[idx] = v;
        peak = peak.max(v);
    }
    let saliency: Vec<f32> = raw.iter().map(|v| if peak > 0.0 { (v / peak) as f32 } else { 0.0 }).collect();
    (gw, gh, saliency)
}

fn subject_from(saliency: &[f32], gw: usize, gh: usize) -> Subject {
    let peak = saliency.iter().copied().fold(0.0f32, f32::max);
    if peak <= 0.0 || gw == 0 || gh == 0 {
        return Subject::default();
    }
    let threshold = 0.65 * peak;
    let mut w_sum = 0.0;
    let mut cx_sum = 0.0;
    let mut cy_sum = 0.0;
    let mut strong_sum = 0.0;
    let mut strong_n = 0usize;
    let mut rest_sum = 0.0;
    let mut rest_n = 0usize;
    for (idx, &s) in saliency.iter().enumerate() {
        let x = ((idx % gw) as f64 + 0.5) / gw as f64;
        let y = ((idx / gw) as f64 + 0.5) / gh as f64;
        if s >= threshold {
            let w = f64::from(s / peak).powi(2);
            w_sum += w;
            cx_sum += w * x;
            cy_sum += w * y;
            strong_sum += f64::from(s);
            strong_n += 1;
        } else {
            rest_sum += f64::from(s);
            rest_n += 1;
        }
    }
    if w_sum <= 0.0 || strong_n == 0 {
        return Subject::default();
    }
    let cx = clamp01(cx_sum / w_sum);
    let cy = clamp01(cy_sum / w_sum);
    let mut var_x = 0.0;
    let mut var_y = 0.0;
    for (idx, &s) in saliency.iter().enumerate() {
        if s >= threshold {
            let w = f64::from(s / peak).powi(2);
            let x = ((idx % gw) as f64 + 0.5) / gw as f64;
            let y = ((idx / gw) as f64 + 0.5) / gh as f64;
            var_x += w * (x - cx).powi(2);
            var_y += w * (y - cy).powi(2);
        }
    }
    let rx = (2.0 * (var_x / w_sum).sqrt() + 0.5 / gw as f64).clamp(0.05, 0.5);
    let ry = (2.0 * (var_y / w_sum).sqrt() + 0.5 / gh as f64).clamp(0.05, 0.5);
    let strong_mean = strong_sum / strong_n as f64;
    let rest_mean = if rest_n > 0 { rest_sum / rest_n as f64 } else { 0.0 };
    Subject { cx, cy, rx, ry, confidence: clamp01(strong_mean - rest_mean) }
}

fn sky_fraction(width: usize, height: usize, luma: &[f64], display: &[[f64; 3]], hp: &[f64]) -> f64 {
    let mut sky_n = 0u64;
    let mut total = 0u64;
    for y in 0..height {
        if y.saturating_mul(3) >= height {
            break;
        }
        for x in 0..width {
            let i = y.saturating_mul(width).saturating_add(x);
            total += 1;
            let l = luma.get(i).copied().unwrap_or_default();
            let rgb = display.get(i).copied().unwrap_or([0.0; 3]);
            let (sat, _) = saturation_and_hue(rgb);
            let blueish = rgb[2] >= rgb[0] - 0.02 && rgb[2] >= rgb[1] - 0.05;
            let smooth = hp.get(i).copied().unwrap_or_default().abs() < 0.04;
            if l > 0.45 && (blueish || sat < 0.15) && smooth {
                sky_n += 1;
            }
        }
    }
    if total == 0 { 0.0 } else { sky_n as f64 / total as f64 }
}

fn classify(a: &Analysis) -> Scene {
    let subject_area = a.subject.rx * a.subject.ry;
    if a.colorfulness < 0.08 && a.saturation < 0.08 {
        Scene::Monochrome
    } else if a.p50 < 0.12 && a.mean < 0.18 {
        Scene::Night
    } else if a.clipped_highlights >= 0.05 && a.clipped_shadows >= 0.05 && a.mean < 0.5 {
        Scene::Backlit
    } else if a.p05 > 0.55 && a.mean > 0.65 {
        Scene::HighKey
    } else if a.p95 < 0.35 && a.mean < 0.3 {
        Scene::LowKey
    } else if a.skin >= 0.07 && a.subject.confidence >= 0.15 && subject_area < 0.3 {
        Scene::Portrait
    } else if (a.sky >= 0.25 || a.foliage >= 0.25) && a.width >= a.height {
        Scene::Landscape
    } else if a.saturation >= 0.4 && a.warmth >= 0.08 && a.foliage < 0.1 && a.sky < 0.1 && a.subject.confidence >= 0.15 {
        Scene::Food
    } else {
        Scene::General
    }
}

fn key_target(scene: Scene) -> f64 {
    match scene {
        Scene::Night => 0.22,
        Scene::LowKey => 0.30,
        Scene::HighKey => 0.60,
        Scene::Backlit => 0.42,
        _ => 0.45,
    }
}

/// Analyze a working image (linear Rec.2020). Never panics, whatever the size (an empty image
/// gives [`Analysis::default`]).
pub fn analyze(img: &Rgb32f) -> Analysis {
    let (width, height) = (img.width, img.height);
    if width == 0 || height == 0 || img.data.is_empty() {
        return Analysis { width, height, ..Analysis::default() };
    }
    let Some(total) = width.checked_mul(height) else {
        return Analysis { width, height, ..Analysis::default() };
    };
    let matrix = REC2020.to_space(&SRGB);
    let c = collect(img, total, &matrix);
    let n = total as f64;
    let mut luma_sorted = c.luma.clone();
    luma_sorted.sort_by(|a, b| a.total_cmp(b));
    let mut linear_sorted = c.linear.clone();
    linear_sorted.sort_by(|a, b| a.total_cmp(b));
    let mean = c.luma.iter().sum::<f64>() / n;
    let var = c.luma.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    let contrast = var.max(0.0).sqrt();
    let clipped_highlights = c.luma.iter().filter(|&&v| v >= 0.985).count() as f64 / n;
    let clipped_shadows = c.luma.iter().filter(|&&v| v <= 0.015).count() as f64 / n;
    let lo = percentile(&linear_sorted, 0.005).max(1e-4);
    let hi = percentile(&linear_sorted, 0.995).max(lo);
    let dynamic_range_ev = (hi / lo).log2().clamp(0.0, 16.0);
    let mean_rg = c.rg_sum / n;
    let mean_yb = c.yb_sum / n;
    let std_rg = (c.rg_sq / n - mean_rg * mean_rg).max(0.0).sqrt();
    let std_yb = (c.yb_sq / n - mean_yb * mean_yb).max(0.0).sqrt();
    let colorfulness = (std_rg.powi(2) + std_yb.powi(2)).sqrt() + 0.3 * (mean_rg.powi(2) + mean_yb.powi(2)).sqrt();
    let neutral = c.neutral_n as f64;
    let (warmth, tint) = if neutral > 0.0 {
        ((c.neutral_rb / neutral * 2.5).clamp(-1.0, 1.0), (c.neutral_tint / neutral * 2.5).clamp(-1.0, 1.0))
    } else {
        (0.0, 0.0)
    };
    let mean_rgb = [c.mean_rgb[0] / n, c.mean_rgb[1] / n, c.mean_rgb[2] / n];
    let (hp, detail, noise) = high_pass(width, height, &c.luma);
    let (grid_w, grid_h, saliency) = saliency_grid(width, height, &c.luma, &c.display, mean_rgb);
    let subject = subject_from(&saliency, grid_w, grid_h);
    let sky = sky_fraction(width, height, &c.luma, &c.display, &hp);
    let haze = 0.4 * clamp01((0.20 - contrast) / 0.20) + 0.3 * clamp01((percentile(&luma_sorted, 0.01) - 0.02) / 0.15) + 0.3 * clamp01((0.25 - c.sat_sum / n) / 0.25);
    let mut a = Analysis {
        width,
        height,
        p01: percentile(&luma_sorted, 0.01),
        p05: percentile(&luma_sorted, 0.05),
        p50: percentile(&luma_sorted, 0.5),
        p95: percentile(&luma_sorted, 0.95),
        p99: percentile(&luma_sorted, 0.99),
        mean,
        contrast,
        clipped_highlights,
        clipped_shadows,
        dynamic_range_ev,
        exposure_correction: 0.0,
        saturation: c.sat_sum / n,
        colorfulness,
        warmth,
        tint,
        skin: c.skin_n as f64 / n,
        sky,
        foliage: c.foliage_n as f64 / n,
        noise,
        detail,
        entropy: shannon_entropy(&c.hist),
        haze: clamp01(haze),
        grid_w,
        grid_h,
        saliency,
        subject,
        scene: Scene::General,
    };
    a.scene = classify(&a);
    let mut ev = (key_target(a.scene) / a.p50.max(0.02)).log2();
    if a.clipped_highlights > 0.10 {
        ev = ev.min(-0.25);
    }
    a.exposure_correction = ev.clamp(-2.0, 2.0);
    a
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

    #[test]
    fn uniform_grey_has_no_entropy_or_contrast() {
        let img = Rgb32f::from_fn(32, 32, |_, _| [0.5, 0.5, 0.5]);
        let a = analyze(&img);
        assert_eq!(a.entropy, 0.0);
        assert_eq!(a.contrast, 0.0);
        assert!((a.p50 - 0.735).abs() < 0.01, "p50 {}", a.p50);
        assert_eq!(a.clipped_highlights, 0.0);
        assert_eq!(a.clipped_shadows, 0.0);
        // No texture anywhere, so the subject is just the centre prior's peak.
        assert!((0.4..0.6).contains(&a.subject.cx));
        assert!(a.subject.confidence < 0.6, "confidence {}", a.subject.confidence);
    }

    #[test]
    fn centre_prior_beats_corners_on_uniform_texture() {
        // Every cell holds the same two tones in the same proportions, so only the
        // centre prior separates them.
        let img = Rgb32f::from_fn(48, 48, |x, y| {
            let v = if (x + y) % 2 == 0 { 0.2 } else { 0.8 };
            [v, v, v]
        });
        let a = analyze(&img);
        assert_eq!(a.saliency.len(), a.grid_w * a.grid_h);
        let corner = f64::from(a.saliency[0]);
        let centre = f64::from(a.saliency[(a.grid_h / 2) * a.grid_w + a.grid_w / 2]);
        assert!(centre > corner, "centre {centre} corner {corner}");
        assert!((0.35..0.65).contains(&a.subject.cx));
        assert!((0.30..0.60).contains(&a.subject.cy));
    }

    #[test]
    fn tiny_and_odd_sizes_do_not_panic() {
        for (w, h) in [(1, 1), (3, 5), (1, 17), (17, 1), (2, 3)] {
            let a = analyze(&Rgb32f::from_fn(w, h, |x, y| [x as f32 * 0.1, y as f32 * 0.1, 0.3]));
            assert_eq!((a.width, a.height), (w, h));
            assert!(a.grid_w <= SALIENCY_GRID && a.grid_h <= SALIENCY_GRID);
            assert_eq!(a.saliency.len(), a.grid_w * a.grid_h);
            assert!((0.0..=1.0).contains(&a.subject.cx) && (0.0..=1.0).contains(&a.subject.cy));
            assert!(a.p50.is_finite() && a.entropy.is_finite());
        }
    }

    #[test]
    fn short_data_and_non_finite_pixels_do_not_panic() {
        let short = Rgb32f { width: 4, height: 4, data: vec![[1.0, 1.0, 1.0]] };
        let a = analyze(&short);
        assert_eq!((a.width, a.height), (4, 4));
        assert!(a.mean.is_finite());
        let bad = Rgb32f::from_fn(4, 4, |_, _| [f32::NAN, f32::INFINITY, f32::NEG_INFINITY]);
        let b = analyze(&bad);
        assert!(b.mean.is_finite() && b.entropy.is_finite());
        assert_eq!(b.dynamic_range_ev, 0.0);
    }

    #[test]
    fn black_image_needs_brightening_and_has_no_range() {
        let a = analyze(&Rgb32f::from_fn(8, 8, |_, _| [0.0, 0.0, 0.0]));
        assert_eq!(a.dynamic_range_ev, 0.0);
        assert!(a.exposure_correction > 0.0, "ev {}", a.exposure_correction);
        // Colourless before dark in the classification order: pure black is monochrome.
        assert_eq!(a.scene, Scene::Monochrome);
    }
}
