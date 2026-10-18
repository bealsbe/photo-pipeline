pub mod crop_export;
pub mod edit_copy;
pub mod fsutil;
pub mod library;
pub mod netfs;
pub mod separate;
pub mod trash;
pub mod undo;

/// Where a copy or move stands, sent after each finished file.  The sizes
/// are the ones the plan already knows from the records: no stat is made
/// to report them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// Files finished (succeeded or failed) and files to do.
    pub files: usize,
    pub files_total: usize,
    /// Their bytes, and the bytes of every file to do.
    pub bytes: u64,
    pub bytes_total: u64,
    /// The bytes that actually went over: a same-device move is a rename
    /// and carries none, so a speed can't be read from it.
    pub transferred: u64,
    /// The size of the file being worked on now (0 at the end), so a long
    /// file isn't mistaken for a stall.
    pub next: u64,
}
