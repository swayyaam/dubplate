//! Play counts and listening history.
//!
//! All of this lives in the database and never in tags: nothing dubplate does
//! writes to the user's audio files. Deleting the index and rescanning loses
//! play counts and history, and nothing else.

use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::db::Library;

/// One finished listen, as the player reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Listen {
    pub track_id: i64,
    /// The furthest point reached, not wall-clock time.
    pub ms_played: u64,
    /// Crossed the halfway mark. Anything less counts as a skip.
    pub completed: bool,
}

/// Bank a batch of listens: counts on the track, and a row in the history.
pub fn record(library: &mut Library, listens: &[Listen], now: i64) -> Result<usize> {
    if listens.is_empty() {
        return Ok(0);
    }

    let tx = library.connection_mut().transaction()?;
    let mut recorded = 0usize;
    for listen in listens {
        let touched = if listen.completed {
            tx.execute(
                "UPDATE tracks SET play_count = play_count + 1, last_played = ?2 WHERE id = ?1",
                params![listen.track_id, now],
            )?
        } else {
            tx.execute(
                "UPDATE tracks SET skip_count = skip_count + 1 WHERE id = ?1",
                params![listen.track_id],
            )?
        };
        // The track left the library mid-listen. Not worth failing the batch.
        if touched == 0 {
            continue;
        }
        tx.execute(
            "INSERT INTO play_history (track_id, played_at, ms_played, completed)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                listen.track_id,
                now,
                listen.ms_played as i64,
                i64::from(listen.completed)
            ],
        )?;
        recorded += 1;
    }
    tx.commit()?;
    Ok(recorded)
}

/// Title, artist and album for one track, for the system Now Playing panel.
pub fn summary(library: &Library, track_id: i64) -> Option<(String, String, String)> {
    library
        .connection()
        .query_row(
            "SELECT COALESCE(NULLIF(t.title, ''), t.path),
                    COALESCE(ar.name, ''),
                    COALESCE(al.title, '')
             FROM tracks t
             LEFT JOIN artists ar ON ar.id = t.artist_id
             LEFT JOIN albums  al ON al.id = t.album_id
             WHERE t.id = ?1",
            [track_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok()
}
