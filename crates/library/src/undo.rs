//! Putting tag writes back the way they were.
//!
//! Every write records what the fields held beforehand, so reversing it is a
//! second write with the old values rather than anything clever. That is the
//! whole design: undo is not a special mode, it is the same code path pointed
//! backwards, which means it cannot drift from what writing actually does.
//!
//! Previous cover images are the one thing too big for a column, so they go in
//! a small content-addressed store beside the index. Content-addressed because
//! an album's twelve tracks share one cover, and storing it twelve times to
//! undo one operation would be silly.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::Library;
use crate::tags::Field;

/// How many operations are kept. Older ones are pruned, with their blobs.
///
/// A bounded history rather than an unbounded one: this exists so a bulk edit
/// that went wrong can be taken back, not so the library can be replayed from
/// the beginning of time.
pub const HISTORY: usize = 25;

/// Where previous cover images live.
pub struct UndoStore {
    root: PathBuf,
}

/// One field's value before a write. `None` means the field was absent, which
/// undo restores by clearing it rather than by writing an empty string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviousField {
    pub field: Field,
    pub value: Option<String>,
}

/// What one file looked like before a write.
#[derive(Debug, Clone)]
pub struct Previous {
    pub track_id: i64,
    pub path: String,
    pub fields: Vec<PreviousField>,
    /// The cover the file had, if the write was going to change it.
    pub artwork: Option<Option<Vec<u8>>>,
}

/// An operation that can be undone.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Batch {
    pub id: i64,
    pub description: String,
    pub tracks: usize,
    pub created_at: i64,
}

impl UndoStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path_for(&self, hash: &str) -> PathBuf {
        self.root.join(format!("{hash}.art"))
    }

    /// Keep an image, returning the hash it is stored under. Storing the same
    /// bytes twice costs nothing the second time.
    fn put(&self, bytes: &[u8]) -> Result<String> {
        let hash = blake3::hash(bytes).to_hex()[..32].to_owned();
        let path = self.path_for(&hash);
        if path.exists() {
            return Ok(hash);
        }
        std::fs::create_dir_all(&self.root)?;
        let temp = path.with_extension("art.tmp");
        std::fs::write(&temp, bytes)?;
        std::fs::rename(&temp, &path)?;
        Ok(hash)
    }

    fn get(&self, hash: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path_for(hash)).ok()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Record what a set of files looked like before they were written.
///
/// Called with the state captured *before* the write, and only for files the
/// write actually succeeded on -- recording a failed write would offer to undo
/// something that never happened.
pub fn record(
    library: &mut Library,
    store: &UndoStore,
    description: &str,
    previous: &[Previous],
) -> Result<Option<i64>> {
    if previous.is_empty() {
        return Ok(None);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Blobs are written outside the transaction: a file write must not hold a
    // database lock, and an orphaned blob is harmless where a missing one is
    // not.
    let mut blobs = Vec::with_capacity(previous.len());
    for entry in previous {
        let hash = match &entry.artwork {
            Some(Some(bytes)) => Some(store.put(bytes).context("storing the previous cover")?),
            _ => None,
        };
        blobs.push(hash);
    }

    let tx = library.connection_mut().transaction()?;
    tx.execute(
        "INSERT INTO tag_undo_batch (description, tracks, created_at) VALUES (?1, ?2, ?3)",
        params![description, previous.len() as i64, now],
    )?;
    let batch = tx.last_insert_rowid();

    for (entry, blob) in previous.iter().zip(blobs) {
        let fields = serde_json::to_string(&entry.fields)?;
        tx.execute(
            "INSERT INTO tag_undo (batch, track_id, path, fields, art_blob, had_art)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                batch,
                entry.track_id,
                entry.path,
                fields,
                blob,
                // Distinguishes "had a cover, here it is" from "had none, so
                // undo should remove whatever is there now".
                match &entry.artwork {
                    Some(Some(_)) => 1,
                    Some(None) => 0,
                    None => -1,
                }
            ],
        )?;
    }
    tx.commit()?;

    prune(library, store)?;
    Ok(Some(batch))
}

/// The operations that can still be undone, newest first.
pub fn history(library: &Library) -> Result<Vec<Batch>> {
    let conn = library.connection();
    let mut stmt = conn.prepare(
        "SELECT id, description, tracks, created_at FROM tag_undo_batch ORDER BY id DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Batch {
            id: row.get(0)?,
            description: row.get(1)?,
            tracks: row.get::<_, i64>(2)? as usize,
            created_at: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// One file's place in a recorded batch.
#[derive(Debug, Clone)]
pub struct Entry {
    pub track_id: i64,
    pub path: String,
    pub fields: Vec<PreviousField>,
    /// Hash of the cover in the blob store, if one was kept.
    pub art_blob: Option<String>,
    /// 1 there was a cover, 0 there was none, -1 the write never touched
    /// artwork and undo must not either.
    pub had_art: i64,
}

/// What one batch would restore, without restoring it.
pub fn entries(library: &Library, batch: i64) -> Result<Vec<Entry>> {
    let conn = library.connection();
    let mut stmt = conn.prepare(
        "SELECT track_id, path, fields, art_blob, had_art FROM tag_undo WHERE batch = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([batch], |row| {
        let fields: String = row.get(2)?;
        Ok(Entry {
            track_id: row.get(0)?,
            path: row.get(1)?,
            fields: serde_json::from_str(&fields).unwrap_or_default(),
            art_blob: row.get(3)?,
            had_art: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The cover stored for one entry, if there was one.
pub fn artwork_for(store: &UndoStore, hash: &str) -> Option<Vec<u8>> {
    store.get(hash)
}

/// Forget a batch once it has been applied.
pub fn forget(library: &mut Library, store: &UndoStore, batch: i64) -> Result<()> {
    library
        .connection()
        .execute("DELETE FROM tag_undo_batch WHERE id = ?1", params![batch])?;
    library
        .connection()
        .execute("DELETE FROM tag_undo WHERE batch = ?1", params![batch])?;
    prune(library, store)
}

/// The id of the newest batch, which is the only one that can be undone.
///
/// Only the newest, because undoing out of order is not undo: reversing an
/// older edit while a newer one still stands would produce a state the library
/// was never in.
pub fn newest(library: &Library) -> Result<Option<i64>> {
    Ok(library
        .connection()
        .query_row("SELECT MAX(id) FROM tag_undo_batch", [], |row| row.get(0))
        .optional()?
        .flatten())
}

/// Drop batches beyond the history limit, and any blob nothing refers to.
fn prune(library: &mut Library, store: &UndoStore) -> Result<()> {
    {
        let conn = library.connection();
        conn.execute(
            "DELETE FROM tag_undo WHERE batch IN (
               SELECT id FROM tag_undo_batch
               ORDER BY id DESC LIMIT -1 OFFSET ?1)",
            params![HISTORY as i64],
        )?;
        conn.execute(
            "DELETE FROM tag_undo_batch WHERE id IN (
               SELECT id FROM tag_undo_batch ORDER BY id DESC LIMIT -1 OFFSET ?1)",
            params![HISTORY as i64],
        )?;
    }

    // Sweep blobs no row mentions any more. Cheap: the store only ever holds
    // covers from the last few operations.
    let Ok(entries) = std::fs::read_dir(store.root()) else {
        return Ok(());
    };
    let live: std::collections::HashSet<String> = {
        let conn = library.connection();
        let mut stmt = conn.prepare("SELECT DISTINCT art_blob FROM tag_undo WHERE art_blob IS NOT NULL")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.filter_map(Result::ok).collect()
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(hash) = name.strip_suffix(".art") else {
            continue;
        };
        if !live.contains(hash) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}
