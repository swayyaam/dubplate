//! Scan a folder from the terminal, without launching the app.
//!
//!     cargo run --release -p dubplate-library --example scan -- ~/Music
//!
//! Pass --json to dump the whole report instead of the summary.

use std::path::PathBuf;

use dubplate_library::{scan_folder, Lossiness};

fn main() {
    let Some(arg) = std::env::args().nth(1) else {
        eprintln!("usage: scan <folder>");
        std::process::exit(2);
    };

    let root = PathBuf::from(shellexpand(&arg));
    let report = scan_folder(&root);

    if std::env::args().any(|a| a == "--json") {
        println!("{}", serde_json::to_string(&report).unwrap());
        return;
    }

    let mut lossless = 0;
    let mut lossy = 0;
    let mut unknown = 0;
    let mut total_ms = 0u64;
    let mut bytes = 0u64;
    for track in &report.tracks {
        match track.lossiness {
            Lossiness::Lossless => lossless += 1,
            Lossiness::Lossy => lossy += 1,
            Lossiness::Unknown => unknown += 1,
        }
        total_ms += track.duration_ms;
        bytes += track.size;
    }

    println!("root         {}", report.root);
    println!("files seen   {}", report.files_seen);
    println!("tracks       {}", report.tracks.len());
    println!("errors       {}", report.errors.len());
    println!("elapsed      {} ms", report.elapsed_ms);
    println!();
    println!("lossless     {lossless}");
    println!("lossy        {lossy}");
    println!("unknown      {unknown}");
    println!("runtime      {:.1} hours", total_ms as f64 / 3_600_000.0);
    println!("on disk      {:.1} GB", bytes as f64 / 1_073_741_824.0);

    for error in report.errors.iter().take(10) {
        println!("\n  ! {}\n    {}", error.path, error.message);
    }
}

fn shellexpand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME") {
            Ok(home) => format!("{home}/{rest}"),
            Err(_) => path.to_string(),
        },
        None => path.to_string(),
    }
}
