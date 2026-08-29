use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Current schema version. Bump and add a migration arm when the schema moves.
const SCHEMA_VERSION: i32 = 2;

/// The library index.
///
/// The database is a cache, never the source of truth. Deleting it and
/// rescanning must lose nothing except play counts and history, so nothing here
/// stores anything that cannot be rebuilt from the filesystem.
pub struct Library {
    pub(crate) conn: Connection,
}

impl Library {
    /// Open (or create) the index at `path`. Pass `":memory:"` for tests.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening index at {}", path.display()))?;
        Self::from_connection(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        // WAL lets the UI read while a scan writes. NORMAL is the right
        // durability trade for a cache that can be rebuilt from disk.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;

        let mut library = Self { conn };
        library.migrate()?;
        Ok(library)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i32 =
            self.conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version >= SCHEMA_VERSION {
            return Ok(());
        }
        if version == 0 {
            // A fresh database gets the current schema in one go.
            self.conn.execute_batch(SCHEMA)?;
        } else {
            // An existing one is stepped forward. Each step is additive and
            // safe to apply to a library that already has data in it.
            if version < 2 {
                self.conn.execute_batch(UNDO_SCHEMA)?;
            }
        }
        self.conn
            .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    /// Read a value from the `app_state` table: queue, last view, library root.
    pub fn get_state(&self, key: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row("SELECT value FROM app_state WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn set_state(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO app_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    /// Escape hatch for callers that need raw SQL (the sync pass, mostly).
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

const SCHEMA: &str = r#"
CREATE TABLE artists (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL,
  sort_name   TEXT,
  UNIQUE(name)
);

CREATE TABLE albums (
  id              INTEGER PRIMARY KEY,
  title           TEXT NOT NULL,
  album_artist_id INTEGER REFERENCES artists(id),
  year            INTEGER,
  art_hash        TEXT,          -- key into the artwork cache
  disc_count      INTEGER,
  UNIQUE(title, album_artist_id)
);

CREATE TABLE tracks (
  id            INTEGER PRIMARY KEY,
  path          TEXT NOT NULL UNIQUE,
  content_key   TEXT NOT NULL,   -- size + partial hash, for move detection
  mtime         INTEGER NOT NULL,
  size          INTEGER NOT NULL,

  title         TEXT,
  artist_id     INTEGER REFERENCES artists(id),
  album_id      INTEGER REFERENCES albums(id),
  track_no      INTEGER,
  disc_no       INTEGER,
  year          INTEGER,
  genre         TEXT,

  duration_ms   INTEGER,
  codec         TEXT,
  -- Nullable, unlike the original sketch: MP4 holds either AAC or ALAC and a
  -- tag-level read cannot say which. NULL means "not yet known" rather than
  -- claiming lossless, which is the badge this player must never get wrong.
  is_lossy      INTEGER,
  sample_rate   INTEGER,
  bit_depth     INTEGER,         -- null for lossy codecs, they have none
  sample_format TEXT,
  channels      INTEGER,
  bitrate       INTEGER,
  bitrate_mode  TEXT,

  rg_track_gain REAL,
  rg_track_peak REAL,
  bpm           REAL,
  music_key     TEXT,

  effective_bits   INTEGER,
  spectral_cutoff  INTEGER,
  transcode_score  REAL,
  analyzed_at      INTEGER,

  added_at      INTEGER NOT NULL,
  last_played   INTEGER,
  play_count    INTEGER NOT NULL DEFAULT 0,
  skip_count    INTEGER NOT NULL DEFAULT 0,
  loved         INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_tracks_album  ON tracks(album_id, disc_no, track_no);
CREATE INDEX idx_tracks_artist ON tracks(artist_id);
CREATE INDEX idx_tracks_added  ON tracks(added_at DESC);
CREATE INDEX idx_tracks_key    ON tracks(content_key);

CREATE VIRTUAL TABLE tracks_fts USING fts5(
  title, artist, album, album_artist,
  content='',
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TABLE playlists (
  id         INTEGER PRIMARY KEY,
  name       TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  is_smart   INTEGER NOT NULL DEFAULT 0,
  rules_json TEXT
);

CREATE TABLE playlist_tracks (
  playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
  track_id    INTEGER NOT NULL REFERENCES tracks(id)    ON DELETE CASCADE,
  position    REAL NOT NULL,   -- fractional, so reordering is one UPDATE
  PRIMARY KEY (playlist_id, track_id)
);

CREATE TABLE play_history (
  id         INTEGER PRIMARY KEY,
  track_id   INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  played_at  INTEGER NOT NULL,
  ms_played  INTEGER NOT NULL,
  completed  INTEGER NOT NULL   -- crossed the 50% mark
);

CREATE TABLE app_state (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL          -- queue, position, volume, last view
);

CREATE TABLE tag_undo_batch (
  id          INTEGER PRIMARY KEY,
  description TEXT NOT NULL,
  tracks      INTEGER NOT NULL,
  created_at  INTEGER NOT NULL
);

CREATE TABLE tag_undo (
  id       INTEGER PRIMARY KEY,
  batch    INTEGER NOT NULL REFERENCES tag_undo_batch(id) ON DELETE CASCADE,
  track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  path     TEXT NOT NULL,
  fields   TEXT NOT NULL,
  art_blob TEXT,
  had_art  INTEGER NOT NULL
);

CREATE INDEX tag_undo_batch_idx ON tag_undo(batch);
"#;

/// Added in version 2, with the tag editor.
///
/// Kept as its own statement so a library from version 1 gains the tables
/// without being rebuilt: an index is expensive to recreate and holds play
/// counts and history that exist nowhere else.
const UNDO_SCHEMA: &str = r#"
CREATE TABLE tag_undo_batch (
  id          INTEGER PRIMARY KEY,
  description TEXT NOT NULL,   -- "Edit tags", "Tags from filenames"
  tracks      INTEGER NOT NULL,
  created_at  INTEGER NOT NULL
);

CREATE TABLE tag_undo (
  id       INTEGER PRIMARY KEY,
  batch    INTEGER NOT NULL REFERENCES tag_undo_batch(id) ON DELETE CASCADE,
  track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  path     TEXT NOT NULL,
  -- The values these fields had before the write. A null value means the
  -- field was absent, which undo restores by clearing it again.
  fields   TEXT NOT NULL,
  -- Hash of the cover the file had before, in the undo blob store. Null when
  -- the write did not touch artwork.
  art_blob TEXT,
  had_art  INTEGER NOT NULL
);

CREATE INDEX tag_undo_batch_idx ON tag_undo(batch);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_and_migrates() {
        let library = Library::open_in_memory().unwrap();
        let version: i32 = library
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn fts5_is_available_and_folds_diacritics() {
        let library = Library::open_in_memory().unwrap();
        library
            .conn
            .execute(
                "INSERT INTO tracks_fts(rowid, title, artist, album, album_artist)
                 VALUES (1, 'Björk', 'Björk', 'Post', 'Björk')",
                [],
            )
            .unwrap();

        // remove_diacritics=2 means an ASCII query finds the accented row.
        let hits: i64 = library
            .conn
            .query_row(
                "SELECT count(*) FROM tracks_fts WHERE tracks_fts MATCH 'bjork'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "FTS5 with diacritic folding must be compiled in");
    }

    #[test]
    fn migration_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.sqlite");
        Library::open(&path).unwrap();
        // Opening again must not try to recreate the schema.
        Library::open(&path).unwrap();
    }
}
