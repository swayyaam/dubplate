//! Offline analysis: one decode pass per track, six answers out of it.
//!
//! Everything here is a suspicion or a measurement, never a verdict that acts on
//! its own. Nothing in this crate deletes, hides, or rewrites anything.

pub mod analyse;
pub mod depth;
pub mod peaks_cache;
pub mod pipeline;
pub mod key;
pub mod spectral;
pub mod tempo;

pub use analyse::{analyse, TrackAnalysis, PEAK_BUCKETS};
pub use peaks_cache::PeaksCache;
pub use pipeline::{
    analyse_all, remaining, reset, run_batch, store_all, take_pending, AnalysedTrack, BatchReport,
    PendingTrack,
};
