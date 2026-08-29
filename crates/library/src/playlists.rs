//! Playlists, smart and otherwise.
//!
//! A smart playlist stores its rules rather than its members, so it answers
//! from the library as it is now. A plain one stores its members with a
//! fractional position, so reordering is a single write.

use anyhow::Result;
use rusqlite::params;
use serde::Serialize;

use crate::db::Library;
use crate::model::{TrackRow, TRACK_COLUMNS, TRACK_JOINS};
use crate::smart::{self, Condition, Field, Op, SmartRules, Sort, Value};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistRow {
    pub id: i64,
    pub name: String,
    pub is_smart: bool,
    pub created_at: i64,
    /// Resolved for smart playlists, counted for plain ones.
    pub track_count: i64,
}

pub fn list(library: &Library) -> Result<Vec<PlaylistRow>> {
    let conn = library.connection();
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name, p.is_smart, p.created_at, p.rules_json,
                (SELECT count(*) FROM playlist_tracks pt WHERE pt.playlist_id = p.id)
         FROM playlists p
         ORDER BY p.name COLLATE NOCASE",
    )?;
    let rows: Vec<(i64, String, bool, i64, Option<String>, i64)> = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get::<_, i64>(2)? != 0,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let mut out = Vec::with_capacity(rows.len());
    for (id, name, is_smart, created_at, rules_json, member_count) in rows {
        // A smart playlist has no members to count: it has to be resolved
        // against the library as it stands.
        let track_count = if is_smart {
            rules_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<SmartRules>(json).ok())
                .and_then(|rules| smart::resolve(library, &rules).ok())
                .map(|tracks| tracks.len() as i64)
                .unwrap_or(0)
        } else {
            member_count
        };
        out.push(PlaylistRow {
            id,
            name,
            is_smart,
            created_at,
            track_count,
        });
    }
    Ok(out)
}

pub fn create_smart(library: &Library, name: &str, rules: &SmartRules) -> Result<i64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let json = serde_json::to_string(rules)?;
    library.connection().execute(
        "INSERT INTO playlists (name, created_at, is_smart, rules_json)
         VALUES (?1, ?2, 1, ?3)",
        params![name, now, json],
    )?;
    Ok(library.connection().last_insert_rowid())
}

pub fn delete(library: &Library, id: i64) -> Result<()> {
    library
        .connection()
        .execute("DELETE FROM playlists WHERE id = ?1", params![id])?;
    Ok(())
}

/// What is in a playlist right now.
pub fn tracks(library: &Library, id: i64) -> Result<Vec<TrackRow>> {
    let (is_smart, rules_json): (bool, Option<String>) = library.connection().query_row(
        "SELECT is_smart, rules_json FROM playlists WHERE id = ?1",
        params![id],
        |row| Ok((row.get::<_, i64>(0)? != 0, row.get(1)?)),
    )?;

    if is_smart {
        let rules: SmartRules = rules_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default();
        return smart::resolve(library, &rules);
    }

    let sql = format!(
        "SELECT {TRACK_COLUMNS} {TRACK_JOINS}
         JOIN playlist_tracks pt ON pt.track_id = t.id
         WHERE pt.playlist_id = ?1
         ORDER BY pt.position"
    );
    let conn = library.connection();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![id], TrackRow::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn number(field: Field, op: Op, value: f64) -> Condition {
    Condition {
        field,
        op,
        value: Some(Value::Number(value)),
    }
}

fn flag(field: Field, op: Op) -> Condition {
    Condition {
        field,
        op,
        value: None,
    }
}

/// Ready-made smart playlists, each answering a question worth asking of a
/// collection assembled over years.
pub fn presets() -> Vec<(&'static str, SmartRules)> {
    vec![
        (
            "Hi-res",
            SmartRules {
                any: vec![
                    number(Field::BitDepth, Op::Gte, 24.0),
                    number(Field::SampleRate, Op::Gt, 48_000.0),
                ],
                sort: Sort::Artist,
                ..Default::default()
            },
        ),
        (
            "Suspected transcodes",
            SmartRules {
                all: vec![
                    number(Field::TranscodeScore, Op::Gte, 0.5),
                    number(Field::IsLossy, Op::Eq, 0.0),
                ],
                sort: Sort::TranscodeScore,
                ..Default::default()
            },
        ),
        (
            "Padded containers",
            SmartRules {
                all: vec![flag(Field::Padded, Op::IsTrue)],
                sort: Sort::Artist,
                ..Default::default()
            },
        ),
        (
            "Never played",
            SmartRules {
                all: vec![number(Field::PlayCount, Op::Eq, 0.0)],
                sort: Sort::AddedNewest,
                ..Default::default()
            },
        ),
        (
            "Most played",
            SmartRules {
                all: vec![number(Field::PlayCount, Op::Gt, 0.0)],
                sort: Sort::MostPlayed,
                limit: 100,
                ..Default::default()
            },
        ),
        (
            "Loved",
            SmartRules {
                all: vec![flag(Field::Loved, Op::IsTrue)],
                sort: Sort::AddedNewest,
                ..Default::default()
            },
        ),
        (
            "Recently added",
            SmartRules {
                sort: Sort::AddedNewest,
                limit: 100,
                ..Default::default()
            },
        ),
    ]
}
