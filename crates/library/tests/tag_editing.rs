mod common;

use std::path::Path;

use common::{tag, write_wav_seeded};
use dubplate_library::tags::{
    self, ArtworkChange, Field, FieldChange, NameFields, TagEdit,
};
use dubplate_library::undo::{self, UndoStore};
use dubplate_library::{index, Library};

/// Every write needs somewhere to put what it replaced.
fn store(dir: &Path) -> UndoStore {
    UndoStore::new(dir.join(".undo"))
}

fn library_with(dir: &Path, names: &[&str]) -> Library {
    for (position, name) in names.iter().enumerate() {
        let path = dir.join(format!("{name}.wav"));
        write_wav_seeded(&path, 44_100, 16, 2, 44_100, position as u8 + 1);
    }
    let mut library = Library::open_in_memory().unwrap();
    index::sync(&mut library, dir).unwrap();
    library
}

fn ids(library: &Library) -> Vec<i64> {
    let conn = library.connection();
    let mut stmt = conn.prepare("SELECT id FROM tracks ORDER BY path").unwrap();
    let rows = stmt.query_map([], |row| row.get(0)).unwrap();
    rows.collect::<rusqlite::Result<Vec<i64>>>().unwrap()
}

fn set(field: Field, value: &str) -> FieldChange {
    FieldChange {
        field,
        value: Some(value.to_owned()),
    }
}

#[test]
fn a_written_tag_can_be_read_back_from_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut library = library_with(dir.path(), &["one"]);
    let ids = ids(&library);

    let edit = TagEdit {
        fields: vec![
            set(Field::Title, "A Title"),
            set(Field::Artist, "An Artist"),
            set(Field::TrackNumber, "7"),
            set(Field::Comment, "a note"),
        ],
        artwork: None,
    };
    let report = tags::write(&mut library, &store(dir.path()), &ids, &edit).unwrap();
    assert_eq!(report.written, 1, "{:?}", report.outcomes);
    assert_eq!(report.failed, 0);

    let values = tags::read_one(&dir.path().join("one.wav")).unwrap();
    assert_eq!(values.get(&Field::Title).map(String::as_str), Some("A Title"));
    assert_eq!(values.get(&Field::Artist).map(String::as_str), Some("An Artist"));
    assert_eq!(values.get(&Field::TrackNumber).map(String::as_str), Some("7"));
    assert_eq!(values.get(&Field::Comment).map(String::as_str), Some("a note"));
}

#[test]
fn a_wav_is_tagged_in_both_conventions_it_supports() {
    // Rekordbox and Serato read the ID3v2 chunk; the Finder and older tools
    // read RIFF INFO. Writing one and not the other means half the software
    // that opens the file sees nothing.
    use lofty::file::TaggedFileExt;
    use lofty::prelude::Accessor;
    use lofty::tag::{ItemKey, TagType};

    let dir = tempfile::tempdir().unwrap();
    let mut library = library_with(dir.path(), &["one"]);
    let ids = ids(&library);

    tags::write(
        &mut library,
        &store(dir.path()),
        &ids,
        &TagEdit {
            fields: vec![set(Field::Title, "Both Places")],
            artwork: None,
        },
    )
    .unwrap();

    let file = lofty::read_from_path(dir.path().join("one.wav")).unwrap();
    let id3 = file.tag(TagType::Id3v2).expect("an ID3v2 chunk");
    assert_eq!(id3.title().as_deref(), Some("Both Places"));
    let riff = file.tag(TagType::RiffInfo).expect("a RIFF INFO chunk");
    assert_eq!(riff.get_string(ItemKey::TrackTitle), Some("Both Places"));
}

#[test]
fn a_selection_that_disagrees_reports_varying_rather_than_a_value() {
    let dir = tempfile::tempdir().unwrap();
    let library = library_with(dir.path(), &["one", "two"]);
    tag(&dir.path().join("one.wav"), "First", "Shared Artist", "An Album");
    tag(&dir.path().join("two.wav"), "Second", "Shared Artist", "An Album");
    let ids = ids(&library);

    let values = tags::read_fields(&library, &ids).unwrap();
    let field = |want: Field| values.iter().find(|v| v.field == want).unwrap();

    // Agreed: one value, no "multiple".
    assert_eq!(field(Field::Artist).value.as_deref(), Some("Shared Artist"));
    assert!(!field(Field::Artist).varies);

    // Disagreed: no value at all, so an editor cannot accidentally write one
    // track's title onto the other.
    assert_eq!(field(Field::Title).value, None);
    assert!(field(Field::Title).varies);
}

#[test]
fn editing_one_field_across_a_selection_leaves_the_others_alone() {
    // The reason a change list is explicit rather than a whole tag: setting the
    // album artist for twelve tracks must not flatten twelve different titles
    // into whichever one the editor happened to load first.
    let dir = tempfile::tempdir().unwrap();
    let mut library = library_with(dir.path(), &["one", "two"]);
    tag(&dir.path().join("one.wav"), "First", "A", "Album");
    tag(&dir.path().join("two.wav"), "Second", "B", "Album");
    let ids = ids(&library);

    tags::write(
        &mut library,
        &store(dir.path()),
        &ids,
        &TagEdit {
            fields: vec![set(Field::AlbumArtist, "Various")],
            artwork: None,
        },
    )
    .unwrap();

    let one = tags::read_one(&dir.path().join("one.wav")).unwrap();
    let two = tags::read_one(&dir.path().join("two.wav")).unwrap();
    assert_eq!(one.get(&Field::AlbumArtist).map(String::as_str), Some("Various"));
    assert_eq!(two.get(&Field::AlbumArtist).map(String::as_str), Some("Various"));
    assert_eq!(one.get(&Field::Title).map(String::as_str), Some("First"));
    assert_eq!(two.get(&Field::Title).map(String::as_str), Some("Second"));
}

#[test]
fn an_empty_value_clears_the_field() {
    let dir = tempfile::tempdir().unwrap();
    let mut library = library_with(dir.path(), &["one"]);
    tag(&dir.path().join("one.wav"), "Title", "Artist", "Album");
    let ids = ids(&library);

    tags::write(
        &mut library,
        &store(dir.path()),
        &ids,
        &TagEdit {
            fields: vec![FieldChange {
                field: Field::Album,
                value: None,
            }],
            artwork: None,
        },
    )
    .unwrap();

    let values = tags::read_one(&dir.path().join("one.wav")).unwrap();
    assert_eq!(values.get(&Field::Album), None, "cleared");
    assert_eq!(values.get(&Field::Title).map(String::as_str), Some("Title"));
}

#[test]
fn a_number_field_refuses_something_that_is_not_a_number() {
    let dir = tempfile::tempdir().unwrap();
    let mut library = library_with(dir.path(), &["one"]);
    let ids = ids(&library);

    let report = tags::write(
        &mut library,
        &store(dir.path()),
        &ids,
        &TagEdit {
            fields: vec![set(Field::Year, "nineteen eighty four")],
            artwork: None,
        },
    )
    .unwrap();
    assert_eq!(report.written, 0);
    assert_eq!(report.failed, 1);
    assert!(report.outcomes[0].error.as_ref().unwrap().contains("number"));
}

#[test]
fn a_tag_edit_does_not_cost_the_track_its_analysis() {
    // The failure this is built to prevent. A tag write changes the file's
    // first 64KB, which is what the content key hashes, so a naive rescan
    // decides the audio was replaced and throws away tempo, key, bit depth and
    // spectral figures. Recording the new (mtime, size, content_key) as part of
    // the write means the next scan skips the file entirely.
    let dir = tempfile::tempdir().unwrap();
    let mut library = library_with(dir.path(), &["one"]);
    let ids = ids(&library);

    library
        .connection()
        .execute(
            "UPDATE tracks SET analyzed_at = 111, bpm = 128.0, music_key = '8A',
                               effective_bits = 16, spectral_cutoff = 21000",
            [],
        )
        .unwrap();

    tags::write(
        &mut library,
        &store(dir.path()),
        &ids,
        &TagEdit {
            fields: vec![set(Field::Title, "Renamed")],
            artwork: None,
        },
    )
    .unwrap();

    // A full rescan, exactly as the watcher would trigger.
    let report = index::sync(&mut library, dir.path()).unwrap();
    assert_eq!(report.unchanged, 1, "the rewritten file should look unchanged");

    let (bpm, key, analysed): (Option<f64>, Option<String>, Option<i64>) = library
        .connection()
        .query_row(
            "SELECT bpm, music_key, analyzed_at FROM tracks",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(bpm, Some(128.0), "tempo survived a title edit");
    assert_eq!(key.as_deref(), Some("8A"));
    assert_eq!(analysed, Some(111));
}

#[test]
fn a_failed_write_leaves_the_original_file_untouched() {
    // Writes go through a copy and a rename. If the write fails, the original
    // must still be there and still be playable -- these files are the point of
    // the whole application.
    let dir = tempfile::tempdir().unwrap();
    let mut library = library_with(dir.path(), &["one"]);
    let ids = ids(&library);
    let path = dir.path().join("one.wav");
    let before = std::fs::read(&path).unwrap();

    let report = tags::write(
        &mut library,
        &store(dir.path()),
        &ids,
        &TagEdit {
            fields: vec![set(Field::TrackNumber, "not a number")],
            artwork: None,
        },
    )
    .unwrap();
    assert_eq!(report.failed, 1);
    assert_eq!(std::fs::read(&path).unwrap(), before, "byte for byte");
    // And no temporary file was left lying beside it.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains("dubplate-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn artwork_can_be_added_and_removed() {
    let dir = tempfile::tempdir().unwrap();
    let mut library = library_with(dir.path(), &["one"]);
    let ids = ids(&library);
    let path = dir.path().join("one.wav");

    // A one-pixel PNG is a real image as far as any decoder is concerned.
    let png = dir.path().join("cover.png");
    std::fs::write(&png, ONE_PIXEL_PNG).unwrap();

    assert!(!tags::has_artwork(&path));
    let report = tags::write(
        &mut library,
        &store(dir.path()),
        &ids,
        &TagEdit {
            fields: vec![],
            artwork: Some(ArtworkChange::Set {
                path: png.to_string_lossy().into_owned(),
            }),
        },
    )
    .unwrap();
    assert_eq!(report.written, 1, "{:?}", report.outcomes);
    assert!(tags::has_artwork(&path), "cover was embedded");

    tags::write(
        &mut library,
        &store(dir.path()),
        &ids,
        &TagEdit {
            fields: vec![],
            artwork: Some(ArtworkChange::Remove),
        },
    )
    .unwrap();
    assert!(!tags::has_artwork(&path), "cover was removed");
}

#[test]
fn something_that_is_not_an_image_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut library = library_with(dir.path(), &["one"]);
    let ids = ids(&library);
    let fake = dir.path().join("notes.txt");
    std::fs::write(&fake, b"this is not a picture").unwrap();

    let report = tags::write(
        &mut library,
        &store(dir.path()),
        &ids,
        &TagEdit {
            fields: vec![],
            artwork: Some(ArtworkChange::Set {
                path: fake.to_string_lossy().into_owned(),
            }),
        },
    )
    .unwrap();
    assert_eq!(report.failed, 1);
    assert!(!tags::has_artwork(&dir.path().join("one.wav")));
}

#[test]
fn filenames_fill_in_what_is_missing_without_overwriting_what_is_there() {
    let dir = tempfile::tempdir().unwrap();
    let mut library = library_with(dir.path(), &["Some Artist - Some Title"]);
    let ids = ids(&library);
    let path = dir.path().join("Some Artist - Some Title.wav");

    // The file already claims a title. The filename must not stamp over it.
    tag(&path, "The Real Title", "", "");

    let preview = tags::preview_names(&library, Some(&ids), false).unwrap();
    assert_eq!(preview.len(), 1);
    assert_eq!(preview[0].guess.artist.as_deref(), Some("Some Artist"));
    assert_eq!(preview[0].current_title.as_deref(), Some("The Real Title"));

    let report = tags::apply_names(&mut library, &store(dir.path()), &ids, NameFields::default()).unwrap();
    assert_eq!(report.written, 1, "{:?}", report.outcomes);

    let values = tags::read_one(&path).unwrap();
    assert_eq!(values.get(&Field::Artist).map(String::as_str), Some("Some Artist"));
    assert_eq!(
        values.get(&Field::Title).map(String::as_str),
        Some("The Real Title"),
        "an existing title is not replaced by a guess"
    );
}

#[test]
fn overwriting_is_possible_but_has_to_be_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let mut library = library_with(dir.path(), &["Some Artist - Some Title"]);
    let ids = ids(&library);
    let path = dir.path().join("Some Artist - Some Title.wav");
    tag(&path, "The Real Title", "", "");

    tags::apply_names(
        &mut library,
        &store(dir.path()),
        &ids,
        NameFields {
            overwrite: true,
            ..NameFields::default()
        },
    )
    .unwrap();

    let values = tags::read_one(&path).unwrap();
    assert_eq!(values.get(&Field::Title).map(String::as_str), Some("Some Title"));
}

#[test]
fn a_preview_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let library = library_with(dir.path(), &["Artist - Title"]);
    let path = dir.path().join("Artist - Title.wav");
    let before = std::fs::read(&path).unwrap();

    let preview = tags::preview_names(&library, None, false).unwrap();
    assert_eq!(preview.len(), 1);
    assert!(preview[0].changes);
    assert_eq!(std::fs::read(&path).unwrap(), before, "byte for byte");
}

/// The smallest valid PNG: one opaque pixel.
const ONE_PIXEL_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

#[test]
fn undo_restores_what_a_field_edit_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let mut library = library_with(dir.path(), &["one"]);
    let path = dir.path().join("one.wav");
    tag(&path, "Original", "Original Artist", "Original Album");
    let ids = ids(&library);

    tags::write(
        &mut library,
        &store,
        &ids,
        &TagEdit {
            fields: vec![set(Field::Title, "Replaced"), set(Field::Genre, "House")],
            artwork: None,
        },
    )
    .unwrap();
    assert_eq!(
        tags::read_one(&path).unwrap().get(&Field::Title).map(String::as_str),
        Some("Replaced")
    );

    let batch = undo::newest(&library).unwrap().expect("a batch to undo");
    let report = tags::undo_batch(&mut library, &store, batch).unwrap();
    assert_eq!(report.written, 1, "{:?}", report.outcomes);

    let values = tags::read_one(&path).unwrap();
    assert_eq!(values.get(&Field::Title).map(String::as_str), Some("Original"));
    // Genre had no value before, so undoing must clear it rather than leave
    // "House" or write an empty string.
    assert_eq!(values.get(&Field::Genre), None);
    // And a field the edit never touched is still exactly as it was.
    assert_eq!(values.get(&Field::Album).map(String::as_str), Some("Original Album"));
}

#[test]
fn undo_only_touches_the_fields_the_edit_did() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let mut library = library_with(dir.path(), &["one"]);
    let path = dir.path().join("one.wav");
    tag(&path, "Title", "Artist", "Album");
    let ids = ids(&library);

    tags::write(
        &mut library,
        &store,
        &ids,
        &TagEdit {
            fields: vec![set(Field::Comment, "a note")],
            artwork: None,
        },
    )
    .unwrap();

    // Something else changes the title in between, the way another tagger
    // would. Undoing the comment must not resurrect an old title.
    tag(&path, "Changed Elsewhere", "Artist", "Album");

    let batch = undo::newest(&library).unwrap().unwrap();
    tags::undo_batch(&mut library, &store, batch).unwrap();

    let values = tags::read_one(&path).unwrap();
    assert_eq!(values.get(&Field::Comment), None, "the comment came off");
    assert_eq!(
        values.get(&Field::Title).map(String::as_str),
        Some("Changed Elsewhere"),
        "an untouched field is left alone"
    );
}

#[test]
fn undo_brings_a_replaced_cover_back() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let mut library = library_with(dir.path(), &["one"]);
    let path = dir.path().join("one.wav");
    let first = dir.path().join("first.png");
    std::fs::write(&first, ONE_PIXEL_PNG).unwrap();
    let ids = ids(&library);

    // Give it a cover, then remove it, then undo the removal.
    tags::write(
        &mut library,
        &store,
        &ids,
        &TagEdit {
            fields: vec![],
            artwork: Some(ArtworkChange::Set {
                path: first.to_string_lossy().into_owned(),
            }),
        },
    )
    .unwrap();
    assert!(tags::has_artwork(&path));

    tags::write(
        &mut library,
        &store,
        &ids,
        &TagEdit {
            fields: vec![],
            artwork: Some(ArtworkChange::Remove),
        },
    )
    .unwrap();
    assert!(!tags::has_artwork(&path));

    let batch = undo::newest(&library).unwrap().unwrap();
    tags::undo_batch(&mut library, &store, batch).unwrap();
    assert!(tags::has_artwork(&path), "the cover came back");
}

#[test]
fn undoing_an_added_cover_removes_it_again() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let mut library = library_with(dir.path(), &["one"]);
    let path = dir.path().join("one.wav");
    let png = dir.path().join("cover.png");
    std::fs::write(&png, ONE_PIXEL_PNG).unwrap();
    let ids = ids(&library);

    tags::write(
        &mut library,
        &store,
        &ids,
        &TagEdit {
            fields: vec![],
            artwork: Some(ArtworkChange::Set {
                path: png.to_string_lossy().into_owned(),
            }),
        },
    )
    .unwrap();
    assert!(tags::has_artwork(&path));

    let batch = undo::newest(&library).unwrap().unwrap();
    tags::undo_batch(&mut library, &store, batch).unwrap();
    assert!(!tags::has_artwork(&path), "there was no cover before, so there is none now");
}

#[test]
fn a_bulk_filename_apply_can_be_taken_back_in_one_go() {
    // The operation undo exists for: a hundred and fifty files at once.
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let mut library = library_with(
        dir.path(),
        &["A - One", "B - Two", "C - Three"],
    );
    let ids = ids(&library);

    let report = tags::apply_names(&mut library, &store, &ids, NameFields::default()).unwrap();
    assert_eq!(report.written, 3, "{:?}", report.outcomes);
    for (name, artist) in [("A - One", "A"), ("B - Two", "B"), ("C - Three", "C")] {
        let values = tags::read_one(&dir.path().join(format!("{name}.wav"))).unwrap();
        assert_eq!(values.get(&Field::Artist).map(String::as_str), Some(artist));
    }

    let batch = undo::newest(&library).unwrap().unwrap();
    let report = tags::undo_batch(&mut library, &store, batch).unwrap();
    assert_eq!(report.written, 3);

    for name in ["A - One", "B - Two", "C - Three"] {
        let values = tags::read_one(&dir.path().join(format!("{name}.wav"))).unwrap();
        assert_eq!(values.get(&Field::Artist), None, "{name} went back to untagged");
        assert_eq!(values.get(&Field::Title), None);
    }
}

#[test]
fn undoing_does_not_cost_the_track_its_analysis_either() {
    // Undo is a write like any other, so it has the same obligation.
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let mut library = library_with(dir.path(), &["one"]);
    let ids = ids(&library);

    tags::write(
        &mut library,
        &store,
        &ids,
        &TagEdit {
            fields: vec![set(Field::Title, "Something")],
            artwork: None,
        },
    )
    .unwrap();
    library
        .connection()
        .execute("UPDATE tracks SET analyzed_at = 222, bpm = 140.0", [])
        .unwrap();

    let batch = undo::newest(&library).unwrap().unwrap();
    tags::undo_batch(&mut library, &store, batch).unwrap();

    let report = index::sync(&mut library, dir.path()).unwrap();
    assert_eq!(report.unchanged, 1);
    let bpm: Option<f64> = library
        .connection()
        .query_row("SELECT bpm FROM tracks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(bpm, Some(140.0));
}

#[test]
fn several_operations_undo_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let mut library = library_with(dir.path(), &["one"]);
    let path = dir.path().join("one.wav");
    let ids = ids(&library);

    for title in ["First", "Second", "Third"] {
        tags::write(
            &mut library,
            &store,
            &ids,
            &TagEdit {
                fields: vec![set(Field::Title, title)],
                artwork: None,
            },
        )
        .unwrap();
    }
    assert_eq!(undo::history(&library).unwrap().len(), 3);

    // Walking back one at a time returns each earlier state in turn.
    for expected in ["Second", "First", ""] {
        let batch = undo::newest(&library).unwrap().unwrap();
        tags::undo_batch(&mut library, &store, batch).unwrap();
        let values = tags::read_one(&path).unwrap();
        let title = values.get(&Field::Title).cloned().unwrap_or_default();
        assert_eq!(title, expected);
    }
    assert!(undo::history(&library).unwrap().is_empty(), "nothing left to undo");
    assert_eq!(undo::newest(&library).unwrap(), None);
}

#[test]
fn history_is_bounded_and_old_covers_are_swept_up() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let mut library = library_with(dir.path(), &["one"]);
    let ids = ids(&library);

    for n in 0..(undo::HISTORY + 6) {
        tags::write(
            &mut library,
            &store,
            &ids,
            &TagEdit {
                fields: vec![set(Field::Title, &format!("Take {n}"))],
                artwork: None,
            },
        )
        .unwrap();
    }

    let history = undo::history(&library).unwrap();
    assert_eq!(history.len(), undo::HISTORY, "older operations were pruned");
    // And nothing was orphaned in the blob store.
    let blobs = std::fs::read_dir(store.root())
        .map(|entries| entries.flatten().count())
        .unwrap_or(0);
    assert_eq!(blobs, 0, "no covers were involved, so none are kept");
}

#[test]
fn a_write_that_failed_is_not_offered_for_undo() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let mut library = library_with(dir.path(), &["one"]);
    let ids = ids(&library);

    let report = tags::write(
        &mut library,
        &store,
        &ids,
        &TagEdit {
            fields: vec![set(Field::Year, "not a year")],
            artwork: None,
        },
    )
    .unwrap();
    assert_eq!(report.failed, 1);
    assert!(
        undo::history(&library).unwrap().is_empty(),
        "nothing happened, so there is nothing to take back"
    );
}

#[test]
fn a_wav_that_already_had_id3_still_gets_both_conventions() {
    // The order tags are saved in decides whether both survive. Written the
    // wrong way round, the RIFF INFO chunk goes in as far as memory, the save
    // reports success, and the chunk is simply absent when the file is read
    // back -- so a file that arrived with ID3v2 tags would quietly keep only
    // half of what this writes.
    use lofty::file::TaggedFileExt;
    use lofty::prelude::Accessor;
    use lofty::tag::{ItemKey, TagType};

    let dir = tempfile::tempdir().unwrap();
    let mut library = library_with(dir.path(), &["one"]);
    let path = dir.path().join("one.wav");
    // Arrives with an ID3v2 chunk and nothing else, as a real download does.
    tag(&path, "Before", "Artist", "Album");
    let ids = ids(&library);

    tags::write(
        &mut library,
        &store(dir.path()),
        &ids,
        &TagEdit {
            fields: vec![set(Field::Title, "After")],
            artwork: None,
        },
    )
    .unwrap();

    let file = lofty::read_from_path(&path).unwrap();
    assert_eq!(
        file.tag(TagType::Id3v2).and_then(|t| t.title()).as_deref(),
        Some("After")
    );
    let riff = file.tag(TagType::RiffInfo).expect("a RIFF INFO chunk");
    assert_eq!(riff.get_string(ItemKey::TrackTitle), Some("After"));
    // The new chunk carries what the file already said, not just the one
    // field that was edited, so the two conventions cannot disagree.
    assert_eq!(riff.get_string(ItemKey::TrackArtist), Some("Artist"));
    assert_eq!(riff.get_string(ItemKey::AlbumTitle), Some("Album"));
}
