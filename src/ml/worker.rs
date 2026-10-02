//! The helper process: `photo-pipeline --ml-worker --device gpu|cpu`
//! (plan §4.3).  The same binary, started by [`super::client::Client`],
//! so there is nothing extra to install.  It loads the ONNX Runtime
//! sessions on the chosen device, runs the self-check on the card, reads
//! previews itself (the app sends paths and gets vectors back), batches
//! them, and exits after a few idle minutes so its memory goes back to
//! the system.  A driver fault kills the helper, not the app.
//!
//! **Protocol:** one JSON object per line on stdin (requests) and stdout
//! (replies), in order.  The first reply is [`Reply::Ready`] or
//! [`Reply::CardFailed`].

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::client::ImageResult;
use super::{CardFailure, Device, SelfCheck};
use crate::model::record::FileType;

/// The flag that makes the binary a helper.
pub const FLAG: &str = "--ml-worker";
/// Minutes without a request after which the helper exits.
pub const IDLE_MINUTES: u64 = 3;
/// Photos per inference batch.
pub const BATCH: usize = 32;
/// Threads reading previews (the share is bandwidth-bound; `analysis` uses
/// 4 for the same reason and search reads larger previews).
pub const READERS: usize = 6;

/// What the app asks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "req", rename_all = "snake_case")]
pub enum Request {
    /// Embed these photos; the reply holds one result per path, in order.
    EmbedImages { id: u64, files: Vec<(PathBuf, FileType)> },
    /// Encode query texts on the processor.
    EncodeText { id: u64, texts: Vec<String> },
    /// Exit now.
    Quit,
}

/// What the helper answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    /// Models loaded; on the card, the self-check passed.
    Ready { device: Device, name: Option<String>, self_check: SelfCheck, dim: usize },
    /// The card was asked for and can't be used; the helper exits.
    CardFailed(CardFailure),
    /// The models couldn't be loaded (missing, damaged); the helper exits.
    Failed(String),
    Images { id: u64, results: Vec<ImageResult> },
    Texts { id: u64, vectors: Vec<Vec<f32>> },
    /// Nothing asked for [`IDLE_MINUTES`]: exiting on purpose.
    Idle,
}

/// The helper's main.  `args` are the arguments after [`FLAG`]:
/// `--device gpu|cpu`.  Returns the exit code.
pub fn main(args: &[String]) -> i32 {
    let device = match args.iter().position(|a| a == "--device").and_then(|i| args.get(i + 1)).map(String::as_str) {
        Some("cpu") => Device::Cpu,
        _ => Device::Gpu,
    };
    lower_priority();
    // Phase 1 fills this in: sessions, the self-check, the request loop.
    let reply = Reply::Failed(format!("the search helper isn't built yet (asked for {})", device.label()));
    println!("{}", serde_json::to_string(&reply).unwrap_or_default());
    1
}

/// `nice` and the idle I/O class for the whole helper, so indexing never
/// competes with culling.
fn lower_priority() {
    #[cfg(target_os = "linux")]
    // SAFETY: plain syscalls on this process; failures are harmless.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
        // ioprio_set(IOPRIO_WHO_PROCESS, self, IOPRIO_CLASS_IDLE << 13)
        libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_replies_round_trip_as_one_line_each() {
        let req = Request::EmbedImages { id: 7, files: vec![(PathBuf::from("/x/a.JPG"), FileType::Jpg)] };
        let line = serde_json::to_string(&req).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), req);
        let rep = Reply::Ready { device: Device::Gpu, name: Some("Test GPU".into()), self_check: SelfCheck::Passed { cosine: 1.0, max_diff: 0.0 }, dim: 768 };
        let line = serde_json::to_string(&rep).unwrap();
        assert!(line.starts_with(r#"{"reply":"ready""#));
        assert_eq!(serde_json::from_str::<Reply>(&line).unwrap(), rep);
    }
}
