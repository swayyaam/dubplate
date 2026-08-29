use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rayon::prelude::*;
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;

use crate::db::Library;
use crate::scan::{self, FileEntry};
use crate::track::{Lossiness, ScanError, ScannedTrack};

/// What one incremental sync did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub root: String,
    /// Audio files present on disk.
    pub files_seen: usize,
    pub added: usize,
    pub updated: usize,
    /// Recognised at a new path by content, so play counts survived.
    pub moved: usize,
    pub removed: usize,
    /// Skipped because (mtime, size) still matched the index.
    pub unchanged: usize,
    pub errors: Vec<ScanError>,
    pub elapsed_ms: u64,
}

struct Indexed {
    id: i64,
    mtime: i64,
    size: u64,
    content_key: String,
}

/// Bring the index in line with what is on disk.
///
/// Only files whose (mtime, size) no longer match the index are opened, so a
/// rescan of an unchanged library costs one directory walk and nothing else.
/// Files that vanished from one path and appeared at another are matched by
/// content and updated in place, which is what keeps play counts attached to
/// music through a reorganised folder tree.
pub fn sync(library: &mut Library, root: &Path) -> Result<SyncReport> {
    let started = Instant::now();
    let entries = scan::walk(root);
    let files_seen = entries.len();

    let indexed = load_index(library)?;

    // Split what is on disk into "already correct" and "needs reading".
    let mut unchanged = 0usize;
    let mut to_read: Vec<FileEntry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::with_capacity(entries.len());
    for entry in entries {
        let path = entry.path.to_string_lossy().into_owned();
        match indexed.get(&path) {
            Some(row) if row.mtime == entry.mtime && row.size == entry.size => unchanged += 1,
            _ => to_read.push(entry),
        }
        seen.insert(path);
    }

    // Rows whose file is no longer where the index left it. Each is either a
    // move waiting to be matched, or a deletion.
    let mut missing_by_key: HashMap<String, Vec<i64>> = HashMap::new();
    let mut missing_ids: HashSet<i64> = HashSet::new();
    for (path, row) in &indexed {
        if !seen.contains(path) {
            missing_by_key
                .entry(row.content_key.clone())
                .or_default()
                .push(row.id);
            missing_ids.insert(row.id);
        }
    }

    // The expensive half, across all cores: tags plus a content key.
    let read: Vec<Result<(ScannedTrack, String), ScanError>> = to_read
        .par_iter()
        .map(|entry| {
            let track = scan::read_track(&entry.path, entry.size, entry.mtime)?;
            let key = scan::content_key(&entry.path, entry.size).map_err(|err| ScanError {
                path: entry.path.to_string_lossy().into_owned(),
                message: err.to_string(),
            })?;
            Ok((track, key))
        })
        .collect();

    let mut errors = Vec::new();
    let mut incoming = Vec::with_capacity(read.len());
    for result in read {
        match result {
            Ok(pair) => incoming.push(pair),
            Err(err) => errors.push(err),
        }
    }

    let mut report = SyncReport {
        root: root.to_string_lossy().into_owned(),
        files_seen,
        unchanged,
        errors,
        ..Default::default()
    };

    let now = unix_now();
    let tx = library.connection_mut().transaction()?;
    {
        let mut artists: HashMap<String, i64> = HashMap::new();
        let mut albums: HashMap<(String, Option<i64>), i64> = HashMap::new();
        let mut claimed: HashSet<i64> = HashSet::new();

        for (track, content_key) in &incoming {
            let artist_id = match clean(track.artist.as_deref()) {
                Some(name) => Some(intern_artist(&tx, &mut artists, &name)?),
                None => None,
            };
            // Albums group by album artist, falling back to the track artist so
            // a compilation without an ALBUMARTIST tag still collapses.
            let album_artist_id = match clean(track.album_artist.as_deref()) {
                Some(name) => Some(intern_artist(&tx, &mut artists, &name)?),
                None => artist_id,
            };
            let album_id = match clean(track.album.as_deref()) {
                Some(title) => Some(intern_album(
                    &tx,
                    &mut albums,
                    &title,
                    album_artist_id,
                    track.year,
                )?),
                None => None,
            };
            let fields = TrackFields {
                track,
                content_key,
                artist_id,
                album_id,
            };

            match indexed.get(&track.path) {
                // Same path, different bytes. The file was re-encoded or
                // re-tagged, so any stored analysis no longer describes it.
                Some(row) => {
                    let contents_changed = row.content_key != *content_key;
                    update_track(&tx, row.id, &fields, contents_changed)?;
                    report.updated += 1;
                }
                None => match take_move_candidate(&mut missing_by_key, content_key, &claimed) {
                    // Same bytes at a new path: a rename or a reorganised
                    // folder. Update in place so play count and history survive.
                    Some(id) => {
                        update_track(&tx, id, &fields, false)?;
                        claimed.insert(id);
                        report.moved += 1;
                    }
                    None => {
                        insert_track(&tx, &fields, now)?;
                        report.added += 1;
                    }
                },
            }
        }

        for id in missing_ids.difference(&claimed) {
            tx.execute("DELETE FROM tracks WHERE id = ?1", params![id])?;
            report.removed += 1;
        }

        let changed = report.added + report.updated + report.moved + report.removed > 0;
        if changed {
            prune_orphans(&tx)?;
            rebuild_fts(&tx)?;
        }
    }
    tx.commit()?;

    report.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

struct TrackFields<'a> {
    track: &'a ScannedTrack,
    content_key: &'a str,
    artist_id: Option<i64>,
    album_id: Option<i64>,
}

fn load_index(library: &Library) -> Result<HashMap<String, Indexed>> {
    let conn = library.connection();
    let mut stmt = conn.prepare("SELECT id, path, mtime, size, content_key FROM tracks")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(1)?,
            Indexed {
                id: row.get(0)?,
                mtime: row.get(2)?,
                size: row.get::<_, i64>(3)? as u64,
                content_key: row.get(4)?,
            },
        ))
    })?;

    let mut map = HashMap::new();
    for row in rows {
        let (path, indexed) = row?;
        map.insert(path, indexed);
    }
    Ok(map)
}

fn take_move_candidate(
    missing: &mut HashMap<String, Vec<i64>>,
    key: &str,
    claimed: &HashSet<i64>,
) -> Option<i64> {
    let ids = missing.get_mut(key)?;
    while let Some(id) = ids.pop() {
        if !claimed.contains(&id) {
            return Some(id);
        }
    }
    None
}

fn insert_track(tx: &Transaction, fields: &TrackFields, now: i64) -> Result<()> {
    let t = fields.track;
    tx.execute(
        "INSERT INTO tracks (
            path, content_key, mtime, size,
            title, artist_id, album_id, track_no, disc_no, year, genre,
            duration_ms, codec, is_lossy, sample_rate, bit_depth, channels, bitrate,
            rg_track_gain, rg_track_peak,
            added_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
        params![
            t.path,
            fields.content_key,
            t.mtime,
            t.size as i64,
            clean(t.title.as_deref()),
            fields.artist_id,
            fields.album_id,
            t.track_no,
            t.disc_no,
            t.year,
            clean(t.genre.as_deref()),
            t.duration_ms as i64,
            t.codec,
            is_lossy(t.lossiness),
            t.sample_rate,
            t.bit_depth,
            t.channels,
            t.bitrate,
            t.replay_gain_db,
            t.replay_gain_peak,
            now,
        ],
    )?;
    Ok(())
}

fn update_track(
    tx: &Transaction,
    id: i64,
    fields: &TrackFields,
    reset_analysis: bool,
) -> Result<()> {
    let t = fields.track;
    tx.execute(
        "UPDATE tracks SET
            path = ?1, content_key = ?2, mtime = ?3, size = ?4,
            title = ?5, artist_id = ?6, album_id = ?7, track_no = ?8, disc_no = ?9,
            year = ?10, genre = ?11, duration_ms = ?12, codec = ?13, is_lossy = ?14,
            sample_rate = ?15, bit_depth = ?16, channels = ?17, bitrate = ?18,
            -- Keep a value the analysis pass computed when the file carries no
            -- tag of its own, but let a real tag win.
            rg_track_gain = COALESCE(?19, rg_track_gain),
            rg_track_peak = COALESCE(?20, rg_track_peak)
         WHERE id = ?21",
        params![
            t.path,
            fields.content_key,
            t.mtime,
            t.size as i64,
            clean(t.title.as_deref()),
            fields.artist_id,
            fields.album_id,
            t.track_no,
            t.disc_no,
            t.year,
            clean(t.genre.as_deref()),
            t.duration_ms as i64,
            t.codec,
            is_lossy(t.lossiness),
            t.sample_rate,
            t.bit_depth,
            t.channels,
            t.bitrate,
            t.replay_gain_db,
            t.replay_gain_peak,
            id,
        ],
    )?;

    if reset_analysis {
        // Loudness, BPM, key and spectral figures describe bytes that are gone.
        // The stored analysis described bytes that are gone. ReplayGain comes
        // back from the file's own tags, if it has any, rather than being lost.
        tx.execute(
            "UPDATE tracks SET
                analyzed_at = NULL, bpm = NULL, music_key = NULL,
                effective_bits = NULL, spectral_cutoff = NULL, transcode_score = NULL,
                rg_track_gain = ?2, rg_track_peak = ?3
             WHERE id = ?1",
            params![id, fields.track.replay_gain_db, fields.track.replay_gain_peak],
        )?;
    }
    Ok(())
}

fn intern_artist(
    tx: &Transaction,
    cache: &mut HashMap<String, i64>,
    name: &str,
) -> Result<i64> {
    if let Some(id) = cache.get(name) {
        return Ok(*id);
    }
    tx.execute("INSERT OR IGNORE INTO artists (name) VALUES (?1)", params![name])?;
    let id: i64 = tx.query_row(
        "SELECT id FROM artists WHERE name = ?1",
        params![name],
        |row| row.get(0),
    )?;
    cache.insert(name.to_owned(), id);
    Ok(id)
}

fn intern_album(
    tx: &Transaction,
    cache: &mut HashMap<(String, Option<i64>), i64>,
    title: &str,
    album_artist_id: Option<i64>,
    year: Option<u32>,
) -> Result<i64> {
    let cache_key = (title.to_owned(), album_artist_id);
    if let Some(id) = cache.get(&cache_key) {
        return Ok(*id);
    }
    // UNIQUE(title, album_artist_id) does not dedupe rows where the artist is
    // NULL, because SQLite treats NULLs as distinct. `IS` compares them.
    let found: Option<i64> = tx
        .query_row(
            "SELECT id FROM albums WHERE title = ?1 AND album_artist_id IS ?2",
            params![title, album_artist_id],
            |row| row.get(0),
        )
        .optional()?;

    let id = match found {
        Some(id) => id,
        None => {
            tx.execute(
                "INSERT INTO albums (title, album_artist_id, year) VALUES (?1, ?2, ?3)",
                params![title, album_artist_id, year],
            )?;
            tx.last_insert_rowid()
        }
    };
    cache.insert(cache_key, id);
    Ok(id)
}

/// Drop artists and albums no track points at any more. Albums first, since
/// they hold the last references to some album artists.
fn prune_orphans(tx: &Transaction) -> Result<()> {
    tx.execute(
        "DELETE FROM albums
         WHERE id NOT IN (SELECT album_id FROM tracks WHERE album_id IS NOT NULL)",
        [],
    )?;
    tx.execute(
        "DELETE FROM artists
         WHERE id NOT IN (SELECT artist_id FROM tracks WHERE artist_id IS NOT NULL)
           AND id NOT IN (SELECT album_artist_id FROM albums WHERE album_artist_id IS NOT NULL)",
        [],
    )?;
    Ok(())
}

/// The FTS table is contentless, so it stores no copy of the text and cannot be
/// updated row by row. Rebuilding it wholesale is both simpler and, at library
/// scale, faster than tracking which rows moved.
fn rebuild_fts(tx: &Transaction) -> Result<()> {
    tx.execute("INSERT INTO tracks_fts(tracks_fts) VALUES('delete-all')", [])?;

    let mut select = tx.prepare(
        "SELECT t.id, t.title, t.path, ar.name, al.title, aa.name
         FROM tracks t
         LEFT JOIN artists ar ON ar.id = t.artist_id
         LEFT JOIN albums  al ON al.id = t.album_id
         LEFT JOIN artists aa ON aa.id = al.album_artist_id",
    )?;
    let mut insert = tx.prepare(
        "INSERT INTO tracks_fts(rowid, title, artist, album, album_artist)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;

    let rows = select.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;

    for row in rows {
        let (id, title, path, artist, album, album_artist) = row?;
        // An untagged file has no title, and half a collection assembled from
        // many sources is untagged. Indexing the filename keeps it findable
        // instead of invisible.
        let title = title.unwrap_or_else(|| file_stem(&path));
        insert.execute(params![
            id,
            title,
            artist.unwrap_or_default(),
            album.unwrap_or_default(),
            album_artist.unwrap_or_default()
        ])?;
    }
    Ok(())
}

fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn is_lossy(lossiness: Lossiness) -> Option<i64> {
    match lossiness {
        Lossiness::Lossless => Some(0),
        Lossiness::Lossy => Some(1),
        // Not "lossless by default". See the schema comment on this column.
        Lossiness::Unknown => None,
    }
}

/// Blank and whitespace-only tags are absent tags.
fn clean(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
