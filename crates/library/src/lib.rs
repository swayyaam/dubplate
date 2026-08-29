//! Filesystem scanning and library indexing for dubplate.
//!
//! The filesystem is the source of truth and this index is a cache. Deleting
//! the database and rescanning must lose nothing except play counts and
//! history, so nothing stored here is unrecoverable from disk.
//!
//! Nothing in this crate ever writes to the user's audio files.

pub mod artwork;
pub mod db;
pub mod history;
pub mod index;
pub mod model;
pub mod query;
pub mod scan;
pub mod track;
pub mod watch;

pub use artwork::{ArtworkCache, ArtworkReport};
pub use db::Library;
pub use history::Listen;
pub use index::{sync, SyncReport};
pub use model::{AlbumRow, TrackRow};
pub use query::{album_tracks, list_albums, list_tracks, search};
pub use scan::{scan_folder, AUDIO_EXTENSIONS};
pub use track::{Lossiness, ScanError, ScanReport, ScannedTrack};
pub use watch::{watch, LibraryWatcher, DEFAULT_DEBOUNCE};
