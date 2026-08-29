mod common;

use common::{tag, write_wav_seeded};
use dubplate_library::smart::{Condition, Field, Op, SmartRules, Sort, Value};
use dubplate_library::{index, playlists, smart, Library};

fn library_with_three(dir: &std::path::Path) -> Library {
    for (index, name) in ["a", "b", "c"].iter().enumerate() {
        let path = dir.join(format!("{name}.wav"));
        write_wav_seeded(&path, 44_100, 16, 2, 44_100 * (index as u32 + 1), index as u8 + 1);
        tag(&path, &format!("Track {name}"), "Someone", "An Album");
    }
    let mut library = Library::open_in_memory().unwrap();
    index::sync(&mut library, dir).unwrap();
    library
}

#[test]
fn a_smart_playlist_answers_from_the_library_as_it_is_now() {
    let dir = tempfile::tempdir().unwrap();
    let library = library_with_three(dir.path());

    // "Never played" starts as everything.
    let rules = SmartRules {
        all: vec![Condition {
            field: Field::PlayCount,
            op: Op::Eq,
            value: Some(Value::Number(0.0)),
        }],
        sort: Sort::Title,
        ..Default::default()
    };
    let id = playlists::create_smart(&library, "Never played", &rules).unwrap();
    assert_eq!(playlists::tracks(&library, id).unwrap().len(), 3);

    // Play one, and it drops out without the playlist being touched.
    library
        .connection()
        .execute("UPDATE tracks SET play_count = 1 WHERE id = (SELECT MIN(id) FROM tracks)", [])
        .unwrap();
    assert_eq!(playlists::tracks(&library, id).unwrap().len(), 2);
}

#[test]
fn rules_survive_being_stored_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let library = library_with_three(dir.path());

    let rules = SmartRules {
        all: vec![Condition {
            field: Field::Title,
            op: Op::Contains,
            value: Some(Value::Text("Track b".into())),
        }],
        ..Default::default()
    };
    let id = playlists::create_smart(&library, "Just b", &rules).unwrap();

    let found = playlists::tracks(&library, id).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].title.as_deref(), Some("Track b"));
}

#[test]
fn a_contains_rule_looks_for_the_text_not_a_pattern() {
    let dir = tempfile::tempdir().unwrap();
    let library = library_with_three(dir.path());

    // A percent sign means a percent sign. If it were pasted into the SQL as a
    // wildcard this would match everything.
    let rules = SmartRules {
        all: vec![Condition {
            field: Field::Title,
            op: Op::Contains,
            value: Some(Value::Text("%".into())),
        }],
        ..Default::default()
    };
    assert!(smart::resolve(&library, &rules).unwrap().is_empty());
}

#[test]
fn every_preset_resolves_without_error() {
    let dir = tempfile::tempdir().unwrap();
    let library = library_with_three(dir.path());

    for (name, rules) in playlists::presets() {
        let resolved = smart::resolve(&library, &rules);
        assert!(resolved.is_ok(), "preset {name} failed: {:?}", resolved.err());
    }
}

#[test]
fn listing_shows_how_many_a_smart_playlist_currently_holds() {
    let dir = tempfile::tempdir().unwrap();
    let library = library_with_three(dir.path());
    playlists::create_smart(&library, "Everything", &SmartRules::default()).unwrap();

    let listed = playlists::list(&library).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].is_smart);
    assert_eq!(listed[0].track_count, 3);
}

#[test]
fn a_deleted_playlist_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let library = library_with_three(dir.path());
    let id = playlists::create_smart(&library, "Temporary", &SmartRules::default()).unwrap();
    playlists::delete(&library, id).unwrap();
    assert!(playlists::list(&library).unwrap().is_empty());
}
