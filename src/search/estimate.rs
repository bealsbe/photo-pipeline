//! The numbers the "Not indexed yet" page and the download dialog quote
//! (plan §3.7 and §7a.4), as pure functions of the shot count.

use crate::ml::Device;

/// Bytes read per shot: one ~1616-px preview (measured 0.34–0.6 MB).
pub const BYTES_READ_PER_SHOT: u64 = 350_000;
/// Bytes the index keeps per shot: a 768-d f16 vector plus key, path and
/// capture info (87 MB + 7 MB for 56 710 shots).
pub const BYTES_KEPT_PER_SHOT: u64 = 1_660;
/// Seconds per shot on each device.  The card is bound by reading from
/// the share (~6–7 min for the library); the processor by inference
/// (~45 min on 4 threads).
pub const SECONDS_PER_SHOT_GPU: f64 = 0.0075;
pub const SECONDS_PER_SHOT_CPU: f64 = 0.048;

/// Bytes read from the library to index `shots`.
pub fn bytes_read(shots: usize) -> u64 {
    shots as u64 * BYTES_READ_PER_SHOT
}

/// Bytes the index takes for `shots`.
pub fn bytes_kept(shots: usize) -> u64 {
    shots as u64 * BYTES_KEPT_PER_SHOT
}

/// Seconds to index `shots` on `device`.
pub fn seconds(shots: usize, device: Device) -> f64 {
    shots as f64
        * match device {
            Device::Gpu => SECONDS_PER_SHOT_GPU,
            Device::Cpu => SECONDS_PER_SHOT_CPU,
        }
}

/// A duration in the invitation's words: "about 7 minutes", "about an
/// hour", "about 1.4 hours", "under a minute".
pub fn describe_seconds(s: f64) -> String {
    if s < 60.0 {
        "under a minute".into()
    } else if s < 90.0 {
        "about a minute".into()
    } else if s < 55.0 * 60.0 {
        format!("about {} minutes", (s / 60.0).round() as u64)
    } else if s < 65.0 * 60.0 {
        "about an hour".into()
    } else {
        let h = s / 3600.0;
        if h < 10.0 { format!("about {h:.1} hours") } else { format!("about {} hours", h.round() as u64) }
    }
}

/// "about 20 GB", "about 350 MB": a size for the invitation.
pub fn describe_bytes(n: u64) -> String {
    if n >= 10_000_000_000 {
        format!("about {} GB", (n as f64 / 1e9).round() as u64)
    } else if n >= 1_000_000_000 {
        format!("about {:.1} GB", n as f64 / 1e9)
    } else if n >= 1_000_000 {
        format!("about {} MB", (n as f64 / 1e6).round() as u64)
    } else {
        format!("about {} KB", (n as f64 / 1e3).round().max(1.0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_sized_estimates_match_the_plan() {
        let n = 56_710;
        assert_eq!(describe_bytes(bytes_read(n)), "about 20 GB");
        assert_eq!(describe_seconds(seconds(n, Device::Gpu)), "about 7 minutes");
        assert_eq!(describe_seconds(seconds(n, Device::Cpu)), "about 45 minutes");
        assert_eq!(describe_bytes(bytes_kept(n)), "about 94 MB");
        assert_eq!(describe_seconds(seconds(5_000, Device::Gpu)), "under a minute");
        assert_eq!(describe_seconds(seconds(5_000, Device::Cpu)), "about 4 minutes");
        assert_eq!(describe_seconds(3_600.0), "about an hour");
        assert_eq!(describe_seconds(5_040.0), "about 1.4 hours");
        assert_eq!(describe_seconds(15.0 * 3600.0), "about 15 hours");
    }
}
