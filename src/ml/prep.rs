//! A photo's preview as the model's input (plan §5.1 `prep`): the JPG's
//! MPF preview (else its own image at a DCT-reduced size), a RAW-only
//! shot's smallest embedded preview of at least 300 px, decoded at the
//! largest DCT reduction that still covers the input, oriented as the
//! photo is shown, squashed to [`INPUT_SIZE`] square (what SigLIP2's
//! preprocessing does, so the whole frame is seen) and normalised to
//! mean 0.5, std 0.5 in CHW order.
//!
//! The same read gives the content key (size and first 64 KB) and the
//! capture info, so each photo is read once.

use std::path::Path;

use chrono::NaiveDateTime;

use crate::imaging::decode::Pixels;
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

/// Read and prepare `path`.  Errors are plain words (the pass classifies
/// I/O errors by their kind through the message's errno text where it
/// can; the helper passes `io::Error`s through unchanged).
pub fn prepare(_path: &Path, _file_type: FileType) -> Result<Prepared, String> {
    Err("preview preparation isn't built yet".into())
}

/// RGB pixels (any size) → the model's normalised CHW input: squashed to
/// [`INPUT_SIZE`] square, each channel `(v / 255 − 0.5) / 0.5`.
pub fn normalise(rgb: &Pixels) -> Vec<f32> {
    let px = crate::imaging::decode::resize_to(rgb, INPUT_SIZE, INPUT_SIZE);
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
        // Black everywhere else: the red channel's mean is small but
        // positive where the bright pixel was stretched.
        assert!(x[..n].iter().sum::<f32>() > x[n..2 * n].iter().sum::<f32>());
        // Greyscale input is repeated across the three channels.
        let g = normalise(&Pixels::new(2, 2, 1, vec![0, 255, 255, 0]));
        assert_eq!(g[..n], g[n..2 * n]);
    }
}
