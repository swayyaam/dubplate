//! Building a set that flows from one track.
//!
//! Everything this needs was produced by the analysis pass: tempo, key in
//! Camelot notation, and loudness. The rules are the ones a DJ uses -- stay
//! close in tempo, move around the Camelot wheel one step at a time, and do not
//! jump between a whisper and a wall.

use anyhow::Result;
use serde::Serialize;

use crate::db::Library;
use crate::model::{TrackRow, TRACK_COLUMNS, TRACK_JOINS};

/// How far the tempo may drift between neighbouring tracks. Beyond about 6% a
/// pitch change stops being invisible.
const BPM_TOLERANCE: f32 = 0.06;
/// And how far the set as a whole may wander from where it started.
const BPM_RANGE: f32 = 0.18;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowStep {
    pub track: TrackRow,
    /// Why this one follows the last, in the terms a DJ would use.
    pub reason: String,
}

/// A Camelot key: wheel position 1-12, and whether it is the minor (A) side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Camelot {
    pub number: u8,
    pub minor: bool,
}

pub fn parse_camelot(key: &str) -> Option<Camelot> {
    let key = key.trim();
    let (digits, letter) = key.split_at(key.len().checked_sub(1)?);
    let number: u8 = digits.parse().ok()?;
    if !(1..=12).contains(&number) {
        return None;
    }
    match letter {
        "A" | "a" => Some(Camelot { number, minor: true }),
        "B" | "b" => Some(Camelot { number, minor: false }),
        _ => None,
    }
}

/// Steps around the wheel between two keys.
///
/// Zero is the same key. One covers the three classic moves: the relative
/// major or minor, and one step either way round the wheel. Anything above two
/// is a key change people will hear.
pub fn harmonic_distance(from: Camelot, to: Camelot) -> u8 {
    let around = {
        let raw = (from.number as i16 - to.number as i16).abs();
        raw.min(12 - raw) as u8
    };
    match (around, from.minor == to.minor) {
        (0, true) => 0,
        // Relative major/minor: same number, other letter.
        (0, false) => 1,
        (steps, true) => steps,
        // Crossing the wheel and switching mode at once is a bigger move.
        (steps, false) => steps + 1,
    }
}

/// Build a set starting from `seed_id`.
///
/// Greedy rather than optimal: at each step it takes the best next track it can
/// reach, which is also how a set actually gets built. Tracks the analysis pass
/// has not reached yet are skipped, because there is nothing to match on.
pub fn build_set(library: &Library, seed_id: i64, length: usize) -> Result<Vec<FlowStep>> {
    let seed = match load(library, seed_id)? {
        Some(track) => track,
        None => return Ok(Vec::new()),
    };
    let Some(seed_bpm) = seed.bpm.filter(|bpm| *bpm > 0.0) else {
        // Without a tempo there is nothing to flow from.
        return Ok(vec![FlowStep {
            track: seed,
            reason: "Starting point".into(),
        }]);
    };
    let seed_key = seed.music_key.as_deref().and_then(parse_camelot);

    let candidates = pool(library, seed_bpm, seed_id)?;
    let mut used: Vec<i64> = vec![seed_id];
    // A library assembled from many sources holds the same music at several
    // paths, and playing it twice in one set is worse than any key clash.
    let mut heard: Vec<String> = vec![identity(&seed)];
    let mut steps = vec![FlowStep {
        track: seed.clone(),
        reason: "Starting point".into(),
    }];

    let mut current_bpm = seed_bpm;
    let mut current_key = seed_key;
    let mut current_gain = seed.replay_gain_db;
    // How long the set has sat on one key. A set that never moves is a loop.
    let mut same_key_run = 0u32;

    while steps.len() < length {
        let mut best: Option<(f32, &TrackRow, String)> = None;

        for candidate in &candidates {
            if used.contains(&candidate.id) || heard.contains(&identity(candidate)) {
                continue;
            }
            let Some(bpm) = candidate.bpm.filter(|b| *b > 0.0) else {
                continue;
            };

            let drift = (bpm - current_bpm).abs() / current_bpm;
            if drift > BPM_TOLERANCE {
                continue;
            }
            // Stay in the neighbourhood of where the set started, so a long set
            // does not creep from house to drum and bass one step at a time.
            if (bpm - seed_bpm).abs() / seed_bpm > BPM_RANGE {
                continue;
            }

            let key = candidate.music_key.as_deref().and_then(parse_camelot);
            let harmonic = match (current_key, key) {
                (Some(from), Some(to)) => harmonic_distance(from, to),
                // An unknown key is not a clash, but it is not a match either.
                _ => 2,
            };
            if harmonic > 2 {
                continue;
            }

            let loudness_gap = match (current_gain, candidate.replay_gain_db) {
                (Some(a), Some(b)) => (a - b).abs(),
                _ => 0.0,
            };

            // Lower is better: tempo first, then key, then energy.
            let mut score = drift * 40.0 + harmonic as f32 * 1.5 + loudness_gap * 0.15;
            // Sets build. A track a shade faster than the last is the natural
            // direction, so going backwards costs a little.
            if bpm < current_bpm {
                score += 0.4;
            }
            // After a few tracks in one key, staying put costs more than
            // stepping round the wheel. Otherwise the "best" next track is
            // always the most similar one and the set never goes anywhere.
            if same_key_run >= 3 && harmonic == 0 {
                score += 2.5;
            }
            if best.as_ref().map(|(current, _, _)| score < *current).unwrap_or(true) {
                best = Some((score, candidate, describe(drift, bpm, current_bpm, harmonic, key)));
            }
        }

        let Some((_, track, reason)) = best else {
            break;
        };
        used.push(track.id);
        heard.push(identity(track));
        let next_key = track.music_key.as_deref().and_then(parse_camelot);
        same_key_run = match (current_key, next_key) {
            (Some(from), Some(to)) if from == to => same_key_run + 1,
            _ => 0,
        };
        current_bpm = track.bpm.unwrap_or(current_bpm);
        current_key = next_key.or(current_key);
        current_gain = track.replay_gain_db.or(current_gain);
        steps.push(FlowStep {
            track: track.clone(),
            reason,
        });
    }

    Ok(steps)
}

fn describe(
    drift: f32,
    bpm: f32,
    from_bpm: f32,
    harmonic: u8,
    key: Option<Camelot>,
) -> String {
    let tempo = if drift < 0.005 {
        format!("{bpm:.0} BPM, same tempo")
    } else if bpm > from_bpm {
        format!("{bpm:.0} BPM, up {:.1}%", drift * 100.0)
    } else {
        format!("{bpm:.0} BPM, down {:.1}%", drift * 100.0)
    };
    let harmony = match (harmonic, key) {
        (0, Some(k)) => format!("same key ({}{})", k.number, if k.minor { 'A' } else { 'B' }),
        (1, Some(k)) => format!("one step to {}{}", k.number, if k.minor { 'A' } else { 'B' }),
        (_, Some(k)) => format!("{}{}", k.number, if k.minor { 'A' } else { 'B' }),
        (_, None) => "key unknown".into(),
    };
    format!("{tempo} · {harmony}")
}

/// What counts as "the same music", for keeping a set from repeating itself.
/// Title and artist rather than path: the duplicates in a real library differ
/// only in where they live.
fn identity(track: &TrackRow) -> String {
    let title = track
        .title
        .clone()
        .unwrap_or_else(|| track.file_name.clone())
        .to_lowercase();
    let artist = track.artist.clone().unwrap_or_default().to_lowercase();
    format!("{}\u{1}{}", title.trim(), artist.trim())
}

fn load(library: &Library, id: i64) -> Result<Option<TrackRow>> {
    let sql = format!("SELECT {TRACK_COLUMNS} {TRACK_JOINS} WHERE t.id = ?1");
    let conn = library.connection();
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map([id], TrackRow::from_row)?;
    Ok(rows.next().transpose()?)
}

/// Everything within reach of the seed's tempo, so the walk works in memory
/// rather than querying per step.
fn pool(library: &Library, seed_bpm: f32, seed_id: i64) -> Result<Vec<TrackRow>> {
    let sql = format!(
        "SELECT {TRACK_COLUMNS} {TRACK_JOINS}
         WHERE t.id != ?1
           AND t.bpm IS NOT NULL
           AND t.bpm BETWEEN ?2 AND ?3
         ORDER BY t.play_count DESC"
    );
    let conn = library.connection();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params![
            seed_id,
            seed_bpm * (1.0 - BPM_RANGE),
            seed_bpm * (1.0 + BPM_RANGE)
        ],
        TrackRow::from_row,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> Camelot {
        parse_camelot(text).unwrap()
    }

    #[test]
    fn camelot_parsing_accepts_the_wheel_and_rejects_nonsense() {
        assert_eq!(key("8A"), Camelot { number: 8, minor: true });
        assert_eq!(key("12B"), Camelot { number: 12, minor: false });
        assert!(parse_camelot("13A").is_none());
        assert!(parse_camelot("0B").is_none());
        assert!(parse_camelot("8C").is_none());
        assert!(parse_camelot("").is_none());
    }

    #[test]
    fn the_three_classic_moves_are_all_one_step() {
        let from = key("8A");
        // Relative major.
        assert_eq!(harmonic_distance(from, key("8B")), 1);
        // One step either way round the wheel.
        assert_eq!(harmonic_distance(from, key("7A")), 1);
        assert_eq!(harmonic_distance(from, key("9A")), 1);
        // And itself.
        assert_eq!(harmonic_distance(from, key("8A")), 0);
    }

    #[test]
    fn the_wheel_wraps_round() {
        assert_eq!(harmonic_distance(key("12A"), key("1A")), 1);
        assert_eq!(harmonic_distance(key("1A"), key("12A")), 1);
    }

    #[test]
    fn a_distant_key_is_a_distant_key() {
        // Six steps away is the far side of the wheel.
        assert_eq!(harmonic_distance(key("8A"), key("2A")), 6);
        // Crossing the wheel and switching mode at once costs more.
        assert_eq!(harmonic_distance(key("8A"), key("2B")), 7);
    }
}
