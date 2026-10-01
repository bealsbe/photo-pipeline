//! Safe wrapper over the LibRaw shim (native/libraw_shim.cpp).
//!
//! Mirrors the two rawpy calls the Python app makes:
//!
//! * [`Raw::thumbnail`] — `extract_thumb()`: the camera's embedded preview,
//!   either JPEG bytes or an already-decoded bitmap.
//! * [`Raw::process`] — `postprocess(use_camera_wb=True, half_size=…,
//!   no_auto_bright=False, output_bps=8)`: a full demosaic to 8-bit RGB,
//!   already rotated by the file's orientation.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::path::Path;

#[repr(C)]
struct PpImage {
    kind: c_int,
    width: c_int,
    height: c_int,
    colors: c_int,
    bits: c_int,
    size: c_uint,
    data: *const u8,
    owner: *mut c_void,
}

impl PpImage {
    fn empty() -> Self {
        Self {
            kind: 0,
            width: 0,
            height: 0,
            colors: 0,
            bits: 0,
            size: 0,
            data: std::ptr::null(),
            owner: std::ptr::null_mut(),
        }
    }
}

unsafe extern "C" {
    fn pp_open(path: *const c_char, err: *mut c_int) -> *mut c_void;
    fn pp_flip(h: *mut c_void) -> c_int;
    fn pp_thumb(h: *mut c_void, out: *mut PpImage) -> c_int;
    fn pp_thumb_min(h: *mut c_void, min_edge: c_int, out: *mut PpImage) -> c_int;
    fn pp_thumb_size(h: *mut c_void, w: *mut c_int, hgt: *mut c_int) -> c_int;
    fn pp_process(h: *mut c_void, half_size: c_int, out: *mut PpImage) -> c_int;
    fn pp_image_free(img: *mut PpImage);
    fn pp_close(h: *mut c_void);
    fn pp_strerror(e: c_int) -> *const c_char;
}

fn error_text(code: c_int) -> String {
    // SAFETY: libraw_strerror returns a pointer to a static string.
    unsafe {
        let p = pp_strerror(code);
        if p.is_null() {
            format!("LibRaw error {code}")
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

/// An open RAW file.  Each instance is independent, so separate threads may
/// each hold their own.
pub struct Raw {
    handle: *mut c_void,
}

// SAFETY: a LibRaw instance may move between threads as long as it is only
// used from one at a time, which `&mut self` on every call guarantees.
unsafe impl Send for Raw {}

/// The embedded preview returned by [`Raw::thumbnail`].
pub enum Thumbnail {
    Jpeg(Vec<u8>),
    /// Interleaved 8-bit samples, `colors` per pixel.
    Bitmap {
        width: u32,
        height: u32,
        colors: u32,
        data: Vec<u8>,
    },
}

/// 8-bit interleaved RGB from a full demosaic.
pub struct Rgb8 {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Raw {
    pub fn open(path: &Path) -> Result<Self, String> {
        let c_path = path_to_cstring(path)?;
        let mut err: c_int = 0;
        // SAFETY: c_path outlives the call; err is a valid out-pointer.
        let handle = unsafe { pp_open(c_path.as_ptr(), &mut err) };
        if handle.is_null() {
            return Err(error_text(err));
        }
        Ok(Self { handle })
    }

    /// LibRaw's orientation expressed as an EXIF Orientation value (1–8).
    pub fn exif_orientation(&mut self) -> u8 {
        // SAFETY: handle is valid for the lifetime of self.
        match unsafe { pp_flip(self.handle) } {
            3 => 3,
            5 => 8,
            6 => 6,
            _ => 1,
        }
    }

    /// Pixel size of the default embedded thumbnail (what `thumbnail`
    /// returns), without unpacking it.
    pub fn default_thumb_size(&mut self) -> Option<(u32, u32)> {
        let (mut w, mut h) = (0, 0);
        // SAFETY: handle is valid; w and h are valid out-pointers.
        let e = unsafe { pp_thumb_size(self.handle, &mut w, &mut h) };
        (e == 0).then_some((w as u32, h as u32))
    }

    pub fn thumbnail(&mut self) -> Result<Thumbnail, String> {
        // SAFETY: handle is valid; out is a valid out-pointer.
        self.take_thumb(|h, out| unsafe { pp_thumb(h, out) })
    }

    /// The smallest embedded JPEG preview with a long edge of at least
    /// `min_edge` (rawpy has no equivalent; `thumbnail` is what it returns).
    /// On `Err`, reopen the file before calling [`Raw::thumbnail`]: choosing
    /// a preview leaves LibRaw's default-thumbnail state changed.
    pub fn thumbnail_at_least(&mut self, min_edge: u32) -> Result<Thumbnail, String> {
        let min = min_edge.min(i32::MAX as u32) as c_int;
        // SAFETY: handle is valid; out is a valid out-pointer.
        self.take_thumb(|h, out| unsafe { pp_thumb_min(h, min, out) })
    }

    fn take_thumb(&mut self, call: impl FnOnce(*mut c_void, *mut PpImage) -> c_int) -> Result<Thumbnail, String> {
        let mut img = PpImage::empty();
        let e = call(self.handle, &mut img);
        if e != 0 {
            return Err(error_text(e));
        }
        let out = copy_image(&img).and_then(|(kind, bytes)| match kind {
            1 => Ok(Thumbnail::Jpeg(bytes)),
            2 if img.bits == 8 && img.colors >= 1 => Ok(Thumbnail::Bitmap {
                width: img.width as u32,
                height: img.height as u32,
                colors: img.colors as u32,
                data: bytes,
            }),
            _ => Err("unsupported thumbnail format".to_string()),
        });
        // SAFETY: img was filled by the call above and is freed exactly once.
        unsafe { pp_image_free(&mut img) };
        out
    }

    pub fn process(&mut self, half_size: bool) -> Result<Rgb8, String> {
        let mut img = PpImage::empty();
        // SAFETY: handle is valid; img is a valid out-pointer.
        let e = unsafe { pp_process(self.handle, half_size as c_int, &mut img) };
        if e != 0 {
            return Err(error_text(e));
        }
        let out = copy_image(&img).and_then(|(_, bytes)| {
            if img.bits != 8 || img.colors != 3 {
                return Err("unexpected LibRaw output format".to_string());
            }
            Ok(Rgb8 {
                width: img.width as u32,
                height: img.height as u32,
                data: bytes,
            })
        });
        // SAFETY: img was filled by pp_process and is freed exactly once.
        unsafe { pp_image_free(&mut img) };
        out
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        // SAFETY: handle came from pp_open and is closed exactly once.
        unsafe { pp_close(self.handle) };
    }
}

fn copy_image(img: &PpImage) -> Result<(c_int, Vec<u8>), String> {
    if img.data.is_null() || img.size == 0 {
        return Err("empty image from LibRaw".to_string());
    }
    // SAFETY: LibRaw guarantees `size` readable bytes at `data` until the
    // image is freed, which happens after this copy.
    let bytes = unsafe { std::slice::from_raw_parts(img.data, img.size as usize) }.to_vec();
    Ok((img.kind, bytes))
}

#[cfg(unix)]
fn path_to_cstring(path: &Path) -> Result<CString, String> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes()).map_err(|_| "path contains NUL".to_string())
}

#[cfg(not(unix))]
fn path_to_cstring(path: &Path) -> Result<CString, String> {
    // LibRaw's narrow-char open on Windows uses the ANSI code page; paths
    // outside it would need libraw_open_wfile.
    CString::new(path.to_string_lossy().as_bytes().to_vec())
        .map_err(|_| "path contains NUL".to_string())
}
