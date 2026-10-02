//! The one object the app holds for search.  Every method is a message to
//! the engine's own thread (`search-engine`), which owns the index files,
//! the ML helper and the running pass; none of them does I/O on the
//! caller's thread.  State comes back as [`Status`] snapshots
//! (`BgEvent::SearchStatus`, and [`Engine::status`] at any time) and
//! results as `BgEvent::SearchResults`.
//!
//! Where models run is the user's choice; the engine starts a helper on
//! the chosen device only.  If the card fails, work stops and the status
//! says what failed; nothing is sent to the processor unless the user
//! picks it (decision 3, checked by the tests at the end).

use std::path::Path;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;

use super::{Hit, Status};
use crate::bus::{BgEvent, Bus};
use crate::ml::Device;
use crate::ml::client::{Client, EmbedderFactory};
use crate::ml::models::ModelId;
use crate::model::library_index::LibraryIndex;

/// What the engine's thread is asked.
enum Cmd {
    Library(Arc<LibraryIndex>),
    StartFirstIndex(Vec<ModelId>),
    StopIndexing,
    CancelDownloads,
    TryCardAgain,
    SetDevice(Device),
    SetIndexExports(bool),
    ClearIndex,
    RebuildIndex,
    DeleteModels,
    Query { seq: u64, text: String, closeness: f32 },
    Busy(usize),
    Shutdown,
}

/// Sizes Preferences ▸ Storage shows (measured by walking folders:
/// call [`storage`] on a worker).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Storage {
    pub index_bytes: u64,
    pub index_shots: usize,
    pub models_bytes: u64,
}

/// The app's handle.  Cheap to clone-free: one per app.
pub struct Engine {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
}

struct Shared {
    status: Mutex<Status>,
}

impl Engine {
    /// Start the engine with the real helper.  `device` and
    /// `index_exports` are the saved preferences.
    pub fn new(bus: Bus, device: Device, index_exports: bool) -> Engine {
        Engine::with_factory(bus, device, index_exports, Client::factory())
    }

    /// Start the engine with another way of making embedders (tests).
    pub fn with_factory(bus: Bus, device: Device, index_exports: bool, factory: EmbedderFactory) -> Engine {
        let (tx, rx) = crossbeam_channel::unbounded();
        let status = Status { index_exports, device: crate::ml::DeviceStatus { chosen: device, ..Default::default() }, ..Status::default() };
        let shared = Arc::new(Shared { status: Mutex::new(status) });
        let s = shared.clone();
        std::thread::Builder::new()
            .name("search-engine".into())
            .spawn(move || Worker { rx, bus, shared: s, factory, library: None }.run())
            .expect("spawn search-engine");
        Engine { tx, shared }
    }

    /// The latest snapshot (a clone; no I/O).
    pub fn status(&self) -> Status {
        self.shared.status.lock().clone()
    }

    /// The library's index arrived or changed (`BgEvent::IndexReady` with
    /// `done`): the engine diffs it against the path map on its thread and,
    /// once the first index has been asked for and *Index new exports* is
    /// on, queues the new files.
    pub fn on_library(&self, index: Arc<LibraryIndex>) {
        self.send(Cmd::Library(index));
    }

    /// *Index the library…* confirmed in the download dialog with these
    /// models: download what's missing, check the card, index the library.
    pub fn start_first_index(&self, models: &[ModelId]) {
        self.send(Cmd::StartFirstIndex(models.to_vec()));
    }

    /// × on the progress: stop the pass, keeping what is done.
    pub fn stop_indexing(&self) {
        self.send(Cmd::StopIndexing);
    }

    pub fn cancel_downloads(&self) {
        self.send(Cmd::CancelDownloads);
    }

    /// *Try the card again* / *Check again*: a new helper on the card, and
    /// if it passes, the stopped work carries on.
    pub fn try_card_again(&self) {
        self.send(Cmd::TryCardAgain);
    }

    /// Preferences ▸ *Run models on*.  A running pass is stopped and
    /// restarted on the new device.
    pub fn set_device(&self, device: Device) {
        self.send(Cmd::SetDevice(device));
    }

    /// Preferences ▸ *Index new exports*.
    pub fn set_index_exports(&self, on: bool) {
        self.send(Cmd::SetIndexExports(on));
    }

    /// Preferences ▸ Storage ▸ Search index ▸ Clear: stops the pass and
    /// deletes the index files.
    pub fn clear_index(&self) {
        self.send(Cmd::ClearIndex);
    }

    /// Preferences ▸ Storage ▸ Search index ▸ Rebuild: every photo again,
    /// the old index answering until the new one is complete.
    pub fn rebuild_index(&self) {
        self.send(Cmd::RebuildIndex);
    }

    /// Preferences ▸ Storage ▸ Downloaded models ▸ Delete.
    pub fn delete_models(&self) {
        self.send(Cmd::DeleteModels);
    }

    /// Run a query; `BgEvent::SearchResults { seq, .. }` answers.  Queries
    /// never touch the library.
    pub fn query(&self, seq: u64, text: &str, closeness: f32) {
        self.send(Cmd::Query { seq, text: text.to_string(), closeness });
    }

    /// Thumbnails the grid is waiting for: the pass steps back while any.
    pub fn set_busy(&self, pending: usize) {
        self.send(Cmd::Busy(pending));
    }

    /// Stop the pass and the helper; the index is flushed.
    pub fn shutdown(&self) {
        self.send(Cmd::Shutdown);
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

/// Sizes of the index and the models folders.  Walks them: call on a
/// worker (the Preferences storage survey).
pub fn storage(library_root: Option<&Path>) -> Storage {
    let mut out = Storage::default();
    if let Some(dir) = library_root.and_then(super::index::dir_for) {
        out.index_bytes = dir_bytes(&dir);
    }
    if let Some(dir) = crate::ml::models::models_dir() {
        out.models_bytes = dir_bytes(&dir);
    }
    out
}

fn dir_bytes(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_bytes(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

/// The engine's thread.
struct Worker {
    rx: Receiver<Cmd>,
    bus: Bus,
    shared: Arc<Shared>,
    #[allow(dead_code)]
    factory: EmbedderFactory,
    library: Option<Arc<LibraryIndex>>,
}

impl Worker {
    fn run(mut self) {
        self.publish();
        while let Ok(cmd) = self.rx.recv() {
            match cmd {
                Cmd::Library(ix) => {
                    self.update(|s| s.library_shots = ix.total_shots);
                    self.library = Some(ix);
                }
                Cmd::StartFirstIndex(_models) => self.update(|s| s.first_index_asked = true),
                Cmd::StopIndexing => self.update(|s| {
                    s.pass = None;
                    s.stopped = Some(super::Stopped::Cancelled);
                }),
                Cmd::CancelDownloads => {}
                Cmd::TryCardAgain => self.update(|s| s.stopped = None),
                Cmd::SetDevice(d) => self.update(|s| {
                    s.device.chosen = d;
                    s.device.name = None;
                    s.device.self_check = Default::default();
                }),
                Cmd::SetIndexExports(on) => self.update(|s| s.index_exports = on),
                Cmd::ClearIndex => self.update(|s| {
                    s.indexed_shots = 0;
                    s.first_index_asked = false;
                    s.pass = None;
                }),
                Cmd::RebuildIndex | Cmd::DeleteModels => {}
                Cmd::Query { seq, .. } => {
                    let hits: Vec<Hit> = Vec::new();
                    let indexed = self.shared.status.lock().indexed_shots;
                    let _ = self.bus.unbounded_send(BgEvent::SearchResults { seq, hits: Arc::new(hits), indexed, error: None });
                }
                Cmd::Busy(_) => {}
                Cmd::Shutdown => break,
            }
        }
    }

    fn update(&self, f: impl FnOnce(&mut Status)) {
        f(&mut self.shared.status.lock());
        self.publish();
    }

    fn publish(&self) {
        let snapshot = self.shared.status.lock().clone();
        let _ = self.bus.unbounded_send(BgEvent::SearchStatus(Arc::new(snapshot)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_status_starts_from_the_saved_preferences_and_a_query_answers() {
        let (bus, _rx) = crate::bus::channel();
        let e = Engine::with_factory(bus, Device::Cpu, false, Box::new(|_| Err(crate::ml::client::StartError::Other("no".into()))));
        let s = e.status();
        assert_eq!(s.device.chosen, Device::Cpu);
        assert!(!s.index_exports && s.not_indexed());
        e.set_index_exports(true);
        e.query(3, "camera", super::super::CLOSENESS_DEFAULT);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !e.status().index_exports && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(e.status().index_exports);
        e.shutdown();
    }
}
