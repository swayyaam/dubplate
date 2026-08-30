//! Print the accent and backdrop palette for album art hashes.
//!
//!     cargo run -p dubplate-library --example palette -- <hash> ...

use dubplate_library::artwork::{accent, palette, ArtworkCache};

fn main() {
    let root = dirs_next_data().join("artwork");
    let cache = ArtworkCache::new(&root);
    for hash in std::env::args().skip(1) {
        println!("── {hash}");
        println!("   accent  {:?}", accent(&cache, &hash));
        println!("   palette {:?}", palette(&cache, &hash));
    }
}

fn dirs_next_data() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home)
        .join("Library/Application Support/com.swayammishra.dubplate")
}
