//! Taking one file out of a zip archive (InsightFace's `buffalo_l.zip`,
//! of which the app keeps the recognition model).  Enough of the
//! format for that: the end-of-central-directory record, the central
//! directory's entries, a local header, and stored or deflated data,
//! streamed out so the 174 MB member is never held whole in memory.
//! Zip64 archives are refused in plain words (the pinned archive isn't
//! one).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

const EOCD: u32 = 0x0605_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const LOCAL: u32 = 0x0403_4b50;
const STORED: u16 = 0;
const DEFLATE: u16 = 8;

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// One member as the central directory lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub method: u16,
    pub compressed: u64,
    pub size: u64,
    pub local_header: u64,
}

/// The archive's members.
pub fn members(file: &mut File) -> io::Result<Vec<Member>> {
    let len = file.metadata()?.len();
    // The EOCD record is 22 bytes plus a comment of up to 64 KB, at the
    // very end.
    let tail_len = len.min(22 + 65_536);
    file.seek(SeekFrom::Start(len - tail_len))?;
    let mut tail = vec![0u8; tail_len as usize];
    file.read_exact(&mut tail)?;
    let eocd = (0..tail.len().saturating_sub(21)).rev().find(|&i| u32_at(&tail, i) == EOCD).ok_or_else(|| io::Error::other("not a zip archive (no end record)"))?;
    let entries = u16_at(&tail, eocd + 10) as usize;
    let dir_size = u32_at(&tail, eocd + 12) as u64;
    let dir_offset = u32_at(&tail, eocd + 16) as u64;
    if entries == 0xffff || dir_size == 0xffff_ffff || dir_offset == 0xffff_ffff {
        return Err(io::Error::other("a zip64 archive, which isn't supported"));
    }
    file.seek(SeekFrom::Start(dir_offset))?;
    let mut dir = vec![0u8; dir_size as usize];
    file.read_exact(&mut dir)?;
    let mut out = Vec::with_capacity(entries);
    let mut i = 0usize;
    while out.len() < entries {
        if i + 46 > dir.len() || u32_at(&dir, i) != CENTRAL {
            return Err(io::Error::other("the zip's central directory is damaged"));
        }
        let method = u16_at(&dir, i + 10);
        let compressed = u32_at(&dir, i + 20) as u64;
        let size = u32_at(&dir, i + 24) as u64;
        let name_len = u16_at(&dir, i + 28) as usize;
        let extra_len = u16_at(&dir, i + 30) as usize;
        let comment_len = u16_at(&dir, i + 32) as usize;
        let local_header = u32_at(&dir, i + 42) as u64;
        let name = String::from_utf8_lossy(dir.get(i + 46..i + 46 + name_len).ok_or_else(|| io::Error::other("the zip's central directory is damaged"))?).into_owned();
        if compressed == 0xffff_ffff || size == 0xffff_ffff || local_header == 0xffff_ffff {
            return Err(io::Error::other("a zip64 archive, which isn't supported"));
        }
        out.push(Member { name, method, compressed, size, local_header });
        i += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

/// Copy the member called `name` (its base name, whatever folder the
/// archive puts it in) out of the archive into `out`, calling `progress`
/// with the bytes written so far.  Returns the bytes written.
pub fn extract(archive: &Path, name: &str, out: &mut dyn Write, progress: &mut dyn FnMut(u64)) -> io::Result<u64> {
    let mut file = File::open(archive)?;
    let members = members(&mut file)?;
    let m = members.iter().find(|m| m.name == name || m.name.rsplit('/').next() == Some(name)).ok_or_else(|| io::Error::other(format!("the archive holds no {name}")))?;
    file.seek(SeekFrom::Start(m.local_header))?;
    let mut head = [0u8; 30];
    file.read_exact(&mut head)?;
    if u32_at(&head, 0) != LOCAL {
        return Err(io::Error::other("the zip's local header is damaged"));
    }
    let skip = u16_at(&head, 26) as u64 + u16_at(&head, 28) as u64;
    file.seek(SeekFrom::Current(skip as i64))?;
    let raw = file.take(m.compressed);
    let mut reader: Box<dyn Read> = match m.method {
        STORED => Box::new(raw),
        DEFLATE => Box::new(flate2::read::DeflateDecoder::new(raw)),
        other => return Err(io::Error::other(format!("the archive uses compression method {other}, which isn't supported"))),
    };
    let mut buf = vec![0u8; 256 * 1024];
    let mut written = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        written += n as u64;
        progress(written);
    }
    if written != m.size {
        return Err(io::Error::other(format!("{name} came out {written} bytes long, not {}", m.size)));
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zip with two members: `a.txt` stored and `dir/b.bin` deflated.
    fn build_zip() -> (Vec<u8>, Vec<u8>) {
        let a = b"hello zip".to_vec();
        let b: Vec<u8> = (0..300_000u32).map(|i| (i % 7) as u8).collect();
        let mut deflated = Vec::new();
        {
            use std::io::Write;
            let mut e = flate2::write::DeflateEncoder::new(&mut deflated, flate2::Compression::default());
            e.write_all(&b).unwrap();
            e.finish().unwrap();
        }
        let mut out = Vec::new();
        let mut central = Vec::new();
        let mut local = |name: &[u8], method: u16, data: &[u8], size: u32| {
            let offset = out.len() as u32;
            out.extend_from_slice(&LOCAL.to_le_bytes());
            out.extend_from_slice(&[20, 0, 0, 0]);
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0; 8]); // time, date, crc
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&3u16.to_le_bytes()); // an extra field
            out.extend_from_slice(name);
            out.extend_from_slice(b"xyz");
            out.extend_from_slice(data);
            central.extend_from_slice(&CENTRAL.to_le_bytes());
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
            central.extend_from_slice(&method.to_le_bytes());
            central.extend_from_slice(&[0; 8]);
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&size.to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // extra 0, comment 2, disk, attrs
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name);
            central.extend_from_slice(b"hi");
        };
        local(b"a.txt", STORED, &a, a.len() as u32);
        local(b"dir/b.bin", DEFLATE, &deflated, b.len() as u32);
        let dir_offset = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&EOCD.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&dir_offset.to_le_bytes());
        out.extend_from_slice(&7u16.to_le_bytes());
        out.extend_from_slice(b"comment");
        (out, b)
    }

    #[test]
    fn a_member_is_found_by_base_name_and_inflated_whole() {
        let d = std::env::temp_dir().join(format!("pp-unzip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (zip, b) = build_zip();
        let path = d.join("x.zip");
        std::fs::write(&path, &zip).unwrap();
        let ms = members(&mut File::open(&path).unwrap()).unwrap();
        assert_eq!(ms.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["a.txt", "dir/b.bin"]);
        assert_eq!((ms[1].method, ms[1].size), (DEFLATE, b.len() as u64));
        let mut out = Vec::new();
        let mut seen = Vec::new();
        assert_eq!(extract(&path, "b.bin", &mut out, &mut |n| seen.push(n)).unwrap(), b.len() as u64);
        assert_eq!(out, b);
        assert_eq!(seen.last(), Some(&(b.len() as u64)));
        let mut out = Vec::new();
        extract(&path, "a.txt", &mut out, &mut |_| {}).unwrap();
        assert_eq!(out, b"hello zip");
        assert!(extract(&path, "c.bin", &mut Vec::new(), &mut |_| {}).is_err());
        // Not a zip at all, and a damaged directory.
        std::fs::write(d.join("no.zip"), b"nothing here").unwrap();
        assert!(extract(&d.join("no.zip"), "a.txt", &mut Vec::new(), &mut |_| {}).is_err());
        let mut bad = zip.clone();
        bad[zip.len() - 19] ^= 0x55; // the entry count (the comment is 7 bytes)
        std::fs::write(d.join("bad.zip"), &bad).unwrap();
        assert!(extract(&d.join("bad.zip"), "a.txt", &mut Vec::new(), &mut |_| {}).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
