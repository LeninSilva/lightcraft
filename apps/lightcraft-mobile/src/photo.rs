//! Decoding, previews and full-resolution export: the pipeline glue.
//!
//! A [`Photo`] keeps two working copies of the opened file, both already in EXIF orientation and
//! in the pipeline's linear Rec.2020: a `preview` (long edge [`PREVIEW_LONG_EDGE`]) every slider
//! renders from, and a small `proxy` the analysis reads. Develop settings are resolution
//! independent, so the full-resolution export (which decodes the original again) matches what the
//! preview showed.

use std::sync::Arc;

use lightcraft_codecs::{ChromaSubsampling, DecodeOptions, EncodeImage, EncodeMeta};
use lightcraft_develop::DevelopSettings;
use lightcraft_geom::Orientation;
use lightcraft_pipeline::{Quality, RenderRequest, Rendered, SourceInfo};
use lightcraft_raster::Rgb32f;
use lightcraft_raster::resample::{Filter, fit};

/// Long edge of the interactive preview.
pub const PREVIEW_LONG_EDGE: u32 = 1600;
/// Long edge of the analysis proxy.
pub const PROXY_LONG_EDGE: u32 = 384;
/// Largest photo we open (pixels). Phones top out around 200 MP; a full-resolution render of that
/// is ~2.4 GB of float pixels, past what a WebView's 4 GB wasm memory can hold twice.
pub const MAX_PIXELS: u64 = 120_000_000;
/// Default JPEG quality of saved versions.
pub const JPEG_QUALITY: u8 = 92;

/// Something went wrong opening, rendering or saving; the text is shown to the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhotoError(pub String);

impl std::fmt::Display for PhotoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PhotoError {}

impl From<lightcraft_codecs::Error> for PhotoError {
    fn from(e: lightcraft_codecs::Error) -> Self {
        PhotoError(friendly_codec_error(&e))
    }
}

fn friendly_codec_error(e: &lightcraft_codecs::Error) -> String {
    match e {
        lightcraft_codecs::Error::UnknownFormat => "This file isn't a photo LightCraft can open (try JPEG, PNG, WebP or TIFF).".into(),
        other => format!("Couldn't open this photo: {other}"),
    }
}

/// An opened photo.
#[derive(Clone)]
pub struct Photo {
    /// Full-resolution size, oriented.
    pub width: u32,
    pub height: u32,
    /// Interactive working copy, oriented, linear Rec.2020.
    pub preview: Arc<Rgb32f>,
    /// Small copy for analysis.
    pub proxy: Rgb32f,
    pub info: SourceInfo,
}

fn oriented_dims(w: u32, h: u32, o: Orientation) -> (u32, u32) {
    if o.swaps_axes() { (h, w) } else { (w, h) }
}

/// Decode `bytes` fitting within `long_edge` (or at full size with `None`), oriented.
fn decode_oriented(bytes: &[u8], long_edge: Option<u32>) -> Result<(Rgb32f, u32, u32), PhotoError> {
    let header = lightcraft_codecs::read_header(bytes)?;
    let pixels = u64::from(header.width).saturating_mul(u64::from(header.height));
    if pixels > MAX_PIXELS {
        return Err(PhotoError(format!(
            "This photo is {} megapixels; LightCraft Mobile opens photos up to {} megapixels.",
            pixels / 1_000_000,
            MAX_PIXELS / 1_000_000
        )));
    }
    let opts = DecodeOptions { max_size: long_edge.map(|e| (e, e)), max_pixels: MAX_PIXELS };
    let decoded = lightcraft_codecs::decode(bytes, opts)?;
    let o = Orientation::from_exif(decoded.orientation);
    let (w, h) = oriented_dims(decoded.source_width, decoded.source_height, o);
    let image = decoded.to_working().oriented(o);
    if image.width == 0 || image.height == 0 {
        return Err(PhotoError("This photo has no pixels.".into()));
    }
    Ok((image, w, h))
}

impl Photo {
    /// Open an encoded photo (JPEG, PNG, WebP, TIFF, …).
    pub fn open(bytes: &[u8]) -> Result<Photo, PhotoError> {
        let (preview, width, height) = decode_oriented(bytes, Some(PREVIEW_LONG_EDGE))?;
        Ok(Photo::from_preview(preview, width, height))
    }

    /// A photo from an already decoded, oriented working image (demo scenes, tests).
    pub fn from_image(image: Rgb32f) -> Photo {
        let (w, h) = (image.width as u32, image.height as u32);
        let long = PREVIEW_LONG_EDGE as usize;
        let preview = if image.width.max(image.height) > long { fit(&image, long, long, Filter::Lanczos3) } else { image };
        Photo::from_preview(preview, w, h)
    }

    fn from_preview(preview: Rgb32f, width: u32, height: u32) -> Photo {
        let p = PROXY_LONG_EDGE as usize;
        let proxy = fit(&preview, p, p, Filter::Bilinear);
        Photo { width, height, preview: Arc::new(preview), proxy, info: SourceInfo::default() }
    }

    /// Render the preview to fit `max_w × max_h` display pixels.
    pub fn render(&self, s: &DevelopSettings, max_w: usize, max_h: usize, quality: Quality) -> Rendered {
        let req = RenderRequest { quality, ..RenderRequest::fit(max_w.max(1), max_h.max(1)) };
        lightcraft_pipeline::render(&self.preview, &self.info, s, &req)
    }

    /// Render a small thumbnail of the analysis proxy (look previews).
    pub fn render_proxy(&self, s: &DevelopSettings, max_edge: usize) -> Rendered {
        let req = RenderRequest { quality: Quality::Draft, ..RenderRequest::fit(max_edge.max(1), max_edge.max(1)) };
        lightcraft_pipeline::render(&self.proxy, &self.info, s, &req)
    }
}

/// Output size of a full-resolution render of a `w × h` photo with `s` (after crop and rotation).
pub fn export_size(w: u32, h: u32, s: &DevelopSettings) -> (u32, u32) {
    let (ow, oh) = lightcraft_pipeline::native_output_size(w as usize, h as usize, s);
    (ow.round().max(1.0) as u32, oh.round().max(1.0) as u32)
}

/// Render the original `bytes` at full resolution with `s` and encode a JPEG.
/// A failed full-resolution decode is an error, never a silent fall back to
/// the small proxy: the caller reports the failure instead of saving a
/// low-resolution file the user believes is full resolution.
pub fn export_jpeg(bytes: &[u8], s: &DevelopSettings, quality: u8) -> Result<Vec<u8>, PhotoError> {
    let (full, w, h) =
        decode_oriented(bytes, None).map_err(|e| PhotoError(format!("the original could not be decoded: {e}")))?;
    let (ow, oh) = export_size(w, h, s);
    let req = RenderRequest::fit(ow as usize, oh as usize);
    let rendered = lightcraft_pipeline::render(&full, &SourceInfo::default(), s, &req);
    drop(full);
    encode_jpeg(&rendered, quality)
}

/// Encode a rendered image as JPEG (sRGB, 4:4:4 at high quality).
pub fn encode_jpeg(r: &Rendered, quality: u8) -> Result<Vec<u8>, PhotoError> {
    let sub = if quality >= 90 { ChromaSubsampling::S444 } else { ChromaSubsampling::S420 };
    lightcraft_codecs::encode_jpeg(&EncodeImage::rgba8(&r.image), quality.clamp(1, 100), sub, &EncodeMeta::default()).map_err(PhotoError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(w: usize, h: usize) -> Rgb32f {
        let s = lightcraft_scenes::demo_library().into_iter().next().map(|s| s.render(w, h));
        s.unwrap_or_else(|| Rgb32f::new(w, h))
    }

    #[test]
    fn preview_and_proxy_fit() {
        let p = Photo::from_image(scene(2400, 1600));
        assert_eq!((p.width, p.height), (2400, 1600));
        assert_eq!(p.preview.width.max(p.preview.height), PREVIEW_LONG_EDGE as usize);
        assert_eq!(p.proxy.width.max(p.proxy.height), PROXY_LONG_EDGE as usize);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(Photo::open(b"not a photo").is_err());
        assert!(Photo::open(&[]).is_err());
        assert!(Photo::open(&[0xff, 0xd8, 0xff, 0xe0, 0, 0]).is_err());
    }

    #[test]
    fn jpeg_round_trip_and_full_res_export() {
        let p = Photo::from_image(scene(640, 480));
        let r = p.render(&DevelopSettings::default(), 640, 480, Quality::Full);
        let jpeg = encode_jpeg(&r, 90).unwrap();
        let back = Photo::open(&jpeg).unwrap();
        assert_eq!((back.width, back.height), (640, 480));
        let mut s = DevelopSettings::default();
        s.light.exposure = 0.5;
        let out = export_jpeg(&jpeg, &s, 90).unwrap();
        let again = Photo::open(&out).unwrap();
        assert_eq!((again.width, again.height), (640, 480));
    }
}

#[cfg(test)]
mod timing {
    use super::*;

    /// `cargo test -p lightcraft-mobile --release -- --ignored --nocapture timing`
    #[test]
    #[ignore]
    fn preview_render_timing() {
        let img = lightcraft_scenes::demo_library().into_iter().next().map(|s| s.render(2400, 1600)).unwrap();
        let p = Photo::from_image(img);
        let mut s = DevelopSettings::default();
        s.light.exposure = 0.3;
        s.effects.clarity = 20.0;
        for q in [Quality::Draft, Quality::Full] {
            let t = std::time::Instant::now();
            let _ = p.render(&s, 1600, 1600, q);
            eprintln!("{q:?}: {:?}", t.elapsed());
        }
        let t = std::time::Instant::now();
        let _ = p.render_proxy(&s, 160);
        eprintln!("proxy thumb: {:?}", t.elapsed());
    }
}
