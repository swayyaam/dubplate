//! Collection health: what the library actually is, once every track has been
//! looked at.
//!
//! This falls out of the analysis pass for free, and for a collection assembled
//! from many sources over years it is a more interesting first screen than a
//! genre list. Nothing here deletes or hides anything: it filters and counts.

use anyhow::Result;
use serde::Serialize;

use crate::db::Library;
use crate::model::{TrackRow, TRACK_COLUMNS, TRACK_JOINS};

/// Above this, a lossless container is worth a second look. Chosen so the
/// obvious cases surface without burying them in maybes -- and it is a
/// threshold for *showing* a suspicion, never for acting on one.
pub const SUSPECT_THRESHOLD: f32 = 0.5;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bucket {
    pub label: String,
    pub count: i64,
    pub bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionHealth {
    pub total: i64,
    pub analysed: i64,
    pub lossless: i64,
    pub lossy: i64,
    /// MP4 containers whose codec has not been resolved yet.
    pub unknown: i64,
    /// A bigger container than the audio needs.
    pub padded: i64,
    /// Lossless containers whose spectrum suggests a lossy origin.
    pub suspected: i64,
    pub total_bytes: i64,
    pub total_duration_ms: i64,
    pub codecs: Vec<Bucket>,
    pub sample_rates: Vec<Bucket>,
    pub bit_depths: Vec<Bucket>,
}

pub fn summary(library: &Library) -> Result<CollectionHealth> {
    let conn = library.connection();

    let (total, analysed, lossless, lossy, unknown, bytes, duration): (
        i64, i64, i64, i64, i64, i64, i64,
    ) = conn.query_row(
        "SELECT count(*),
                COALESCE(SUM(analyzed_at IS NOT NULL), 0),
                COALESCE(SUM(is_lossy = 0), 0),
                COALESCE(SUM(is_lossy = 1), 0),
                COALESCE(SUM(is_lossy IS NULL), 0),
                COALESCE(SUM(size), 0),
                COALESCE(SUM(duration_ms), 0)
         FROM tracks",
        [],
        |row| {
            Ok((
                row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                row.get(4)?, row.get(5)?, row.get(6)?,
            ))
        },
    )?;

    // Padded only counts where both numbers are known: an unanalysed file is
    // not evidence of anything.
    let padded: i64 = conn.query_row(
        "SELECT count(*) FROM tracks
         WHERE effective_bits IS NOT NULL AND bit_depth IS NOT NULL
           AND effective_bits < bit_depth",
        [],
        |row| row.get(0),
    )?;

    // Only lossless containers can be *suspected*: an MP3 is not a suspicion,
    // it is simply lossy and says so on the tin.
    let suspected: i64 = conn.query_row(
        "SELECT count(*) FROM tracks
         WHERE is_lossy = 0 AND transcode_score >= ?1",
        [SUSPECT_THRESHOLD],
        |row| row.get(0),
    )?;

    Ok(CollectionHealth {
        total,
        analysed,
        lossless,
        lossy,
        unknown,
        padded,
        suspected,
        total_bytes: bytes,
        total_duration_ms: duration,
        codecs: buckets(library, "COALESCE(codec, 'unknown')")?,
        sample_rates: buckets(library, "COALESCE(CAST(sample_rate AS TEXT), 'unknown')")?,
        bit_depths: buckets(library, "COALESCE(CAST(bit_depth AS TEXT), 'none')")?,
    })
}

fn buckets(library: &Library, expression: &str) -> Result<Vec<Bucket>> {
    let sql = format!(
        "SELECT {expression} AS label, count(*), COALESCE(SUM(size), 0)
         FROM tracks GROUP BY label ORDER BY count(*) DESC"
    );
    let conn = library.connection();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(Bucket {
            label: row.get(0)?,
            count: row.get(1)?,
            bytes: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The tracks behind one number in the health view.
///
/// "Show me every suspected transcode" should be one click, and it should show
/// the evidence rather than a conclusion.
pub fn tracks(library: &Library, filter: &str, limit: usize) -> Result<Vec<TrackRow>> {
    let (clause, order) = match filter {
        "padded" => (
            "t.effective_bits IS NOT NULL AND t.bit_depth IS NOT NULL
             AND t.effective_bits < t.bit_depth",
            "t.bit_depth DESC",
        ),
        "suspected" => (
            "t.is_lossy = 0 AND t.transcode_score >= 0.5",
            "t.transcode_score DESC",
        ),
        "lossless" => ("t.is_lossy = 0", "t.size DESC"),
        "lossy" => ("t.is_lossy = 1", "t.size DESC"),
        "unknown" => ("t.is_lossy IS NULL", "t.size DESC"),
        "unanalysed" => ("t.analyzed_at IS NULL", "t.id"),
        _ => ("1 = 1", "t.added_at DESC"),
    };

    let sql = format!(
        "SELECT {TRACK_COLUMNS} {TRACK_JOINS} WHERE {clause} ORDER BY {order} LIMIT ?1"
    );
    let conn = library.connection();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([limit as i64], TrackRow::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
