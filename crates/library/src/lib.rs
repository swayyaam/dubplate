//! Filesystem scanning and library indexing for dubplate.
//!
//! Phase 0 is a read-only walk: find audio files, read their tags, hand them to
//! the UI. The SQLite index, incremental scanning and move detection arrive in
//! phase 1; this crate is where they will live.
//!
//! Nothing here ever writes to the user's files.

pub mod scan;
pub mod track;

pub use scan::{scan_folder, AUDIO_EXTENSIONS};
pub use track::{Lossiness, ScanError, ScanReport, ScannedTrack};
