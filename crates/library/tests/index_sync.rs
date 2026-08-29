mod common;

use std::fs;

use common::{tag, touch_forward, write_wav_seeded};
use dubplate_library::{index, query, Library};

fn library_with(dir: &std::path::Path) -> Library {
    let mut library = Library::open_in_memory().unwrap();
    index::sync(&mut library, dir).unwrap();
    library
}

fn count(library: &Library, sql: &str) -> i64 {
    library.connection().query_row(sql, [], |row| row.get(0)).unwrap()
}

fn play_count(library: &Library, path: &str) -> i64 {
    library
        .connection()
        .query_row(
            "SELECT play_count FROM tracks WHERE path = ?1",
            [path],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn first_sync_indexes_every_file() {
    let dir = tempfile::tempdir().unwrap();
    write_wav_seeded(&dir.path().join("a.wav"), 44_100, 16, 2, 44_100, 1);
    write_wav_seeded(&dir.path().join("b.wav"), 44_100, 16, 2, 44_100, 2);

    let mut library = Library::open_in_memory().unwrap();
    let report = index::sync(&mut library, dir.path()).unwrap();

    assert_eq!(report.files_seen, 2);
    assert_eq!(report.added, 2);
    assert_eq!(report.updated, 0);
    assert_eq!(report.removed, 0);
    assert_eq!(query::list_tracks(&library).unwrap().len(), 2);
}

#[test]
fn a_second_sync_reopens_nothing() {
    let dir = tempfile::tempdir().unwrap();
    write_wav_seeded(&dir.path().join("a.wav"), 44_100, 16, 2, 44_100, 1);
    write_wav_seeded(&dir.path().join("b.wav"), 44_100, 16, 2, 44_100, 2);

    let mut library = library_with(dir.path());
    let report = index::sync(&mut library, dir.path()).unwrap();

    assert_eq!(report.unchanged, 2, "(mtime, size) still matches the index");
    assert_eq!(report.added, 0);
    assert_eq!(report.updated, 0);
    assert_eq!(report.removed, 0);
}

#[test]
fn a_renamed_file_keeps_its_identity_and_play_count() {
    let dir = tempfile::tempdir().unwrap();
    let before = dir.path().join("old name.wav");
    write_wav_seeded(&before, 44_100, 16, 2, 44_100, 7);

    let mut library = library_with(dir.path());
    let id_before: i64 = library
        .connection()
        .query_row("SELECT id FROM tracks", [], |row| row.get(0))
        .unwrap();
    library
        .connection()
        .execute("UPDATE tracks SET play_count = 42", [])
        .unwrap();

    // Same bytes, new path: a rename, not a delete plus an add.
    let after = dir.path().join("Artist - Title.wav");
    fs::rename(&before, &after).unwrap();

    let report = index::sync(&mut library, dir.path()).unwrap();

    assert_eq!(report.moved, 1, "matched by content, not by path");
    assert_eq!(report.added, 0);
    assert_eq!(report.removed, 0);

    let id_after: i64 = library
        .connection()
        .query_row("SELECT id FROM tracks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(id_after, id_before, "the row must be updated, not replaced");
    assert_eq!(play_count(&library, after.to_str().unwrap()), 42);
}

#[test]
fn a_file_moved_into_a_subfolder_is_still_a_move() {
    let dir = tempfile::tempdir().unwrap();
    let before = dir.path().join("loose.wav");
    write_wav_seeded(&before, 44_100, 16, 2, 44_100, 9);

    let mut library = library_with(dir.path());
    library
        .connection()
        .execute("UPDATE tracks SET play_count = 3", [])
        .unwrap();

    let nested = dir.path().join("Artist").join("Album");
    fs::create_dir_all(&nested).unwrap();
    let after = nested.join("01 loose.wav");
    fs::rename(&before, &after).unwrap();

    let report = index::sync(&mut library, dir.path()).unwrap();

    assert_eq!(report.moved, 1);
    assert_eq!(play_count(&library, after.to_str().unwrap()), 3);
}

#[test]
fn a_deleted_file_leaves_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gone.wav");
    write_wav_seeded(&path, 44_100, 16, 2, 44_100, 4);

    let mut library = library_with(dir.path());
    fs::remove_file(&path).unwrap();

    let report = index::sync(&mut library, dir.path()).unwrap();

    assert_eq!(report.removed, 1);
    assert_eq!(report.moved, 0);
    assert!(query::list_tracks(&library).unwrap().is_empty());
}

#[test]
fn rewriting_a_file_updates_it_and_discards_stale_analysis() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("track.wav");
    write_wav_seeded(&path, 44_100, 16, 2, 44_100, 1);

    let mut library = library_with(dir.path());
    library
        .connection()
        .execute(
            "UPDATE tracks SET analyzed_at = 1700000000, bpm = 128.0, rg_track_gain = -7.5",
            [],
        )
        .unwrap();

    // Different bytes at the same path: re-encoded or re-tagged.
    write_wav_seeded(&path, 44_100, 16, 2, 88_200, 5);
    touch_forward(&path);

    let report = index::sync(&mut library, dir.path()).unwrap();
    assert_eq!(report.updated, 1);
    assert_eq!(report.added, 0);

    let (analyzed, bpm): (Option<i64>, Option<f64>) = library
        .connection()
        .query_row("SELECT analyzed_at, bpm FROM tracks", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(analyzed, None, "analysis describes bytes that are gone");
    assert_eq!(bpm, None);
}

#[test]
fn search_matches_prefixes_across_tags() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("01.wav");
    write_wav_seeded(&path, 44_100, 16, 2, 44_100, 3);
    tag(&path, "Hyperballad", "Björk", "Post");

    let mut library = Library::open_in_memory().unwrap();
    index::sync(&mut library, dir.path()).unwrap();

    // Partial word, and the accent folded away.
    let hits = query::search(&library, "bjor", 20).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title.as_deref(), Some("Hyperballad"));

    // Terms are ANDed across columns.
    assert_eq!(query::search(&library, "hyper post", 20).unwrap().len(), 1);
    assert!(query::search(&library, "hyper nirvana", 20).unwrap().is_empty());
}

#[test]
fn untagged_files_are_findable_by_filename() {
    let dir = tempfile::tempdir().unwrap();
    write_wav_seeded(
        &dir.path().join("Yellow Claw - Amsterdamned.wav"),
        44_100,
        16,
        2,
        44_100,
        6,
    );

    let mut library = Library::open_in_memory().unwrap();
    index::sync(&mut library, dir.path()).unwrap();

    // No tags at all, so without the filename fallback this would be invisible.
    let hits = query::search(&library, "amsterd", 20).unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn artists_and_albums_are_pruned_when_their_last_track_goes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("only.wav");
    write_wav_seeded(&path, 44_100, 16, 2, 44_100, 8);
    tag(&path, "Only Track", "Lone Artist", "Lone Album");

    let mut library = Library::open_in_memory().unwrap();
    index::sync(&mut library, dir.path()).unwrap();

    assert_eq!(count(&library, "SELECT count(*) FROM artists"), 1);
    assert_eq!(count(&library, "SELECT count(*) FROM albums"), 1);

    fs::remove_file(&path).unwrap();
    index::sync(&mut library, dir.path()).unwrap();

    assert_eq!(count(&library, "SELECT count(*) FROM artists"), 0, "orphan artist");
    assert_eq!(count(&library, "SELECT count(*) FROM albums"), 0, "orphan album");
}
