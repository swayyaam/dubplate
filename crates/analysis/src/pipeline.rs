//! The background analysis pass: resumable, throttled, and interruptible.
//!
//! Resumable because it works from `analyzed_at IS NULL` rather than from a
//! position in a list, so stopping it halfway costs nothing. Throttled because
//! the machine has to stay usable, and because the audio callback matters more
//! than finishing sooner.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use dubplate_library::Library;
use rayon::prelude::*;
use rusqlite::params;
use serde::Serialize;

use crate::analyse::{analyse, TrackAnalysis};
use crate::peaks_cache::PeaksCache;

/// A track waiting to be analysed.
#[derive(Debug, Clone)]
pub struct PendingTrack {
    pub id: i64,
    pub path: PathBuf,
    /// Peaks are cached against this, so a file that moves keeps its waveform.
    pub content_key: String,
}

/// A finished analysis, ready to be written.
pub struct AnalysedTrack {
    pub id: i64,
    pub content_key: String,
    /// `None` when the file would not decode.
    pub analysis: Option<TrackAnalysis>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchReport {
    pub analysed: usize,
    pub failed: usize,
    /// Still waiting after this batch. Zero means the library is done.
    pub remaining: i64,
    pub elapsed_ms: u64,
}

/// How many tracks are still unanalysed.
pub fn remaining(library: &Library) -> Result<i64> {
    Ok(library.connection().query_row(
        "SELECT count(*) FROM tracks WHERE analyzed_at IS NULL",
        [],
        |row| row.get(0),
    )?)
}

/// Forget every stored analysis, so the next pass redoes it.
pub fn reset(library: &Library) -> Result<usize> {
    Ok(library.connection().execute(
        "UPDATE tracks SET analyzed_at = NULL, bpm = NULL, music_key = NULL,
                           effective_bits = NULL, spectral_cutoff = NULL,
                           transcode_score = NULL",
        [],
    )?)
}

/// Claim the next tracks to analyse.
///
/// Split from the work on purpose: analysis takes seconds and the database
/// lock has to be free during it, or every query in the app waits behind a
/// background job.
pub fn take_pending(library: &Library, batch: usize) -> Result<Vec<PendingTrack>> {
    let conn = library.connection();
    let mut stmt = conn.prepare(
        "SELECT id, path, content_key FROM tracks
         WHERE analyzed_at IS NULL
         ORDER BY id
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([batch as i64], |row| {
        Ok(PendingTrack {
            id: row.get(0)?,
            path: PathBuf::from(row.get::<_, String>(1)?),
            content_key: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Decode and analyse, holding no lock on anything.
///
/// `threads` is deliberately the caller's decision: the right number while
/// music is playing is not the right number while the app is idle.
pub fn analyse_all(pending: &[PendingTrack], threads: usize) -> Vec<AnalysedTrack> {
    if pending.is_empty() {
        return Vec::new();
    }
    let pool = match rayon::ThreadPoolBuilder::new()
        .num_threads(threads.max(1))
        .build()
    {
        Ok(pool) => pool,
        Err(err) => {
            tracing::error!(%err, "cannot build the analysis pool");
            return Vec::new();
        }
    };

    pool.install(|| {
        pending
            .par_iter()
            .map(|track| AnalysedTrack {
                id: track.id,
                content_key: track.content_key.clone(),
                analysis: analyse(&track.path).ok(),
            })
            .collect()
    })
}

/// Write a finished batch: peaks to disk, everything else to the index.
pub fn store_all(
    library: &mut Library,
    peaks: &PeaksCache,
    results: &[AnalysedTrack],
) -> Result<BatchReport> {
    let started = Instant::now();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut report = BatchReport::default();
    let tx = library.connection_mut().transaction()?;
    for result in results {
        match &result.analysis {
            Some(analysis) => {
                // Peaks are a thousand floats; they belong on disk, not in a
                // column that every track query would carry.
                let _ = peaks.write(&result.content_key, &analysis.peaks);
                store(&tx, result.id, analysis, now)?;
                report.analysed += 1;
            }
            None => {
                // Marked done regardless. A file that will not decode will not
                // decode next time either, and retrying it every pass would
                // mean the analysis never finishes.
                tx.execute(
                    "UPDATE tracks SET analyzed_at = ?2 WHERE id = ?1",
                    params![result.id, now],
                )?;
                report.failed += 1;
            }
        }
    }
    tx.commit()?;

    report.remaining = remaining(library)?;
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// Take, analyse and store in one call. Convenient for a command line; the app
/// uses the three parts so it can hold the lock for as little as possible.
pub fn run_batch(
    library: &mut Library,
    peaks: &PeaksCache,
    batch: usize,
    threads: usize,
) -> Result<BatchReport> {
    let pending = take_pending(library, batch)?;
    if pending.is_empty() {
        return Ok(BatchReport::default());
    }
    let results = analyse_all(&pending, threads);
    store_all(library, peaks, &results)
}

fn store(
    tx: &rusqlite::Transaction<'_>,
    id: i64,
    analysis: &TrackAnalysis,
    now: i64,
) -> Result<()> {
    tx.execute(
        "UPDATE tracks SET
            rg_track_gain   = ?2,
            rg_track_peak   = ?3,
            bpm             = ?4,
            music_key       = ?5,
            effective_bits  = ?6,
            spectral_cutoff = ?7,
            transcode_score = ?8,
            analyzed_at     = ?9
         WHERE id = ?1",
        params![
            id,
            analysis.replay_gain_db,
            analysis.true_peak,
            analysis.bpm,
            analysis.key.as_ref().map(|key| key.camelot.clone()),
            analysis.effective_bits,
            analysis.spectral_cutoff,
            analysis.transcode_score,
            now,
        ],
    )?;
    Ok(())
}
