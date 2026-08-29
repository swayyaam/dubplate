//! Print the accent colour derived from real covers, as ANSI swatches.
//!
//!     cargo run --release -p dubplate-library --example accents -- <artwork-cache-dir>

use dubplate_library::artwork::{self, ArtworkCache};

fn main() {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: accents <artwork-cache-dir>");
        std::process::exit(2);
    };
    let cache = ArtworkCache::new(&dir);

    let mut hashes: Vec<String> = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(&dir)];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(path) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if let Some((hash, rest)) = name.split_once('-') {
                    if rest == "64.webp" {
                        hashes.push(hash.to_string());
                    }
                }
            }
        }
    }
    hashes.sort();
    hashes.dedup();

    let mut greyscale = 0;
    for hash in hashes.iter().take(24) {
        match artwork::accent(&cache, hash) {
            Some(hex) => {
                let value = u32::from_str_radix(&hex[1..], 16).unwrap_or(0);
                let (r, g, b) = (value >> 16, (value >> 8) & 0xff, value & 0xff);
                println!("  \x1b[48;2;{r};{g};{b}m      \x1b[0m  {hex}  {hash}");
            }
            None => {
                greyscale += 1;
                println!("  (no accent — greyscale)      {hash}");
            }
        }
    }
    println!(
        "\n{} covers, {} of the first 24 offered no accent",
        hashes.len(),
        greyscale
    );
}
