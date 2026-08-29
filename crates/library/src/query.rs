use anyhow::Result;
use rusqlite::params;

use crate::db::Library;
use crate::model::{AlbumRow, TrackRow, TRACK_COLUMNS, TRACK_JOINS};

/// Album order, the way a tracklist is meant to read. Untagged rows sort last
/// within their group rather than first, which is where SQLite would put NULLs.
const TRACK_ORDER: &str = "
    ORDER BY COALESCE(aa.name, ar.name, '\u{ffff}') COLLATE NOCASE,
             COALESCE(al.title, '\u{ffff}') COLLATE NOCASE,
             COALESCE(t.disc_no, 2147483647),
             COALESCE(t.track_no, 2147483647),
             COALESCE(t.title, t.path) COLLATE NOCASE
";

/// Every track in the library, in album order.
pub fn list_tracks(library: &Library) -> Result<Vec<TrackRow>> {
    let sql = format!("SELECT {TRACK_COLUMNS} {TRACK_JOINS} {TRACK_ORDER}");
    let conn = library.connection();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], TrackRow::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Every album, newest additions first, with a summary of what is in it.
///
/// The format columns use `MIN(x) = MAX(x)` to collapse to a single value only
/// when every track agrees. An album that is half FLAC and half MP3 gets no
/// badge, which is more honest than picking one.
pub fn list_albums(library: &Library) -> Result<Vec<AlbumRow>> {
    let conn = library.connection();
    let mut stmt = conn.prepare(
        "SELECT al.id,
                al.title,
                aa.name,
                al.year,
                al.art_hash,
                COUNT(t.id),
                COALESCE(SUM(t.duration_ms), 0),
                CASE WHEN MIN(t.codec) = MAX(t.codec) THEN MIN(t.codec) END,
                CASE WHEN MIN(t.sample_rate) = MAX(t.sample_rate) THEN MIN(t.sample_rate) END,
                CASE WHEN MIN(t.bit_depth) = MAX(t.bit_depth) THEN MIN(t.bit_depth) END,
                CASE WHEN MIN(t.is_lossy) = MAX(t.is_lossy) THEN MIN(t.is_lossy) END
         FROM albums al
         JOIN tracks t ON t.album_id = al.id
         LEFT JOIN artists aa ON aa.id = al.album_artist_id
         GROUP BY al.id
         ORDER BY COALESCE(aa.name, '\u{ffff}') COLLATE NOCASE,
                  al.year,
                  al.title COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], AlbumRow::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// One album's tracklist, in disc and track order.
pub fn album_tracks(library: &Library, album_id: i64) -> Result<Vec<TrackRow>> {
    let sql = format!(
        "SELECT {TRACK_COLUMNS} {TRACK_JOINS}
         WHERE t.album_id = ?1
         ORDER BY COALESCE(t.disc_no, 1), COALESCE(t.track_no, 2147483647),
                  COALESCE(t.title, t.path) COLLATE NOCASE"
    );
    let conn = library.connection();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![album_id], TrackRow::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Full-text search, ranked by a blend of match quality, play count and recency.
///
/// Every term is a prefix query, so the result set narrows on each keystroke
/// rather than only matching once a word is complete.
pub fn search(library: &Library, query: &str, limit: usize) -> Result<Vec<TrackRow>> {
    let Some(match_expr) = to_fts_query(query) else {
        return Ok(Vec::new());
    };

    // bm25 returns a negative score where more negative is a better match, so
    // the bonuses below are subtracted to push a row further up the list.
    let sql = format!(
        "SELECT {TRACK_COLUMNS}
         {TRACK_JOINS}
         JOIN tracks_fts f ON f.rowid = t.id
         WHERE tracks_fts MATCH ?1
         ORDER BY bm25(tracks_fts, 10.0, 5.0, 3.0, 3.0)
                  - (MIN(t.play_count, 50) * 0.05)
                  - (CASE
                       WHEN t.last_played IS NULL THEN 0.0
                       WHEN t.last_played > strftime('%s', 'now') - 604800 THEN 0.75
                       WHEN t.last_played > strftime('%s', 'now') - 2592000 THEN 0.25
                       ELSE 0.0
                     END)
         LIMIT ?2"
    );

    let conn = library.connection();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![match_expr, limit as i64], TrackRow::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Turn user input into an FTS5 MATCH expression.
///
/// Each term is quoted so punctuation in a title cannot be read as FTS syntax,
/// and suffixed with `*` so partial words match. Returns None for input with no
/// usable terms, which the caller treats as "no query" rather than "no results".
fn to_fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split_whitespace()
        // A quote inside a quoted FTS string is escaped by doubling it.
        .map(|term| term.replace('"', "\"\""))
        .filter(|term| !term.trim().is_empty())
        .map(|term| format!("\"{term}\"*"))
        .collect();

    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" AND "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_prefix_queries() {
        assert_eq!(to_fts_query("bjork"), Some("\"bjork\"*".into()));
        assert_eq!(
            to_fts_query("  yellow  claw "),
            Some("\"yellow\"* AND \"claw\"*".into())
        );
        assert_eq!(to_fts_query("   "), None);
    }

    #[test]
    fn quotes_are_escaped_not_injected() {
        // Without escaping this would terminate the string and change the query.
        assert_eq!(to_fts_query("don\"t"), Some("\"don\"\"t\"*".into()));
    }
}
