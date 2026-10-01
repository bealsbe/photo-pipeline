//! Small binary cache files: little-endian fields, length-prefixed byte
//! strings, and an FNV-1a checksum at the end so a torn or foreign file is
//! refused rather than half-read.  Used by the library index cache and the
//! per-folder capture-time cache.

use std::ffi::OsString;
use std::path::PathBuf;

use chrono::{DateTime, NaiveDateTime};

pub fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(unix)]
pub fn os_from_bytes(b: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(b).to_owned()
}

#[cfg(not(unix))]
pub fn os_from_bytes(b: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(b).into_owned())
}

pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    pub fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
    }
    pub fn path(&mut self, p: &std::path::Path) {
        self.bytes(p.as_os_str().as_encoded_bytes());
    }
    pub fn stamp(&mut self, s: (i64, u32)) {
        self.i64(s.0);
        self.u32(s.1);
    }
    /// An optional date and time: a flag, then seconds and nanoseconds.
    pub fn time(&mut self, t: Option<NaiveDateTime>) {
        match t {
            Some(t) => {
                let utc = t.and_utc();
                self.u8(1);
                self.i64(utc.timestamp());
                self.u32(utc.timestamp_subsec_nanos());
            }
            None => self.u8(0),
        }
    }
    /// The finished file: everything written, then its checksum.
    pub fn seal(mut self) -> Vec<u8> {
        let sum = fnv1a(&self.0);
        self.u64(sum);
        self.0
    }
}

pub struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    /// A reader over a sealed file's contents, if its checksum holds.
    pub fn open(data: &'a [u8]) -> Option<Reader<'a>> {
        let body = data.get(..data.len().checked_sub(8)?)?;
        let sum = u64::from_le_bytes(data[body.len()..].try_into().ok()?);
        (fnv1a(body) == sum).then_some(Reader { b: body, i: 0 })
    }
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
    pub fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    pub fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    pub fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    pub fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    pub fn path(&mut self) -> Option<PathBuf> {
        Some(PathBuf::from(os_from_bytes(self.bytes()?)))
    }
    pub fn stamp(&mut self) -> Option<(i64, u32)> {
        Some((self.i64()?, self.u32()?))
    }
    pub fn time(&mut self) -> Option<Option<NaiveDateTime>> {
        Some(match self.u8()? {
            1 => {
                let (secs, nanos) = (self.i64()?, self.u32()?);
                Some(DateTime::from_timestamp(secs, nanos)?.naive_utc())
            }
            _ => None,
        })
    }
    /// A count, sanity-checked against the bytes left (each item takes at
    /// least `min` bytes), so a corrupt count can't ask for a huge allocation.
    pub fn count(&mut self, min: usize) -> Option<usize> {
        let n = self.u32()? as usize;
        (n.saturating_mul(min) <= self.b.len() - self.i).then_some(n)
    }
    /// Everything was read.
    pub fn done(&self) -> bool {
        self.i == self.b.len()
    }
}
