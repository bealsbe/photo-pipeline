//! A photo's preview as the models' input (plan §5.1 `prep`): the JPG's
//! MPF preview (else its own image at a DCT-reduced size), a RAW-only
//! shot's smallest embedded preview of at least 300 px (1 000 px when
//! faces are wanted, §7.3), decoded at the largest DCT reduction that
//! still covers the input, oriented as the photo is shown, squashed to
//! [`INPUT_SIZE`] square (what SigLIP2's preprocessing does, so the whole
//! frame is seen) and normalised to mean 0.5, std 0.5 in CHW order.
//!
//! With faces wanted, the same decode (at least ~1 000 px on its long
//! side, filling the detector's canvas) also fills that canvas, and the
//! preview's bytes are kept so the faces found can be cut from the full
//! preview afterwards ([`FacePrep::full_pixels`]), one read per photo.
//!
//! The same read gives the content key (size and first 64 KB), the
//! capture info and whether the frame looks accidental (the analysis's
//! own rule on the same luma), so each photo is read once.

use std::path::Path;

use chrono::NaiveDateTime;

use super::client::FileError;
use super::face::{self, Canvas};
use crate::exif;
use crate::imaging::decode::{self, Pixels};
use crate::imaging::libraw::{Raw, Thumbnail};
use crate::imaging::quality::{self, Plane, Source};
use crate::model::hints;
use crate::model::record::{FileType, ShotMeta};
use crate::search::index::Key;

/// The model's input edge, in pixels.
pub const INPUT_SIZE: u32 = 256;
/// Smaller previews aren't worth embedding.
pub const PREVIEW_MIN_EDGE: u32 = 300;
/// A RAW-only shot's preview for faces: at least this long (§7.3).
pub const FACE_PREVIEW_MIN_EDGE: u32 = 1000;
/// The decode for the detector keeps at least this on the long side, so
/// the canvas (`face::DET_SIZE`) is filled rather than upscaled into.
pub const DETECT_MIN_LONG_EDGE: u32 = 1000;

/// Everything one read of a photo gives the index.
#[derive(Clone)]
pub struct Prepared {
    pub key: Key,
    pub capture: Option<NaiveDateTime>,
    pub meta: ShotMeta,
    pub accidental: bool,
    /// `3 × INPUT_SIZE × INPUT_SIZE` floats, CHW, normalised.
    pub input: Vec<f32>,
    /// The face detector's canvas and the way back to the full preview,
    /// when faces were asked for.
    pub faces: Option<FacePrep>,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared").field("key", &self.key).field("capture", &self.capture).field("accidental", &self.accidental).field("faces", &self.faces.is_some()).finish()
    }
}

/// What the face step needs from a photo.
#[derive(Clone)]
pub struct FacePrep {
    pub canvas: Canvas,
    /// The oriented preview's size: the boxes are fractions of it.
    pub width: u32,
    pub height: u32,
    source: FaceSource,
}

#[derive(Clone)]
enum FaceSource {
    /// The preview's JPEG bytes, decoded whole and oriented when faces
    /// are found.
    Jpeg { data: Vec<u8>, orientation: u8 },
    /// Already pixels (a RAW's bitmap thumbnail).
    Pixels(Pixels),
}

impl FacePrep {
    /// The full oriented preview, for cutting faces out (a second decode
    /// of bytes already in memory; no read).
    pub fn full_pixels(&self) -> Option<Pixels> {
        match &self.source {
            FaceSource::Jpeg { data, orientation } => decode::decode_jpeg_full_scaled(data, 1).ok().map(|px| decode::apply_orientation(px, *orientation)),
            FaceSource::Pixels(px) => Some(px.clone()),
        }
    }
}

/// Read and prepare `path`; with `faces`, the detector's canvas too.
pub fn prepare(path: &Path, file_type: FileType, faces: bool) -> Result<Prepared, FileError> {
    let size = std::fs::metadata(path).map_err(|e| FileError::io(&e))?.len();
    match file_type {
        FileType::Jpg => prepare_jpeg(path, size, faces),
        FileType::Raw => prepare_raw(path, size, faces),
    }
}

fn prepare_jpeg(path: &Path, size: u64, faces: bool) -> Result<Prepared, FileError> {
    // The MPF preview, with the header it was found in (≥ 64 KB unless the
    // file is shorter): the key and the capture info come from that.
    if let Some((data, head)) = exif::jpeg_preview_bytes(path, PREVIEW_MIN_EDGE) {
        if decode::jpeg_dimensions(&data).is_some_and(|(w, h)| w.min(h) >= INPUT_SIZE) {
            let key = Key::of(size, &head);
            let (capture, meta) = exif::shot_from_jpeg_head(&head, true);
            let orientation = exif::orientation_from_jpeg_head(&head);
            return finish(data, orientation, Source::Mpf, key, capture, meta, faces);
        }
    }
    // No usable preview: the image itself, at a reduced size.
    let data = read_all(path)?;
    let key = Key::of(size, &data);
    let (capture, meta) = exif::shot_from_jpeg_head(&data[..data.len().min(256 * 1024)], true);
    let orientation = exif::orientation_from_jpeg_head(&data);
    finish(data, orientation, Source::Primary, key, capture, meta, faces)
}

fn prepare_raw(path: &Path, size: u64, faces: bool) -> Result<Prepared, FileError> {
    let head = {
        use std::io::Read;
        let mut f = exif::open_for_header(path).map_err(|e| FileError::io(&e))?;
        let mut buf = Vec::with_capacity(super::super::search::index::KEY_HEAD_BYTES);
        f.by_ref().take(super::super::search::index::KEY_HEAD_BYTES as u64).read_to_end(&mut buf).map_err(|e| FileError::io(&e))?;
        buf
    };
    let key = Key::of(size, &head);
    let (capture, meta) = exif::read_shot(path, true);
    let mut raw = Raw::open(path).map_err(|e| FileError::other(format!("RAW: {e}")))?;
    let flip = raw.exif_orientation();
    let min_edge = if faces { FACE_PREVIEW_MIN_EDGE } else { PREVIEW_MIN_EDGE };
    match raw.thumbnail_at_least(min_edge).map_err(|e| FileError::other(format!("RAW preview: {e}")))? {
        Thumbnail::Jpeg(bytes) => {
            let orientation = crate::imaging::thumbs::embedded_orientation(&bytes, flip);
            finish(bytes, orientation, Source::RawPreview, key, capture, meta, faces)
        }
        Thumbnail::Bitmap { width, height, colors, data } => {
            let px = crate::imaging::thumbs::bitmap(width, height, colors, data).ok_or_else(|| FileError::other("RAW preview is not readable"))?;
            let px = decode::apply_orientation(px, flip);
            let accidental = accidental(&px, 1, Source::RawPreview);
            let face_prep = faces.then(|| FacePrep { canvas: face::canvas(&px), width: px.width, height: px.height, source: FaceSource::Pixels(px.clone()) });
            Ok(Prepared { key, capture, meta, accidental, input: normalise(&px), faces: face_prep })
        }
    }
}

/// Decode a JPEG preview at the largest DCT reduction keeping both edges
/// at least [`INPUT_SIZE`] (with faces, the long edge at least
/// [`DETECT_MIN_LONG_EDGE`]), orient it, judge it and normalise it.
fn finish(jpeg: Vec<u8>, orientation: u8, source: Source, key: Key, capture: Option<NaiveDateTime>, meta: ShotMeta, faces: bool) -> Result<Prepared, FileError> {
    let (w, h) = decode::jpeg_dimensions(&jpeg).ok_or_else(|| FileError::other("not a readable JPEG"))?;
    // The search input and the accident rule work on a small decode (a
    // DCT reduction is nearly free); the detector's canvas, when wanted,
    // on a second, larger one.  Measuring accidents on the large decode
    // cost more than the detector itself.
    let denom = [8, 4, 2].into_iter().find(|&d| w.min(h) / d >= INPUT_SIZE).unwrap_or(1);
    let px = decode::decode_jpeg_full_scaled(&jpeg, denom).map_err(FileError::other)?;
    let accidental = accidental(&px, orientation, source);
    let px = decode::apply_orientation(px, orientation);
    let input = normalise(&px);
    let face_prep = if faces {
        let big_denom = [8, 4, 2].into_iter().find(|&d| w.max(h) / d >= DETECT_MIN_LONG_EDGE).unwrap_or(1);
        let big = if big_denom == denom { px } else { decode::apply_orientation(decode::decode_jpeg_full_scaled(&jpeg, big_denom).map_err(FileError::other)?, orientation) };
        // The full preview's oriented size, which the decoded size scales
        // to (the DCT reduction rounds up).
        let swap = orientation >= 5;
        let (fw, fh) = if swap { (h, w) } else { (w, h) };
        Some(FacePrep { canvas: face::canvas(&big), width: fw, height: fh, source: FaceSource::Jpeg { data: jpeg, orientation } })
    } else {
        None
    };
    Ok(Prepared { key, capture, meta, accidental, input, faces: face_prep })
}

/// The analysis's own accident rule (black, blank, flat, featureless
/// frames; a lit subject against the dark is a photo), on this preview's
/// luma.  Without the analysis's look at neighbouring frames, so it errs
/// towards keeping: only frames with nothing in them are left out.
fn accidental(px: &Pixels, orientation: u8, source: Source) -> bool {
    let luma = decode::to_luma(px);
    let f = quality::measure(Plane::of(&luma), orientation, None, source);
    hints::accident(&f).is_some()
}

fn read_all(path: &Path) -> Result<Vec<u8>, FileError> {
    std::fs::read(path).map_err(|e| FileError::io(&e))
}

/// RGB pixels (any size) → the model's normalised CHW input: squashed to
/// [`INPUT_SIZE`] square, each channel `(v / 255 − 0.5) / 0.5`.
pub fn normalise(rgb: &Pixels) -> Vec<f32> {
    let px = decode::resize_to(rgb, INPUT_SIZE, INPUT_SIZE);
    let n = (INPUT_SIZE * INPUT_SIZE) as usize;
    let c = px.channels as usize;
    let mut out = vec![0f32; 3 * n];
    for (i, p) in px.data.chunks_exact(c).enumerate() {
        for ch in 0..3 {
            let v = p[if c >= 3 { ch } else { 0 }] as f32;
            out[ch * n + i] = v / 127.5 - 1.0;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalised_input_is_chw_in_minus_one_to_one() {
        let mut data = vec![0u8; 4 * 2 * 3];
        data[0..3].copy_from_slice(&[255, 0, 128]);
        let px = Pixels::new(4, 2, 3, data);
        let x = normalise(&px);
        let n = (INPUT_SIZE * INPUT_SIZE) as usize;
        assert_eq!(x.len(), 3 * n);
        assert!(x.iter().all(|v| (-1.0..=1.0).contains(v)));
        assert!(x[..n].iter().sum::<f32>() > x[n..2 * n].iter().sum::<f32>());
        // Greyscale input is repeated across the three channels.
        let g = normalise(&Pixels::new(2, 2, 1, vec![0, 255, 255, 0]));
        assert_eq!(g[..n], g[n..2 * n]);
    }

    #[test]
    fn a_plain_jpeg_is_prepared_from_its_own_image_and_a_black_one_is_accidental() {
        let d = std::env::temp_dir().join(format!("pp-prep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let img = crate::imaging::quality::tests::texture(640, 480, 5);
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 90).encode(&img.d, 640, 480, jpeg_encoder::ColorType::Luma).unwrap();
        std::fs::write(d.join("a.jpg"), &out).unwrap();
        let p = prepare(&d.join("a.jpg"), FileType::Jpg, false).expect("prepared");
        assert_eq!(p.input.len(), 3 * (INPUT_SIZE * INPUT_SIZE) as usize);
        assert_eq!(p.key, Key::of(out.len() as u64, &out));
        assert!(!p.accidental, "a textured frame is a photo");
        assert!(p.faces.is_none());
        // With faces: the canvas at the photo's own size (640 fits), and
        // the full preview back from the kept bytes.
        let f = prepare(&d.join("a.jpg"), FileType::Jpg, true).expect("prepared");
        let fp = f.faces.as_ref().expect("a face prep");
        assert_eq!((fp.width, fp.height), (640, 480));
        assert_eq!(fp.canvas.scale, 1.0);
        let full = fp.full_pixels().expect("decodes");
        assert_eq!((full.width, full.height), (640, 480));
        assert_eq!(f.input.len(), p.input.len());
        // A black frame is an accident; its key differs.
        let mut black = Vec::new();
        jpeg_encoder::Encoder::new(&mut black, 90).encode(&vec![0u8; 640 * 480], 640, 480, jpeg_encoder::ColorType::Luma).unwrap();
        std::fs::write(d.join("black.jpg"), &black).unwrap();
        let b = prepare(&d.join("black.jpg"), FileType::Jpg, false).expect("prepared");
        assert!(b.accidental);
        assert_ne!(b.key, p.key);
        assert!(b.input.iter().all(|&v| v <= -0.9));
        // Not a JPEG at all.
        std::fs::write(d.join("bad.jpg"), b"nope").unwrap();
        let e = prepare(&d.join("bad.jpg"), FileType::Jpg, false).unwrap_err();
        assert!(e.errno.is_none() && !e.message.is_empty());
        let gone = prepare(&d.join("missing.jpg"), FileType::Jpg, false).unwrap_err();
        assert_eq!(gone.errno, Some(libc::ENOENT));
        let _ = std::fs::remove_dir_all(&d);
    }
}
