//! Guess artist and title from filenames, over a whole library.
//!
//!     cargo run --release -p dubplate-library --example names -- ~/Music/DJ
//!
//! The point is the same as the transcode sweep: a parser is only trustworthy
//! if you have watched it run over a real collection and read what it produced.
//! It prints every guess so they can be judged, not just a success count.

use std::path::Path;

use dubplate_library::filename;

fn main() -> anyhow::Result<()> {
    let root = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let show_all = std::env::args().any(|a| a == "--all");

    let mut total = 0usize;
    let mut with_artist = 0usize;
    let mut with_bpm = 0usize;
    let mut with_track = 0usize;
    let mut no_title = 0usize;
    let mut samples = Vec::new();

    visit(Path::new(&root), &mut |path| {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        if !matches!(
            path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref(),
            Some("flac" | "wav" | "mp3" | "m4a" | "aiff" | "aif" | "ogg" | "opus")
        ) {
            return;
        }
        total += 1;
        let guess = filename::parse(name);
        if guess.artist.is_some() {
            with_artist += 1;
        }
        if guess.bpm.is_some() {
            with_bpm += 1;
        }
        if let Some(track) = guess.track {
            with_track += 1;
            // A track number above 40 is more likely a year, a bitrate or part
            // of the title than a position on a record.
            if track > 40 {
                println!("?? track {track} from {name:?} -> {:?}", guess.title);
            }
        }
        if guess.title.is_none() {
            no_title += 1;
            println!("!! no title from {name:?}");
        }
        if show_all || samples.len() < 40 {
            samples.push((name.to_owned(), guess));
        }
    });

    for (name, guess) in &samples {
        println!("── {name}");
        println!(
            "   artist {:<28} title {}",
            guess.artist.as_deref().unwrap_or("—"),
            guess.title.as_deref().unwrap_or("—")
        );
        if guess.bpm.is_some() || guess.key.is_some() {
            println!(
                "   bpm {:<6} key {}",
                guess.bpm.map(|b| b.to_string()).unwrap_or_else(|| "—".into()),
                guess.key.as_deref().unwrap_or("—")
            );
        }
    }

    println!("\nfiles        {total}");
    println!("with artist  {with_artist}");
    println!("with track   {with_track}");
    println!("with bpm     {with_bpm}");
    println!("no title     {no_title}");
    Ok(())
}

fn visit(dir: &Path, f: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit(&path, f);
        } else {
            f(&path);
        }
    }
}
