//! The numbers the "Not indexed yet" page and the download dialog quote
//! (plan §3.7 and §7a.4), as pure functions of the shot count.

use crate::ml::Device;
use crate::ml::models::ModelId;

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

/// Faces per shot in this library: 8–21 k faces of at least 40 px in
/// 56 710 shots (plan §3.3, sample L at YuNet score ≥ 0.9 gives ~0.14;
/// event days hold more).
pub const FACES_PER_SHOT: f64 = 0.25;
/// Seconds of detection per shot on each device, beyond the read the
/// search step already pays (YuNet at half size: 3.2 ms on the processor;
/// a batch on the card is far cheaper).
pub const DETECT_SECONDS_PER_SHOT_CPU: f64 = 0.0035;
pub const DETECT_SECONDS_PER_SHOT_GPU: f64 = 0.0005;

/// Seconds per face for a face model on the processor (plan §3.3:
/// AuraFace 30 ms, SFace 3–4 ms, InsightFace about half AuraFace's work)
/// and on the card (about a tenth).
pub fn face_seconds_per_face(device: Device, model: ModelId) -> f64 {
    let cpu = match model {
        ModelId::AuraFace => 0.030,
        ModelId::InsightFace => 0.015,
        ModelId::SFace => 0.0035,
        _ => 0.0,
    };
    match device {
        Device::Cpu => cpu,
        Device::Gpu => cpu / 10.0,
    }
}

/// Seconds to find and recognise the faces in `shots` with `model` on
/// `device`, on top of reading them.  A catch-up over an indexed library
/// reads every preview again (`seconds(shots, Gpu)` is the read-bound
/// floor), so [`face_catch_up_seconds`] is the number the invitation
/// quotes; during the first index the read is already paid for.
pub fn face_seconds(shots: usize, device: Device, model: ModelId) -> f64 {
    let detect = match device {
        Device::Gpu => DETECT_SECONDS_PER_SHOT_GPU,
        Device::Cpu => DETECT_SECONDS_PER_SHOT_CPU,
    };
    shots as f64 * (detect + FACES_PER_SHOT * face_seconds_per_face(device, model))
}

/// Seconds for a face catch-up over `shots` already indexed: the previews
/// are read again (the read-bound time on the card), and the faces found
/// and recognised; the slower of reading and inference decides.
#[allow(dead_code)] // Phase 3 UI: the People tab's invitation; called when it lands.
pub fn face_catch_up_seconds(shots: usize, device: Device, model: ModelId) -> f64 {
    let read = shots as f64 * SECONDS_PER_SHOT_GPU;
    let work = face_seconds(shots, device, model);
    match device {
        Device::Gpu => read.max(work),
        Device::Cpu => read + work,
    }
}

/// Seconds to recognise again the faces already found in `shots` with
/// faces (a face-model change: the photos with faces are read again,
/// about a quarter of the library, and only the face model runs).
#[allow(dead_code)] // Phase 3 UI: Preferences, People, Change…; called when it lands.
pub fn face_model_change_seconds(shots_with_faces: usize, faces: usize, device: Device, model: ModelId) -> f64 {
    let read = shots_with_faces as f64 * SECONDS_PER_SHOT_GPU;
    let work = faces as f64 * face_seconds_per_face(device, model);
    match device {
        Device::Gpu => read.max(work),
        Device::Cpu => read + work,
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

    /// Plan §3.7: faces add a few minutes on the card and 4–15 on the
    /// processor; a catch-up is bound by reading the previews again.
    #[test]
    fn face_estimates_follow_the_plan() {
        let n = 56_710;
        assert_eq!(describe_seconds(face_catch_up_seconds(n, Device::Gpu, ModelId::AuraFace)), "about 7 minutes", "read-bound on the card");
        assert_eq!(describe_seconds(face_catch_up_seconds(n, Device::Gpu, ModelId::SFace)), "about 7 minutes");
        let cpu_aura = face_seconds(n, Device::Cpu, ModelId::AuraFace);
        let cpu_sface = face_seconds(n, Device::Cpu, ModelId::SFace);
        assert!((7.0 * 60.0..=15.0 * 60.0).contains(&cpu_aura), "{cpu_aura}");
        assert!((3.0 * 60.0..=5.0 * 60.0).contains(&cpu_sface), "{cpu_sface}");
        assert!(face_seconds(n, Device::Cpu, ModelId::InsightFace) < cpu_aura);
        assert!(face_seconds(n, Device::Gpu, ModelId::AuraFace) < cpu_aura / 5.0);
        assert_eq!(face_seconds_per_face(Device::Cpu, ModelId::Siglip2), 0.0, "not a face model");
        // A model change over a quarter of the library with 14 k faces.
        assert_eq!(describe_seconds(face_model_change_seconds(14_000, 14_000, Device::Gpu, ModelId::AuraFace)), "about 2 minutes");
        assert!(face_model_change_seconds(14_000, 14_000, Device::Cpu, ModelId::AuraFace) > 5.0 * 60.0);
    }
}
