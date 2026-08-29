//! End-to-end check of tag writing and undo on real files.
//!
//!     cargo run --release -p dubplate-analysis --example tagcheck -- <dir>
//!
//! The point is the audio hash. Tags live in the same file as the samples, and
//! a tagger that quietly re-encodes, truncates or shifts the audio would be the
//! worst possible bug here -- so every file's decoded samples are hashed before
//! anything is written and compared after each step.

use std::path::Path;

use dubplate_audio::decode::TrackDecoder;
use dubplate_library::tags::{self, Field, FieldChange, TagEdit};
use dubplate_library::undo::{self, UndoStore};
use dubplate_library::{index, Library};

/// Hash every decoded sample. Identical before and after means the audio was
/// not touched, whatever happened to the tags around it.
fn audio_hash(path: &Path) -> anyhow::Result<(String, u64)> {
    let mut decoder = TrackDecoder::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut frames = 0u64;
    let channels = decoder.format().channels.max(1) as usize;
    while let Some(block) = decoder.next_block()? {
        for sample in block {
            hasher.update(&sample.to_le_bytes());
        }
        frames += (block.len() / channels) as u64;
    }
    Ok((hasher.finalize().to_hex()[..16].to_owned(), frames))
}

fn main() -> anyhow::Result<()> {
    let dir = std::env::args().nth(1).expect("a directory of files to test");
    let dir = Path::new(&dir);
    let store = UndoStore::new(dir.join(".undo"));
    let mut library = Library::open_in_memory()?;
    let report = index::sync(&mut library, dir)?;
    println!("indexed {} tracks\n", report.added);

    let rows: Vec<(i64, String)> = {
        let conn = library.connection();
        let mut stmt = conn.prepare("SELECT id, path FROM tracks ORDER BY path")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    // Before: tags and audio.
    let mut before = Vec::new();
    for (id, path) in &rows {
        let file = Path::new(path);
        let (hash, frames) = audio_hash(file)?;
        let values = tags::read_one(file)?;
        println!(
            "── {}\n   audio  {hash}  {frames} frames\n   title  {:?}\n   artist {:?}",
            file.file_name().unwrap_or_default().to_string_lossy(),
            values.get(&Field::Title),
            values.get(&Field::Artist),
        );
        before.push((*id, path.clone(), hash, frames, values));
    }

    let ids: Vec<i64> = rows.iter().map(|(id, _)| *id).collect();

    println!("\n== writing ==");
    let written = tags::write(
        &mut library,
        &store,
        &ids,
        &TagEdit {
            fields: vec![
                FieldChange { field: Field::Artist, value: Some("DUBPLATE TEST".into()) },
                FieldChange { field: Field::Comment, value: Some("written by tagcheck".into()) },
            ],
            artwork: None,
        },
    )?;
    println!("written {} failed {}", written.written, written.failed);
    for outcome in &written.outcomes {
        if let Some(error) = &outcome.error {
            println!("  !! {} — {error}", outcome.path);
        }
    }

    let mut problems = 0usize;
    for (_, path, hash, frames, _) in &before {
        let file = Path::new(path);
        let (now, now_frames) = audio_hash(file)?;
        let values = tags::read_one(file)?;
        let name = file.file_name().unwrap_or_default().to_string_lossy();
        if now != *hash || now_frames != *frames {
            println!("  !! {name}: AUDIO CHANGED {hash}/{frames} -> {now}/{now_frames}");
            problems += 1;
        }
        if values.get(&Field::Artist).map(String::as_str) != Some("DUBPLATE TEST") {
            println!("  !! {name}: artist not written, got {:?}", values.get(&Field::Artist));
            problems += 1;
        }
    }
    println!("after write: {problems} problems");

    println!("\n== undoing ==");
    let batch = undo::newest(&library)?.expect("a batch to undo");
    let undone = tags::undo_batch(&mut library, &store, batch)?;
    println!("restored {} failed {}", undone.written, undone.failed);

    for (_, path, hash, frames, original) in &before {
        let file = Path::new(path);
        let (now, now_frames) = audio_hash(file)?;
        let values = tags::read_one(file)?;
        let name = file.file_name().unwrap_or_default().to_string_lossy();
        if now != *hash || now_frames != *frames {
            println!("  !! {name}: AUDIO CHANGED after undo");
            problems += 1;
        }
        for field in [Field::Artist, Field::Comment, Field::Title, Field::Album] {
            if values.get(&field) != original.get(&field) {
                println!(
                    "  !! {name}: {field:?} did not come back — {:?} vs {:?}",
                    values.get(&field),
                    original.get(&field)
                );
                problems += 1;
            }
        }
    }

    println!("\n{}", if problems == 0 { "ALL CLEAN" } else { "PROBLEMS FOUND" });
    Ok(())
}
