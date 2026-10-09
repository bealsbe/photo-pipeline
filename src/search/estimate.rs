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
/// Seconds of finding faces per shot on each device, beyond the read the
/// search step already pays: the larger decode for the detector's canvas,
/// the canvas, YuNet, and the full decode of the photos with faces.
/// Measured on 200 previews (phase 3): 11.8 ms a photo on the processor
/// with SFace (whose recognition is nearly free), ~10 ms on the card
/// (15.4 ms with faces against 5.6 without).
pub const DETECT_SECONDS_PER_SHOT_CPU: f64 = 0.0115;
pub const DETECT_SECONDS_PER_SHOT_GPU: f64 = 0.010;

/// Seconds per face for a face model on the processor (measured, phase
/// 3, over the detection cost: AuraFace 12 ms, InsightFace 5 ms, SFace
/// about 1 ms, at eight threads and batches of up to 32) and on the card
/// (about a tenth).
pub fn face_seconds_per_face(device: Device, model: ModelId) -> f64 {
    let cpu = match model {
        ModelId::AuraFace => 0.012,
        ModelId::InsightFace => 0.005,
        ModelId::SFace => 0.001,
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
pub fn face_model_change_seconds(shots_with_faces: usize, faces: usize, device: Device, model: ModelId) -> f64 {
    let read = shots_with_faces as f64 * SECONDS_PER_SHOT_GPU;
    let work = faces as f64 * face_seconds_per_face(device, model);
    match device {
        Device::Gpu => read.max(work),
        Device::Cpu => read + work,
    }
}

/// Seconds per fursuit photo for the head step (OWLv2 over the preview,
/// then DINOv2 on each head), **measured with the helper's own graphs**
/// on the study's previews (2026-10-03, `ml::heads` ignored test): on the
/// card 0.17 s for OWLv2 one photo at a time plus the heads' DINOv2; on
/// the processor (8 threads) 4.0 s for OWLv2 and 4.5 s in all, three and
/// a half times the study's PyTorch figure, so the library's ~42 000
/// fursuit photos are about 50 hours there (decision 11: it asks every
/// time).
pub const HEAD_SECONDS_PER_PHOTO_GPU: f64 = 0.3;
pub const HEAD_SECONDS_PER_PHOTO_CPU: f64 = 4.5;

/// Seconds to look for heads in `photos` fursuit photos on `device`,
/// beyond reading them.
pub fn head_seconds(photos: usize, device: Device) -> f64 {
    photos as f64
        * match device {
            Device::Gpu => HEAD_SECONDS_PER_PHOTO_GPU,
            Device::Cpu => HEAD_SECONDS_PER_PHOTO_CPU,
        }
}

/// Seconds for a head scan over `photos` fursuit photos already indexed
/// (a catch-up, or new exports): the previews are read again and the two
/// models run; inference dominates on both devices.  This is the number
/// the Fursuits tab and the processor's question quote (decision 11).
pub fn head_catch_up_seconds(photos: usize, device: Device) -> f64 {
    let read = photos as f64 * SECONDS_PER_SHOT_GPU;
    let work = head_seconds(photos, device);
    match device {
        Device::Gpu => read.max(work),
        Device::Cpu => read + work,
    }
}

/// Seconds per photo for the v2 catch-up (the search model's vectors
/// of heads already found: the preview read again, each head cropped
/// and through the image tower; no detector), **measured through the
/// helper** on the research's 7 033 photos (10 826 heads, 2026-10-09,
/// `heads::store_tests::fursuit_v2_vectors_of_the_research_heads`):
/// 0.015 s a photo on the card with the previews in the page cache, and
/// 0.25 s a photo on the processor (300 photos).
pub const HEAD_VECTOR_SECONDS_PER_PHOTO_GPU: f64 = 0.015;
pub const HEAD_VECTOR_SECONDS_PER_PHOTO_CPU: f64 = 0.25;

/// Seconds for the v2 catch-up over `photos` photos with heads.
pub fn head_vector_seconds(photos: usize, device: Device) -> f64 {
    let read = photos as f64 * SECONDS_PER_SHOT_GPU;
    let work = photos as f64
        * match device {
            Device::Gpu => HEAD_VECTOR_SECONDS_PER_PHOTO_GPU,
            Device::Cpu => HEAD_VECTOR_SECONDS_PER_PHOTO_CPU,
        };
    match device {
        Device::Gpu => read.max(work),
        Device::Cpu => read + work,
    }
}

/// Bytes read from the library by the v2 catch-up (each preview again).
pub fn head_vector_bytes_read(photos: usize) -> u64 {
    bytes_read(photos)
}

/// The close-call judge (v2): seconds a pair, both orders, and the load
/// onto the card, **measured through the helper** on the RX 7900 XTX
/// (2026-10-09): 2.06–2.12 s a pair (the research's server: 2.30), the
/// load 2.3–5.8 s with the files in the page cache (a cold disk reads
/// 18 GB first).
pub const JUDGE_SECONDS_PER_PAIR: f64 = 2.1;
pub const JUDGE_LOAD_SECONDS: f64 = 6.0;
/// Qwen3-VL-Reranker (Q8_0, 280 image tokens), **measured through the
/// helper** on the RX 7900 XTX (2026-10-09): 0.77–0.79 s a pair, both
/// orders (the research's server: 0.86), and 1.8–3.5 s to load with the
/// files cached.
pub const QWEN_SECONDS_PER_PAIR: f64 = 0.8;
pub const QWEN_LOAD_SECONDS: f64 = 3.0;

/// Seconds a close call (a pair, both orders) with `choice`.
pub fn judge_seconds_per_pair(choice: crate::ml::judge::JudgeChoice) -> f64 {
    match choice {
        crate::ml::judge::JudgeChoice::Qwen3VlReranker => QWEN_SECONDS_PER_PAIR,
        crate::ml::judge::JudgeChoice::Gemma4 => JUDGE_SECONDS_PER_PAIR,
        crate::ml::judge::JudgeChoice::Off => 0.0,
    }
}

/// Seconds to load `choice` onto the card (files cached).
pub fn judge_load_seconds(choice: crate::ml::judge::JudgeChoice) -> f64 {
    match choice {
        crate::ml::judge::JudgeChoice::Qwen3VlReranker => QWEN_LOAD_SECONDS,
        crate::ml::judge::JudgeChoice::Gemma4 => JUDGE_LOAD_SECONDS,
        crate::ml::judge::JudgeChoice::Off => 0.0,
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

    /// Measured (plan, "As built — phase 4"): about 50 hours on the
    /// processor for the library's fursuit photos (74 % of 56 710 ≈
    /// 42 000), over four hours an event day; about 3.5 hours on the card.
    #[test]
    fn head_estimates_follow_the_measurements() {
        let photos = 42_000;
        let cpu = head_catch_up_seconds(photos, Device::Cpu) / 3600.0;
        let gpu = head_catch_up_seconds(photos, Device::Gpu) / 3600.0;
        assert!((50.0..55.0).contains(&cpu), "{cpu} h");
        assert!((3.0..4.0).contains(&gpu), "{gpu} h");
        assert!(head_catch_up_seconds(3_500, Device::Cpu) > 3600.0, "an event day is over an hour on the processor");
        assert!(head_catch_up_seconds(100, Device::Gpu) >= head_seconds(100, Device::Gpu), "never under the work itself");
    }

    /// Measured in phase 3: a catch-up over the library is about ten
    /// minutes on the card (the extra decodes outweigh the read) and
    /// 11–14 minutes on the processor; the plan's +7–14 minutes held.
    #[test]
    fn face_estimates_follow_the_measurements() {
        let n = 56_710;
        assert_eq!(describe_seconds(face_catch_up_seconds(n, Device::Gpu, ModelId::AuraFace)), "about 10 minutes");
        assert_eq!(describe_seconds(face_catch_up_seconds(n, Device::Gpu, ModelId::SFace)), "about 9 minutes");
        let cpu_aura = face_seconds(n, Device::Cpu, ModelId::AuraFace);
        let cpu_sface = face_seconds(n, Device::Cpu, ModelId::SFace);
        assert!((12.0 * 60.0..=15.0 * 60.0).contains(&cpu_aura), "{cpu_aura}");
        assert!((10.0 * 60.0..=12.0 * 60.0).contains(&cpu_sface), "{cpu_sface}");
        assert!(face_seconds(n, Device::Cpu, ModelId::InsightFace) < cpu_aura);
        assert!(face_seconds(n, Device::Gpu, ModelId::AuraFace) < cpu_aura);
        assert_eq!(face_seconds_per_face(Device::Cpu, ModelId::Siglip2), 0.0, "not a face model");
        // A model change over a quarter of the library with 14 k faces:
        // read-bound on the card, a few minutes on the processor.
        assert_eq!(describe_seconds(face_model_change_seconds(14_000, 14_000, Device::Gpu, ModelId::AuraFace)), "about 2 minutes");
        assert!(face_model_change_seconds(14_000, 14_000, Device::Cpu, ModelId::AuraFace) > 4.0 * 60.0);
    }
}
