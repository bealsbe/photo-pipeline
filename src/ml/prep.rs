//! A photo's preview as the model's input (plan §5.1 `prep`): the JPG's
//! MPF preview (else its own image at a DCT-reduced size), a RAW-only
//! shot's smallest embedded preview of at least 300 px, decoded at the
//! largest DCT reduction that still covers the input, oriented as the
//! photo is shown, squashed to [`INPUT_SIZE`] square (what SigLIP2's
//! preprocessing does, so the whole frame is seen) and normalised to
//! mean 0.5, std 0.5 in CHW order.
//!
//! The same read gives the content key (size and first 64 KB), the
//! capture info and whether the frame looks accidental (the analysis's
//! own rule on the same luma), so each photo is read once.

use std::path::Path;

use chrono::NaiveDateTime;

use super::client::FileError;
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

/// Everything one read of a photo gives the index.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub key: Key,
    pub capture: Option<NaiveDateTime>,
    pub meta: ShotMeta,
    pub accidental: bool,
    /// `3 × INPUT_SIZE × INPUT_SIZE` floats, CHW, normalised.
    pub input: Vec<f32>,
}

/// Read and prepare `path`.
pub fn prepare(path: &Path, file_type: FileType) -> Result<Prepared, FileError> {
    let size = std::fs::metadata(path).map_err(|e| FileError::io(&e))?.len();
    match file_type {
        FileType::Jpg => prepare_jpeg(path, size),
        FileType::Raw => prepare_raw(path, size),
    }
}

fn prepare_jpeg(path: &Path, size: u64) -> Result<Prepared, FileError> {
    // The MPF preview, with the header it was found in (≥ 64 KB unless the
    // file is shorter): the key and the capture info come from that.
    if let Some((data, head)) = exif::jpeg_preview_bytes(path, PREVIEW_MIN_EDGE) {
        if decode::jpeg_dimensions(&data).is_some_and(|(w, h)| w.min(h) >= INPUT_SIZE) {
            let key = Key::of(size, &head);
            let (capture, meta) = exif::shot_from_jpeg_head(&head, true);
            let orientation = exif::orientation_from_jpeg_head(&head);
            return finish(&data, orientation, Source::Mpf, key, capture, meta);
        }
    }
    // No usable preview: the image itself, at a reduced size.
    let data = read_all(path)?;
    let key = Key::of(size, &data);
    let (capture, meta) = exif::shot_from_jpeg_head(&data[..data.len().min(256 * 1024)], true);
    let orientation = exif::orientation_from_jpeg_head(&data);
    finish(&data, orientation, Source::Primary, key, capture, meta)
}

fn prepare_raw(path: &Path, size: u64) -> Result<Prepared, FileError> {
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
    match raw.thumbnail_at_least(PREVIEW_MIN_EDGE).map_err(|e| FileError::other(format!("RAW preview: {e}")))? {
        Thumbnail::Jpeg(bytes) => {
            let orientation = crate::imaging::thumbs::embedded_orientation(&bytes, flip);
            finish(&bytes, orientation, Source::RawPreview, key, capture, meta)
        }
        Thumbnail::Bitmap { width, height, colors, data } => {
            let px = crate::imaging::thumbs::bitmap(width, height, colors, data).ok_or_else(|| FileError::other("RAW preview is not readable"))?;
            let px = decode::apply_orientation(px, flip);
            Ok(Prepared { key, capture, meta, accidental: accidental(&px, flip, Source::RawPreview), input: normalise(&px) })
        }
    }
}

/// Decode a JPEG preview at the largest DCT reduction keeping both edges
/// at least [`INPUT_SIZE`], orient it, judge it and normalise it.
fn finish(jpeg: &[u8], orientation: u8, source: Source, key: Key, capture: Option<NaiveDateTime>, meta: ShotMeta) -> Result<Prepared, FileError> {
    let (w, h) = decode::jpeg_dimensions(jpeg).ok_or_else(|| FileError::other("not a readable JPEG"))?;
    let denom = [8, 4, 2].into_iter().find(|&d| w.min(h) / d >= INPUT_SIZE).unwrap_or(1);
    let px = decode::decode_jpeg_full_scaled(jpeg, denom).map_err(FileError::other)?;
    let accidental = accidental(&px, orientation, source);
    let px = decode::apply_orientation(px, orientation);
    Ok(Prepared { key, capture, meta, accidental, input: normalise(&px) })
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
        let p = prepare(&d.join("a.jpg"), FileType::Jpg).expect("prepared");
        assert_eq!(p.input.len(), 3 * (INPUT_SIZE * INPUT_SIZE) as usize);
        assert_eq!(p.key, Key::of(out.len() as u64, &out));
        assert!(!p.accidental, "a textured frame is a photo");
        // A black frame is an accident; its key differs.
        let mut black = Vec::new();
        jpeg_encoder::Encoder::new(&mut black, 90).encode(&vec![0u8; 640 * 480], 640, 480, jpeg_encoder::ColorType::Luma).unwrap();
        std::fs::write(d.join("black.jpg"), &black).unwrap();
        let b = prepare(&d.join("black.jpg"), FileType::Jpg).expect("prepared");
        assert!(b.accidental);
        assert_ne!(b.key, p.key);
        assert!(b.input.iter().all(|&v| v <= -0.9));
        // Not a JPEG at all.
        std::fs::write(d.join("bad.jpg"), b"nope").unwrap();
        let e = prepare(&d.join("bad.jpg"), FileType::Jpg).unwrap_err();
        assert!(e.errno.is_none() && !e.message.is_empty());
        let gone = prepare(&d.join("missing.jpg"), FileType::Jpg).unwrap_err();
        assert_eq!(gone.errno, Some(libc::ENOENT));
        let _ = std::fs::remove_dir_all(&d);
    }
}
