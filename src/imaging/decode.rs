//! Pixel decoding and the few transforms the app needs: EXIF orientation,
//! cropping, high-quality resizing and conversion to RGBA for the GPU.
//!
//! JPEGs go through libjpeg-turbo (native/jpeg_shim.c), the SIMD decoder Qt
//! used for the Python app, including its DCT-domain 1/2, 1/4 and 1/8
//! scaling for thumbnails.

use std::ffi::{c_int, c_uchar, c_ulong};

use fast_image_resize::{self as fr, PixelType, ResizeAlg, ResizeOptions, Resizer, images::Image as FrImage, images::ImageRef};
use rayon::prelude::*;

/// 8-bit interleaved pixels: 1 (gray), 3 (RGB) or 4 (RGBA) channels.
#[derive(Clone)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub data: Vec<u8>,
}

impl Pixels {
    pub fn new(width: u32, height: u32, channels: u8, data: Vec<u8>) -> Self {
        debug_assert_eq!(data.len(), width as usize * height as usize * channels as usize);
        Self { width, height, channels, data }
    }
}

unsafe extern "C" {
    fn pp_jpeg_decode(
        data: *const c_uchar,
        len: c_ulong,
        scale_denom: c_int,
        mode: c_int,
        buf: *mut c_uchar,
        buf_len: usize,
        out_w: *mut c_int,
        out_h: *mut c_int,
        out_c: *mut c_int,
    ) -> c_int;
}

/// What `pp_jpeg_decode` writes: RGB (grayscale JPEGs stay one channel),
/// RGBA, or luma alone.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Out {
    Native = 0,
    Rgba = 1,
    Luma = 2,
}

/// Output (width, height, channels) of a decode at 1/`denom` scale.
fn turbo_probe(data: &[u8], denom: u32, out: Out) -> Option<(u32, u32, u8)> {
    let (mut w, mut h, mut c) = (0, 0, 0);
    // SAFETY: data is valid for len bytes; a null buffer only reads headers.
    let e = unsafe {
        pp_jpeg_decode(data.as_ptr(), data.len() as c_ulong, denom as c_int, out as c_int, std::ptr::null_mut(), 0, &mut w, &mut h, &mut c)
    };
    (e == 0 && w > 0 && h > 0).then_some((w as u32, h as u32, c as u8))
}

fn turbo_decode(data: &[u8], denom: u32, out: Out) -> Result<Pixels, String> {
    let (w, h, c) = turbo_probe(data, denom, out).ok_or("not a readable JPEG")?;
    let mut buf = vec![0u8; w as usize * h as usize * c as usize];
    let (mut ow, mut oh, mut oc) = (0, 0, 0);
    // SAFETY: buf holds exactly the probed output size; data is valid.
    let e = unsafe {
        pp_jpeg_decode(data.as_ptr(), data.len() as c_ulong, denom as c_int, out as c_int, buf.as_mut_ptr(), buf.len(), &mut ow, &mut oh, &mut oc)
    };
    if e != 0 || (ow as u32, oh as u32, oc as u8) != (w, h, c) {
        return Err("JPEG decode failed".into());
    }
    Ok(Pixels::new(w, h, c, buf))
}

/// (width, height) from a JPEG's headers, without decoding pixels.
pub fn jpeg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    turbo_probe(data, 1, Out::Native).map(|(w, h, _)| (w, h))
}

/// Full-resolution decode.  Grayscale stays single-channel; everything else
/// (YCbCr, CMYK, YCCK) comes out as RGB.
pub fn decode_jpeg_full(data: &[u8]) -> Result<Pixels, String> {
    turbo_decode(data, 1, Out::Native)
}

/// Decode at 1/`denom` scale (1, 2, 4 or 8) in the JPEG's own channels
/// (grayscale stays one channel, the rest RGB): the cheapest way to a
/// small colour image for the search model's input.
pub fn decode_jpeg_full_scaled(data: &[u8], denom: u32) -> Result<Pixels, String> {
    turbo_decode(data, denom, Out::Native)
}

/// Full-resolution decode straight to RGBA, ready for the GPU.
pub fn decode_jpeg_rgba(data: &[u8]) -> Result<Pixels, String> {
    turbo_decode(data, 1, Out::Rgba)
}

/// Luma (one channel) at 1/`denom` scale (1, 2, 4 or 8).  A YCbCr JPEG
/// gives its Y plane without touching the colour planes (about two thirds
/// of an RGB decode's time on a camera preview); CMYK is decoded to RGB and
/// converted.
pub fn decode_jpeg_luma(data: &[u8], denom: u32) -> Result<Pixels, String> {
    let px = turbo_decode(data, denom, Out::Luma)?;
    Ok(if px.channels == 1 { px } else { to_luma(&px) })
}

/// Rec. 601 luma (what a JPEG's Y plane holds) of 8-bit pixels.
pub fn to_luma(px: &Pixels) -> Pixels {
    let c = px.channels as usize;
    if c == 1 {
        return px.clone();
    }
    let data = px
        .data
        .chunks_exact(c)
        .map(|p| ((p[0] as u32 * 19_595 + p[1] as u32 * 38_470 + p[2] as u32 * 7_471 + 32_768) >> 16) as u8)
        .collect();
    Pixels::new(px.width, px.height, 1, data)
}

/// Decode at reduced size and resize smoothly to fit `box_w`×`box_h` — what
/// Qt's `QImageReader::setScaledSize` does: the largest DCT reduction (1/2,
/// 1/4, 1/8) that stays at or above the target, then a smooth resample.
pub fn decode_jpeg_scaled(data: &[u8], box_w: u32, box_h: u32) -> Result<Pixels, String> {
    let (w, h) = jpeg_dimensions(data).ok_or("not a JPEG")?;
    let (tw, th) = qt_scaled_keep(w, h, box_w, box_h);
    let (tw, th) = (tw.max(1), th.max(1));
    let ratio = (w / tw).min(h / th);
    let denom = match ratio {
        r if r >= 8 => 8,
        r if r >= 4 => 4,
        r if r >= 2 => 2,
        _ => 1,
    };
    let px = turbo_decode(data, denom, Out::Native)?;
    Ok(resize_to(&px, tw, th))
}

/// `QSize(w, h).scaled(bw, bh, Qt::KeepAspectRatio)` — integer truncation
/// exactly as Qt does it, so thumbnail sizes match the Python app's.
pub fn qt_scaled_keep(w: u32, h: u32, bw: u32, bh: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (bw, bh);
    }
    let rw = bh as u64 * w as u64 / h as u64;
    if rw <= bw as u64 {
        (rw as u32, bh)
    } else {
        (bw, (bw as u64 * h as u64 / w as u64) as u32)
    }
}

/// Scale down to fit within `size`×`size`; smaller images are returned as is.
pub fn fit(px: Pixels, size: u32) -> Pixels {
    if px.width <= size && px.height <= size {
        return px;
    }
    let (tw, th) = qt_scaled_keep(px.width, px.height, size, size);
    resize_to(&px, tw.max(1), th.max(1))
}

fn pixel_type(channels: u8) -> PixelType {
    match channels {
        1 => PixelType::U8,
        3 => PixelType::U8x3,
        _ => PixelType::U8x4,
    }
}

/// High-quality resample to exactly `w`×`h`.
pub fn resize_to(px: &Pixels, w: u32, h: u32) -> Pixels {
    if px.width == w && px.height == h {
        return px.clone();
    }
    let pt = pixel_type(px.channels);
    let Ok(src) = ImageRef::new(px.width, px.height, &px.data, pt) else {
        return px.clone();
    };
    let mut dst = FrImage::new(w, h, pt);
    let alg = ResizeAlg::Convolution(fr::FilterType::CatmullRom);
    let mut resizer = Resizer::new();
    if resizer.resize(&src, &mut dst, &ResizeOptions::new().resize_alg(alg)).is_err() {
        return px.clone();
    }
    Pixels::new(w, h, px.channels, dst.into_vec())
}

/// 2× box downsample — one level of a mipmap pyramid.  Each output pixel
/// is the rounded mean of a 2×2 block (an odd last row/column is dropped),
/// computed in parallel bands.
pub fn half(px: &Pixels) -> Pixels {
    let (w, h) = ((px.width / 2).max(1), (px.height / 2).max(1));
    if px.width < 2 || px.height < 2 {
        return resize_to(px, w, h);
    }
    let c = px.channels as usize;
    let src_stride = px.width as usize * c;
    let dst_stride = w as usize * c;
    let mut out = vec![0u8; dst_stride * h as usize];
    out.par_chunks_mut(dst_stride * 16).enumerate().for_each(|(band, chunk)| {
        for (i, dst) in chunk.chunks_exact_mut(dst_stride).enumerate() {
            let y = band * 16 + i;
            let r0 = &px.data[2 * y * src_stride..][..src_stride];
            let r1 = &px.data[(2 * y + 1) * src_stride..][..src_stride];
            for (x, d) in dst.chunks_exact_mut(c).enumerate() {
                let o = 2 * x * c;
                for k in 0..c {
                    let sum = r0[o + k] as u16 + r0[o + c + k] as u16 + r1[o + k] as u16 + r1[o + c + k] as u16;
                    d[k] = ((sum + 2) >> 2) as u8;
                }
            }
        }
    });
    Pixels::new(w, h, px.channels, out)
}

/// Rotate / mirror by an EXIF Orientation value (1 = unchanged).
pub fn apply_orientation(px: Pixels, orientation: u8) -> Pixels {
    if !(2..=8).contains(&orientation) {
        return px;
    }
    let (w, h, c) = (px.width as usize, px.height as usize, px.channels as usize);
    let swap = orientation >= 5;
    let (nw, nh) = if swap { (h, w) } else { (w, h) };
    // Source pixel for destination (dx, dy).
    let src_of = |dx: usize, dy: usize| -> (usize, usize) {
        match orientation {
            2 => (w - 1 - dx, dy),         // mirror horizontal
            3 => (w - 1 - dx, h - 1 - dy), // rotate 180
            4 => (dx, h - 1 - dy),         // mirror vertical
            5 => (dy, dx),                 // transpose
            6 => (dy, h - 1 - dx),         // rotate 90 CW
            7 => (w - 1 - dy, h - 1 - dx), // transverse
            8 => (w - 1 - dy, dx),         // rotate 270 CW
            _ => (dx, dy),
        }
    };
    const TILE: usize = 64;
    let dst_stride = nw * c;
    let mut out = vec![0u8; px.data.len()];
    // Bands of output rows in parallel; within a band, square tiles keep
    // both the reads and the writes cache-friendly for the 90° cases.
    out.par_chunks_mut(dst_stride * TILE).enumerate().for_each(|(band, chunk)| {
        let y0 = band * TILE;
        let rows = chunk.len() / dst_stride;
        for x0 in (0..nw).step_by(TILE) {
            let x1 = (x0 + TILE).min(nw);
            for r in 0..rows {
                let dy = y0 + r;
                let drow = &mut chunk[r * dst_stride..(r + 1) * dst_stride];
                for dx in x0..x1 {
                    let (sx, sy) = src_of(dx, dy);
                    let si = (sy * w + sx) * c;
                    drow[dx * c..dx * c + c].copy_from_slice(&px.data[si..si + c]);
                }
            }
        }
    });
    Pixels::new(nw as u32, nh as u32, px.channels, out)
}

/// Copy out a sub-rectangle.
pub fn crop(px: &Pixels, x: u32, y: u32, w: u32, h: u32) -> Pixels {
    let c = px.channels as usize;
    let stride = px.width as usize * c;
    let mut out = Vec::with_capacity(w as usize * h as usize * c);
    for row in y..y + h {
        let start = row as usize * stride + x as usize * c;
        out.extend_from_slice(&px.data[start..start + w as usize * c]);
    }
    Pixels::new(w, h, px.channels, out)
}

/// Interleaved RGBA for an `iced::widget::image::Handle`.
pub fn to_rgba(px: Pixels) -> Vec<u8> {
    match px.channels {
        4 => px.data,
        3 => {
            let mut out = Vec::with_capacity(px.data.len() / 3 * 4);
            for p in px.data.chunks_exact(3) {
                out.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
            out
        }
        1 => {
            let mut out = Vec::with_capacity(px.data.len() * 4);
            for &g in &px.data {
                out.extend_from_slice(&[g, g, g, 255]);
            }
            out
        }
        _ => px.data,
    }
}

/// Drop to RGB (no alpha) — what the JPEG encoder wants.
pub fn to_rgb(px: &Pixels) -> Vec<u8> {
    match px.channels {
        3 => px.data.clone(),
        4 => px.data.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect(),
        1 => px.data.iter().flat_map(|&g| [g, g, g]).collect(),
        _ => px.data.clone(),
    }
}

/// Decode a PNG (legacy disk-cache entries) to pixels.
pub fn decode_png(data: &[u8]) -> Option<Pixels> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(data));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::GrayscaleAlpha => {
            let rgba: Vec<u8> = buf.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect();
            return Some(Pixels::new(info.width, info.height, 4, rgba));
        }
        png::ColorType::Indexed => return None,
    };
    Some(Pixels::new(info.width, info.height, channels, buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qt_keep_aspect_matches_qt() {
        assert_eq!(qt_scaled_keep(1616, 1080, 300, 300), (300, 200));
        assert_eq!(qt_scaled_keep(1080, 1616, 300, 300), (200, 300));
        assert_eq!(qt_scaled_keep(7008, 4672, 300, 300), (300, 200));
    }

    #[test]
    fn luma_decode_is_the_y_plane() {
        // A colourful RGB JPEG: its luma decode matches the RGB decode's
        // luma to within rounding, at full and at reduced size.
        let (w, h) = (64u16, 48u16);
        let rgb: Vec<u8> = (0..w as usize * h as usize).flat_map(|i| [(i * 7 % 256) as u8, (i * 3 % 256) as u8, (255 - i % 256) as u8]).collect();
        let mut jpg = Vec::new();
        jpeg_encoder::Encoder::new(&mut jpg, 95).encode(&rgb, w, h, jpeg_encoder::ColorType::Rgb).unwrap();
        let y = decode_jpeg_luma(&jpg, 1).unwrap();
        assert_eq!((y.width, y.height, y.channels), (64, 48, 1));
        let from_rgb = to_luma(&decode_jpeg_full(&jpg).unwrap());
        let worst = y.data.iter().zip(&from_rgb.data).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        assert!(worst <= 3, "{worst}");
        let half = decode_jpeg_luma(&jpg, 2).unwrap();
        assert_eq!((half.width, half.height, half.channels), (32, 24, 1));
        // A grayscale JPEG stays one channel.
        let mut g = Vec::new();
        jpeg_encoder::Encoder::new(&mut g, 90).encode(&y.data, w, h, jpeg_encoder::ColorType::Luma).unwrap();
        assert_eq!(decode_jpeg_luma(&g, 1).unwrap().channels, 1);
        assert!(decode_jpeg_luma(b"not a jpeg", 1).is_err());
    }

    #[test]
    fn orientation_6_rotates_clockwise() {
        // 2x1 image: [A, B] → rotate 90 CW → 1x2: [A; B]
        let px = Pixels::new(2, 1, 1, vec![1, 2]);
        let r = apply_orientation(px, 6);
        assert_eq!((r.width, r.height), (1, 2));
        assert_eq!(r.data, vec![1, 2]);
        let px = Pixels::new(2, 1, 1, vec![1, 2]);
        let r = apply_orientation(px, 8);
        assert_eq!(r.data, vec![2, 1]);
    }
}
