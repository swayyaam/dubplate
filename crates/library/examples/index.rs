//! Exercise the full phase 1 pipeline against a real folder, without the app.
//!
//!     cargo run --release -p dubplate-library --example index -- ~/Music
//!
//! Syncs twice: the first pass builds the index, the second proves an unchanged
//! library costs a directory walk and nothing more.

use std::path::PathBuf;
use std::time::Instant;

use dubplate_library::{artwork, index, query, ArtworkCache, Library};

fn main() -> anyhow::Result<()> {
    let Some(arg) = std::env::args().nth(1) else {
        eprintln!("usage: index <folder>");
        std::process::exit(2);
    };
    let root = PathBuf::from(expand_home(&arg));

    let workdir = tempdir();
    let mut library = Library::open(workdir.join("library.sqlite"))?;
    let cache = ArtworkCache::new(workdir.join("artwork"));

    println!("== cold sync ==");
    let first = index::sync(&mut library, &root)?;
    report(&first);

    println!("\n== artwork ==");
    let art = artwork::build_cache(&mut library, &cache)?;
    println!("albums checked  {}", art.albums_checked);
    println!("covers cached   {}", art.art_found);
    println!("no cover found  {}", art.art_missing);
    println!("elapsed         {} ms", art.elapsed_ms);

    println!("\n== warm sync (nothing changed) ==");
    let second = index::sync(&mut library, &root)?;
    report(&second);

    if std::env::args().any(|arg| arg == "--json") {
        // Feed the real row shape to a UI harness without launching the app.
        println!("{}", serde_json::to_string(&query::list_tracks(&library)?)?);
        return Ok(());
    }

    {
        let conn = library.connection();
        let (with_gain, with_peak): (i64, i64) = conn
            .query_row(
                "SELECT SUM(rg_track_gain IS NOT NULL), SUM(rg_track_peak IS NOT NULL) FROM tracks",
                [],
                |row| Ok((row.get(0).unwrap_or(0), row.get(1).unwrap_or(0))),
            )
            .unwrap_or((0, 0));
        let total: i64 = conn
            .query_row("SELECT count(*) FROM tracks", [], |row| row.get(0))
            .unwrap_or(0);
        println!("\n== replaygain (from tags) ==");
        println!("tracks with gain  {with_gain} of {total}");
        println!("tracks with peak  {with_peak} of {total}");
        if with_gain > 0 {
            let (min, max, avg): (f64, f64, f64) = conn
                .query_row(
                    "SELECT MIN(rg_track_gain), MAX(rg_track_gain), AVG(rg_track_gain)
                     FROM tracks WHERE rg_track_gain IS NOT NULL",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap_or((0.0, 0.0, 0.0));
            println!("gain range        {min:.2} to {max:.2} dB, mean {avg:.2} dB");
        }
    }

    println!("\n== queries ==");
    let started = Instant::now();
    let all = query::list_tracks(&library)?;
    println!("list_tracks     {} rows in {} ms", all.len(), started.elapsed().as_millis());

    for term in ["yellow", "bro", "remix"] {
        let started = Instant::now();
        let hits = query::search(&library, term, 2000)?;
        println!(
            "search {:<8} {:>5} hits in {:>6.3} ms",
            format!("\"{term}\""),
            hits.len(),
            started.elapsed().as_secs_f64() * 1000.0
        );
    }

    // In WAL mode most of the data sits in the -wal file until a checkpoint, so
    // measuring the .sqlite file alone reports a few kilobytes and a lie.
    library
        .connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    println!("\nindex on disk   {}", human_bytes(file_size(&workdir.join("library.sqlite"))));
    println!("artwork cache   {}", human_bytes(dir_size(&workdir.join("artwork"))));
    println!("workdir         {}", workdir.display());
    Ok(())
}

fn report(report: &index::SyncReport) {
    println!("files seen      {}", report.files_seen);
    println!("added           {}", report.added);
    println!("updated         {}", report.updated);
    println!("moved           {}", report.moved);
    println!("removed         {}", report.removed);
    println!("unchanged       {}", report.unchanged);
    println!("errors          {}", report.errors.len());
    println!("elapsed         {} ms", report.elapsed_ms);
}

fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dubplate-index-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn file_size(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn dir_size(root: &std::path::Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                total += file_size(&path);
            }
        }
    }
    total
}

fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let units = ["KB", "MB", "GB"];
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", units[unit])
}

fn expand_home(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var("HOME")
            .map(|home| format!("{home}/{rest}"))
            .unwrap_or_else(|_| path.to_string()),
        None => path.to_string(),
    }
}
