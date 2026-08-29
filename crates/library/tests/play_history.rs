mod common;

use common::write_wav_seeded;
use dubplate_library::{history, index, history::Listen, Library};

fn one_track_library(dir: &std::path::Path) -> (Library, i64) {
    write_wav_seeded(&dir.join("a.wav"), 44_100, 16, 2, 44_100, 1);
    let mut library = Library::open_in_memory().unwrap();
    index::sync(&mut library, dir).unwrap();
    let id = library
        .connection()
        .query_row("SELECT id FROM tracks", [], |row| row.get(0))
        .unwrap();
    (library, id)
}

fn counts(library: &Library, id: i64) -> (i64, i64, Option<i64>) {
    library
        .connection()
        .query_row(
            "SELECT play_count, skip_count, last_played FROM tracks WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

#[test]
fn a_finished_listen_counts_as_a_play() {
    let dir = tempfile::tempdir().unwrap();
    let (mut library, id) = one_track_library(dir.path());

    history::record(
        &mut library,
        &[Listen { track_id: id, ms_played: 900, completed: true }],
        1_700_000_000,
    )
    .unwrap();

    let (plays, skips, last) = counts(&library, id);
    assert_eq!(plays, 1);
    assert_eq!(skips, 0);
    assert_eq!(last, Some(1_700_000_000));
}

#[test]
fn an_abandoned_listen_counts_as_a_skip_and_does_not_touch_last_played() {
    let dir = tempfile::tempdir().unwrap();
    let (mut library, id) = one_track_library(dir.path());

    history::record(
        &mut library,
        &[Listen { track_id: id, ms_played: 120, completed: false }],
        1_700_000_000,
    )
    .unwrap();

    let (plays, skips, last) = counts(&library, id);
    assert_eq!(plays, 0);
    assert_eq!(skips, 1);
    assert_eq!(last, None, "skipping is not listening");
}

#[test]
fn every_listen_lands_in_the_history_either_way() {
    let dir = tempfile::tempdir().unwrap();
    let (mut library, id) = one_track_library(dir.path());

    history::record(
        &mut library,
        &[
            Listen { track_id: id, ms_played: 900, completed: true },
            Listen { track_id: id, ms_played: 40, completed: false },
        ],
        1_700_000_000,
    )
    .unwrap();

    let rows: i64 = library
        .connection()
        .query_row("SELECT count(*) FROM play_history", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 2);

    let (plays, skips, _) = counts(&library, id);
    assert_eq!((plays, skips), (1, 1));
}

#[test]
fn a_track_that_left_the_library_does_not_sink_the_batch() {
    let dir = tempfile::tempdir().unwrap();
    let (mut library, id) = one_track_library(dir.path());

    let recorded = history::record(
        &mut library,
        &[
            Listen { track_id: 9_999, ms_played: 900, completed: true },
            Listen { track_id: id, ms_played: 900, completed: true },
        ],
        1_700_000_000,
    )
    .unwrap();

    assert_eq!(recorded, 1, "the real track still counts");
    assert_eq!(counts(&library, id).0, 1);
}

#[test]
fn play_counts_survive_a_rescan_but_are_the_only_thing_that_cannot_be_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let (mut library, id) = one_track_library(dir.path());
    history::record(
        &mut library,
        &[Listen { track_id: id, ms_played: 900, completed: true }],
        1_700_000_000,
    )
    .unwrap();

    // Rescanning must not reset what the user has listened to.
    index::sync(&mut library, dir.path()).unwrap();
    assert_eq!(counts(&library, id).0, 1);
}
