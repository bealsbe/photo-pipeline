//! Turning a stored crop into something on disk.
//!
//! **JPG** — re-encoded with the crop applied to the pixels (q95, 4:4:4,
//! optimised Huffman tables).  EXIF and the ICC profile are carried across;
//! Orientation is reset to 1 because the crop is computed against the
//! already-rotated image.  The EXIF block is patched in place rather than
//! rebuilt, so maker notes (which hold absolute offsets) stay valid; the IFD1
//! thumbnail is dropped since it shows the uncropped frame.  (The Python app
//! re-serialised EXIF through Pillow, which broke Sony maker-note offsets and
//! added IFD0 ImageWidth/ImageLength — tags exiftool rejects in a JPEG.  The
//! new size goes in PixelX/YDimension instead.)
//!
//! **RAW** — the bits are sacred: the file is copied untouched and an XMP
//! sidecar carries the same rect in Camera Raw's `crs:Crop*` fields, which
//! Lightroom / ACR / Bridge honour.  An existing sidecar is merged, never
//! replaced, so develop settings written by other software survive.

use std::fs;
use std::path::{Path, PathBuf};

use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;

use super::fsutil;
use crate::exif::{self, Endian};
use crate::imaging::decode;
use crate::model::crop::Crop;

pub const JPEG_QUALITY: u8 = 95;

const NS_X: &str = "adobe:ns:meta/";
const NS_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const NS_CRS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";
const NS_XMP: &str = "http://ns.adobe.com/xap/1.0/";

const EMPTY_XMP: &str = concat!(
    r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="photo-pipeline">"#,
    r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">"#,
    r#"<rdf:Description rdf:about="""#,
    r#" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/""#,
    r#" xmlns:xmp="http://ns.adobe.com/xap/1.0/"/>"#,
    r#"</rdf:RDF></x:xmpmeta>"#
);

const PACKET_HEAD: &str = "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n";
const PACKET_TAIL: &str = "\n<?xpacket end=\"w\"?>\n";

// ── JPEG ────────────────────────────────────────────────────────────────────

/// Write `src` to `dest` with `crop` applied to the pixels, atomically.
pub fn render_cropped_jpeg(src: &Path, dest: &Path, crop: &Crop, overwrite: bool) -> Result<(), String> {
    let name = crate::util::file_name(src);
    let data = fs::read(src).map_err(|e| format!("{name}: {e}"))?;
    let img = decode::decode_jpeg_full(&data).map_err(|e| format!("{name}: {e}"))?;
    let orientation = exif::orientation_from_jpeg_head(&data);
    let upright = decode::apply_orientation(img, orientation);
    let (l, t, r, b) = crop.to_pixels(upright.width, upright.height);
    let cropped = decode::crop(&upright, l, t, r - l, b - t);

    let exif_blob = patched_exif(&data, cropped.width, cropped.height);
    let icc = icc_profile(&data);

    let mut out = Vec::with_capacity(cropped.data.len() / 4);
    {
        use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
        let mut enc = Encoder::new(&mut out, JPEG_QUALITY);
        enc.set_sampling_factor(SamplingFactor::F_1_1);
        enc.set_optimized_huffman_tables(true);
        if let Some(blob) = &exif_blob {
            enc.add_app_segment(1, blob.clone()).map_err(|e| format!("{name}: {e}"))?;
        }
        if let Some(icc) = &icc {
            enc.add_icc_profile(icc).map_err(|e| format!("{name}: {e}"))?;
        }
        let color = if cropped.channels == 1 { ColorType::Luma } else { ColorType::Rgb };
        enc.encode(&cropped.data, cropped.width as u16, cropped.height as u16, color)
            .map_err(|e| format!("{name}: {e}"))?;
    }
    fsutil::ensure_parent(dest).map_err(|e| format!("{name}: {e}"))?;
    fsutil::atomic_write(dest, &out, overwrite).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!("destination appeared after plan: {}", crate::util::file_name(dest))
        } else {
            format!("{name}: {e}")
        }
    })
}

/// The source's EXIF APP1 payload ("Exif\0\0" + TIFF), patched for the
/// cropped, upright output.  `None` when the source carries no EXIF.
fn patched_exif(jpeg: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let (tiff, end, ifd0, _base) = exif::exif_tiff(jpeg)?;
    let mut blob = Vec::with_capacity(tiff.len() + 6);
    blob.extend_from_slice(b"Exif\0\0");
    blob.extend_from_slice(tiff);
    let t = &mut blob[6..];

    let mut exif_ifd = 0u32;
    patch_ifd(t, end, ifd0, |tag, typ, pos, buf| match tag {
        0x0112 => write_short(buf, end, pos, 1),                        // Orientation
        0x0100 => write_dim(buf, end, pos, typ, width),                 // ImageWidth
        0x0101 => write_dim(buf, end, pos, typ, height),                // ImageLength
        0x8769 => exif_ifd = read_u32(buf, end, pos + 8).unwrap_or(0),  // Exif IFD
        _ => {}
    });
    if exif_ifd != 0 {
        patch_ifd(t, end, exif_ifd, |tag, typ, pos, buf| match tag {
            0xA002 => write_dim(buf, end, pos, typ, width),  // PixelXDimension
            0xA003 => write_dim(buf, end, pos, typ, height), // PixelYDimension
            _ => {}
        });
    }
    // Drop IFD1 (the embedded thumbnail of the uncropped frame).
    if let Some(count) = read_u16(t, end, ifd0 as usize) {
        let next = ifd0 as usize + 2 + count as usize * 12;
        if next + 4 <= t.len() {
            t[next..next + 4].copy_from_slice(&[0, 0, 0, 0]);
        }
    }
    Some(blob)
}

fn read_u16(t: &[u8], end: Endian, pos: usize) -> Option<u16> {
    t.get(pos..pos + 2).map(|b| end.u16(b))
}

fn read_u32(t: &[u8], end: Endian, pos: usize) -> Option<u32> {
    t.get(pos..pos + 4).map(|b| end.u32(b))
}

/// Visit each entry of the IFD at `offset`: f(tag, type, entry_pos, buf).
fn patch_ifd(t: &mut [u8], end: Endian, offset: u32, mut f: impl FnMut(u16, u16, usize, &mut [u8])) {
    let off = offset as usize;
    let Some(count) = read_u16(t, end, off) else { return };
    for i in 0..count as usize {
        let pos = off + 2 + i * 12;
        if pos + 12 > t.len() {
            break;
        }
        let tag = end.u16(&t[pos..pos + 2]);
        let typ = end.u16(&t[pos + 2..pos + 4]);
        f(tag, typ, pos, t);
    }
}

fn write_short(t: &mut [u8], end: Endian, pos: usize, v: u16) {
    let bytes = match end {
        Endian::Little => v.to_le_bytes(),
        Endian::Big => v.to_be_bytes(),
    };
    t[pos + 8..pos + 10].copy_from_slice(&bytes);
}

fn write_dim(t: &mut [u8], end: Endian, pos: usize, typ: u16, v: u32) {
    match typ {
        3 if v <= u16::MAX as u32 => write_short(t, end, pos, v as u16), // SHORT
        4 => {
            let bytes = match end {
                Endian::Little => v.to_le_bytes(),
                Endian::Big => v.to_be_bytes(),
            };
            t[pos + 8..pos + 12].copy_from_slice(&bytes);
        }
        _ => {}
    }
}

/// Reassemble an ICC profile split across APP2 `ICC_PROFILE` chunks.
fn icc_profile(jpeg: &[u8]) -> Option<Vec<u8>> {
    let mut chunks: Vec<(u8, &[u8])> = Vec::new();
    for (marker, start, stop) in exif::iter_segments(jpeg) {
        if marker != 0xE2 {
            continue;
        }
        let seg = jpeg.get(start..stop.min(jpeg.len()))?;
        if seg.len() > 14 && &seg[..12] == b"ICC_PROFILE\0" {
            chunks.push((seg[12], &seg[14..]));
        }
    }
    if chunks.is_empty() {
        return None;
    }
    chunks.sort_by_key(|c| c.0);
    Some(chunks.into_iter().flat_map(|c| c.1.iter().copied()).collect())
}

// ── RAW / XMP ───────────────────────────────────────────────────────────────

/// `IMG_0042.cr3` → `IMG_0042.xmp`.
pub fn xmp_sidecar_path(image: &Path) -> PathBuf {
    image.with_extension("xmp")
}

/// Write or update the XMP sidecar next to `image` with `crop`.
pub fn write_xmp_crop(image: &Path, crop: &Crop) -> Result<PathBuf, String> {
    let sidecar = xmp_sidecar_path(image);
    let existing = fs::read_to_string(&sidecar).ok();
    let now = chrono::Local::now().naive_local().format("%Y-%m-%dT%H:%M:%S").to_string();
    let props: Vec<(&str, &str, String)> = vec![
        (NS_CRS, "HasCrop", "True".into()),
        (NS_CRS, "CropTop", format!("{:.6}", crop.top)),
        (NS_CRS, "CropLeft", format!("{:.6}", crop.left)),
        (NS_CRS, "CropBottom", format!("{:.6}", crop.bottom)),
        (NS_CRS, "CropRight", format!("{:.6}", crop.right)),
        (NS_CRS, "CropAngle", "0".into()),
        (NS_CRS, "CropConstrainToWarp", "1".into()),
        (NS_XMP, "ModifyDate", now),
        (NS_XMP, "CreatorTool", "photo-pipeline".into()),
    ];
    let body = existing
        .as_deref()
        .and_then(|text| rewrite_description(text, &props, &[]))
        .or_else(|| rewrite_description(EMPTY_XMP, &props, &[]))
        .ok_or_else(|| "could not build XMP".to_string())?;
    let packet = format!("{PACKET_HEAD}{body}{PACKET_TAIL}");
    let name = crate::util::file_name(&sidecar);
    fsutil::ensure_parent(&sidecar).map_err(|e| format!("{name}: {e}"))?;
    fsutil::atomic_write(&sidecar, packet.as_bytes(), true).map_err(|e| format!("{name}: {e}"))?;
    Ok(sidecar)
}

/// Clear the crop from an existing sidecar (`HasCrop` false, rect removed).
/// Best effort: other develop settings are kept, and failure is ignored.
pub fn remove_xmp_crop(image: &Path) {
    let sidecar = xmp_sidecar_path(image);
    let Ok(text) = fs::read_to_string(&sidecar) else { return };
    let props = [(NS_CRS, "HasCrop", "False".to_string())];
    let remove = [(NS_CRS, "CropTop"), (NS_CRS, "CropLeft"), (NS_CRS, "CropBottom"), (NS_CRS, "CropRight")];
    if let Some(body) = rewrite_description(&text, &props, &remove) {
        let _ = fsutil::atomic_write(&sidecar, format!("{PACKET_HEAD}{body}{PACKET_TAIL}").as_bytes(), true);
    }
}

/// Rewrite the root element of an XMP document, setting `props` (and
/// deleting `remove`) on the first `rdf:Description`.  Returns `None` when
/// the text is not well-formed or has no Description.
fn rewrite_description(
    text: &str,
    props: &[(&str, &str, String)],
    remove: &[(&str, &str)],
) -> Option<String> {
    let text = text.trim().trim_start_matches('\u{feff}');
    let mut reader = quick_xml::reader::NsReader::from_str(text);
    reader.config_mut().trim_text(false);
    let mut out = quick_xml::Writer::new(Vec::new());
    let mut depth = 0usize;
    let mut started = false;
    let mut done_root = false;
    let mut patched = false;
    // While inside the Description we rewrote, skip child elements that
    // duplicate a property we set (depth at which skipping ends).
    let mut in_desc: Option<usize> = None;
    let mut skip_until: Option<usize> = None;
    // Prefixes in scope for the namespaces we write.
    let touched: Vec<(&str, &str)> =
        props.iter().map(|(ns, l, _)| (*ns, *l)).chain(remove.iter().copied()).collect();

    loop {
        let (resolved, event) = reader.read_resolved_event().ok()?;
        let is_desc = |r: &ResolveResult, e: &BytesStart| {
            matches!(r, ResolveResult::Bound(ns) if ns.as_ref() == NS_RDF)
                && e.local_name().as_ref() == "Description"
        };
        match event {
            Event::Eof => break,
            _ if done_root => continue,
            Event::Start(e) => {
                if !started {
                    started = true;
                }
                depth += 1;
                if skip_until.is_some() {
                    continue;
                }
                if in_desc == Some(depth - 1) && is_property(&resolved, &e, &touched) {
                    skip_until = Some(depth - 1);
                    continue;
                }
                if !patched && is_desc(&resolved, &e) {
                    let new = patch_start(&reader, &e, props, remove)?;
                    out.write_event(Event::Start(new)).ok()?;
                    patched = true;
                    in_desc = Some(depth);
                } else {
                    out.write_event(Event::Start(e)).ok()?;
                }
            }
            Event::Empty(e) => {
                if !started {
                    started = true;
                    done_root = true;
                }
                if skip_until.is_some() {
                    continue;
                }
                if in_desc == Some(depth) && is_property(&resolved, &e, &touched) {
                    continue;
                }
                if !patched && is_desc(&resolved, &e) {
                    let new = patch_start(&reader, &e, props, remove)?;
                    out.write_event(Event::Empty(new)).ok()?;
                    patched = true;
                } else {
                    out.write_event(Event::Empty(e)).ok()?;
                }
            }
            Event::End(e) => {
                depth = depth.checked_sub(1)?;
                if let Some(level) = skip_until {
                    if depth == level {
                        skip_until = None;
                    }
                    continue;
                }
                if in_desc == Some(depth + 1) {
                    in_desc = None;
                }
                out.write_event(Event::End(e)).ok()?;
                if depth == 0 {
                    done_root = true;
                }
            }
            // Declarations, processing instructions and comments outside the
            // root element are dropped; the packet wrapper is re-added.
            Event::Decl(_) | Event::PI(_) | Event::DocType(_) if !started || depth == 0 => {}
            Event::Comment(_) | Event::Text(_) if !started => {}
            other => {
                if skip_until.is_none() && started {
                    out.write_event(other).ok()?;
                }
            }
        }
    }
    if !patched {
        return None;
    }
    String::from_utf8(out.into_inner()).ok()
}

fn is_property(resolved: &ResolveResult, e: &BytesStart, touched: &[(&str, &str)]) -> bool {
    let ResolveResult::Bound(ns) = resolved else { return false };
    touched
        .iter()
        .any(|(uri, local)| ns.as_ref() == *uri && e.local_name().as_ref() == *local)
}

/// Copy the Description's attributes minus the ones we set or remove, then
/// append ours, declaring namespaces where they are not yet in scope.
fn patch_start<'a>(
    reader: &quick_xml::reader::NsReader<&[u8]>,
    e: &BytesStart<'a>,
    props: &[(&str, &str, String)],
    remove: &[(&str, &str)],
) -> Option<BytesStart<'static>> {
    let mut new = BytesStart::new(e.name().as_ref().to_string());
    let targets: Vec<(&str, &str)> =
        props.iter().map(|(ns, l, _)| (*ns, *l)).chain(remove.iter().copied()).collect();
    // Prefixes already bound to our namespaces, from this element or above.
    let prefix_for = |uri: &str| -> Option<String> {
        for attr in e.attributes().flatten() {
            let key: &str = attr.key.as_ref();
            if let Some(p) = key.strip_prefix("xmlns:") {
                let value: &str = attr.value.as_ref();
                if value == uri {
                    return Some(p.to_string());
                }
            }
        }
        for (prefix, ns) in reader.resolver().bindings() {
            if ns.as_ref() == uri {
                if let quick_xml::name::PrefixDeclaration::Named(p) = prefix {
                    return Some(p.to_string());
                }
            }
        }
        None
    };
    let crs = prefix_for(NS_CRS);
    let xmp = prefix_for(NS_XMP);
    for attr in e.attributes().flatten() {
        let (resolved, local) = reader.resolver().resolve_attribute(attr.key);
        let drop = matches!(&resolved, ResolveResult::Bound(ns)
            if targets.iter().any(|(uri, l)| ns.as_ref() == *uri && local.as_ref() == *l));
        if !drop {
            new.push_attribute(attr);
        }
    }
    let crs = crs.unwrap_or_else(|| {
        new.push_attribute(("xmlns:crs", NS_CRS));
        "crs".into()
    });
    let xmp = if props.iter().any(|(ns, _, _)| *ns == NS_XMP) {
        xmp.unwrap_or_else(|| {
            new.push_attribute(("xmlns:xmp", NS_XMP));
            "xmp".into()
        })
    } else {
        xmp.unwrap_or_else(|| "xmp".into())
    };
    for (ns, local, value) in props {
        let prefix = if *ns == NS_CRS { &crs } else { &xmp };
        new.push_attribute((format!("{prefix}:{local}").as_str(), value.as_str()));
    }
    let _ = NS_X;
    Some(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A JPEG shell around a little-endian EXIF block: IFD0 holds
    /// Orientation=6 and an Exif IFD pointer; the Exif IFD holds
    /// PixelX/YDimension; IFD1 is linked.
    fn exif_jpeg() -> Vec<u8> {
        let mut t: Vec<u8> = b"II*\0".to_vec();
        t.extend_from_slice(&8u32.to_le_bytes());
        let entry = |tag: u16, typ: u16, v: u32| {
            let mut e = Vec::new();
            e.extend_from_slice(&tag.to_le_bytes());
            e.extend_from_slice(&typ.to_le_bytes());
            e.extend_from_slice(&1u32.to_le_bytes());
            e.extend_from_slice(&v.to_le_bytes());
            e
        };
        // IFD0 at 8: 2 entries -> 8 + 2 + 24 + 4 = 38; Exif IFD at 38
        t.extend_from_slice(&2u16.to_le_bytes());
        t.extend(entry(0x0112, 3, 6));
        t.extend(entry(0x8769, 4, 38));
        t.extend_from_slice(&64u32.to_le_bytes()); // IFD1 at 64
        t.extend_from_slice(&2u16.to_le_bytes());
        t.extend(entry(0xA002, 4, 6000));
        t.extend(entry(0xA003, 4, 4000));
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes()); // empty IFD1 at 64
        t.extend_from_slice(&0u32.to_le_bytes());
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(t);
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE1];
        j.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        j.extend(app1);
        j.extend_from_slice(&[0xFF, 0xD9]);
        j
    }

    #[test]
    fn exif_patched_for_cropped_output() {
        let blob = patched_exif(&exif_jpeg(), 3000, 2000).unwrap();
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE1];
        j.extend_from_slice(&((blob.len() + 2) as u16).to_be_bytes());
        j.extend(&blob);
        j.extend_from_slice(&[0xFF, 0xD9]);
        let (t, end, ifd0, _) = exif::exif_tiff(&j).unwrap();
        let mut tags = std::collections::HashMap::new();
        let n = read_u16(t, end, ifd0 as usize).unwrap() as usize;
        let mut last = 0;
        for i in 0..n {
            let p = ifd0 as usize + 2 + i * 12;
            let tag = end.u16(&t[p..p + 2]);
            assert!(tag > last, "IFD0 entries must stay sorted");
            last = tag;
            tags.insert(tag, end.u32(&t[p + 8..p + 12]));
        }
        // The size goes in PixelX/YDimension; IFD0 ImageWidth/ImageLength
        // are not allowed in a JPEG (Pillow added them for the Python app).
        assert!(!tags.contains_key(&0x0100) && !tags.contains_key(&0x0101));
        assert_eq!(tags[&0x0112] & 0xffff, 1);
        let next = ifd0 as usize + 2 + n * 12;
        assert_eq!(read_u32(t, end, next), Some(0), "IFD1 dropped");
        let exif_ifd = tags[&0x8769] as usize;
        assert_eq!(read_u32(t, end, exif_ifd + 2 + 8), Some(3000));
        assert_eq!(read_u32(t, end, exif_ifd + 2 + 12 + 8), Some(2000));
    }

    #[test]
    fn xmp_created_from_scratch() {
        let d = std::env::temp_dir().join(format!("pp-xmp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let raw = d.join("IMG_1.ARW");
        let crop = Crop::new(0.1, 0.2, 0.9, 0.8, None);
        let side = write_xmp_crop(&raw, &crop).unwrap();
        assert_eq!(side, d.join("IMG_1.xmp"));
        let text = fs::read_to_string(&side).unwrap();
        assert!(text.starts_with("<?xpacket begin="));
        assert!(text.contains(r#"crs:CropTop="0.200000""#));
        assert!(text.contains(r#"crs:HasCrop="True""#));
        assert!(text.contains(r#"xmp:CreatorTool="photo-pipeline""#));
    }

    #[test]
    fn xmp_merge_keeps_other_settings() {
        let existing = r#"<?xpacket begin="﻿" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="+0.50" crs:CropTop="0.3">
<crs:CropLeft>0.4</crs:CropLeft><crs:ToneCurveName2012>Linear</crs:ToneCurveName2012>
</rdf:Description></rdf:RDF></x:xmpmeta>
<?xpacket end="w"?>"#;
        let props = vec![
            (NS_CRS, "HasCrop", "True".to_string()),
            (NS_CRS, "CropTop", "0.100000".to_string()),
            (NS_CRS, "CropLeft", "0.200000".to_string()),
            (NS_XMP, "CreatorTool", "photo-pipeline".to_string()),
        ];
        let out = rewrite_description(existing, &props, &[]).unwrap();
        assert!(out.contains(r#"crs:Exposure2012="+0.50""#));
        assert!(out.contains("<crs:ToneCurveName2012>Linear</crs:ToneCurveName2012>"));
        assert!(out.contains(r#"crs:CropTop="0.100000""#));
        assert!(!out.contains(r#"crs:CropTop="0.3""#));
        assert!(!out.contains("<crs:CropLeft>"));
        assert!(out.contains(r#"xmlns:xmp="http://ns.adobe.com/xap/1.0/""#));
        assert_eq!(out.matches("CropTop").count(), 1);
    }

    #[test]
    fn remove_crop_clears_rect() {
        let text = rewrite_description(
            EMPTY_XMP,
            &[(NS_CRS, "HasCrop", "True".into()), (NS_CRS, "CropTop", "0.1".into())],
            &[],
        )
        .unwrap();
        let cleared =
            rewrite_description(&text, &[(NS_CRS, "HasCrop", "False".into())], &[(NS_CRS, "CropTop")]).unwrap();
        assert!(cleared.contains(r#"crs:HasCrop="False""#));
        assert!(!cleared.contains("CropTop"));
    }
}
