mod common;

use common::write_wav_seeded;
use dubplate_library::artwork::{self, ArtworkCache, VARIANTS};
use dubplate_library::{index, Library};

/// A solid-colour PNG, which is all the cache pipeline needs to exercise.
fn write_cover(path: &std::path::Path, size: u32, shade: u8) {
    let mut buffer = image::RgbImage::new(size, size);
    for (x, y, pixel) in buffer.enumerate_pixels_mut() {
        *pixel = image::Rgb([shade, (x % 256) as u8, (y % 256) as u8]);
    }
    buffer.save(path).unwrap();
}

#[test]
fn writes_every_variant_and_never_upscales() {
    let dir = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    write_wav_seeded(&dir.path().join("01.wav"), 44_100, 16, 2, 44_100, 1);
    write_cover(&dir.path().join("cover.png"), 600, 90);

    let cache = ArtworkCache::new(cache_dir.path());
    let hash = artwork::ingest(&cache, &dir.path().join("01.wav"))
        .unwrap()
        .expect("a sibling cover.png must be found");

    for width in VARIANTS {
        let path = cache.variant_path(&hash, width);
        assert!(path.exists(), "missing {width}px variant");

        let decoded = image::open(&path).unwrap();
        // The 1000px variant of a 600px source stays 600px: upscaling would
        // make a bigger file that looks worse.
        let expected = width.min(600);
        assert_eq!(decoded.width(), expected, "{width}px variant width");
    }
    assert!(cache.is_complete(&hash));
}

#[test]
fn identical_covers_collapse_to_one_set_of_files() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();

    for dir in [first.path(), second.path()] {
        write_wav_seeded(&dir.join("01.wav"), 44_100, 16, 2, 44_100, 1);
        write_cover(&dir.join("cover.png"), 300, 40);
    }

    let cache = ArtworkCache::new(cache_dir.path());
    let a = artwork::ingest(&cache, &first.path().join("01.wav")).unwrap().unwrap();
    let b = artwork::ingest(&cache, &second.path().join("01.wav")).unwrap().unwrap();

    assert_eq!(a, b, "same image bytes must produce the same key");
    let files = walkdir_count(cache_dir.path());
    assert_eq!(files, VARIANTS.len(), "one set of variants, not two");
}

#[test]
fn an_album_with_no_art_is_marked_so_it_is_not_rechecked() {
    let dir = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let track = dir.path().join("01.wav");
    write_wav_seeded(&track, 44_100, 16, 2, 44_100, 1);
    common::tag(&track, "Track", "Artist", "Album");

    let mut library = Library::open_in_memory().unwrap();
    index::sync(&mut library, dir.path()).unwrap();

    let cache = ArtworkCache::new(cache_dir.path());
    let report = artwork::build_cache(&mut library, &cache).unwrap();
    assert_eq!(report.albums_checked, 1);
    assert_eq!(report.art_missing, 1);
    assert_eq!(report.art_found, 0);

    // Second run has nothing to do, because the album is marked as checked.
    let again = artwork::build_cache(&mut library, &cache).unwrap();
    assert_eq!(again.albums_checked, 0);

    // Until the user adds a cover and asks for a re-check.
    assert_eq!(artwork::refresh_missing(&library).unwrap(), 1);
    write_cover(&dir.path().join("cover.png"), 200, 10);
    let third = artwork::build_cache(&mut library, &cache).unwrap();
    assert_eq!(third.art_found, 1);
}

#[test]
fn build_cache_records_the_hash_against_the_album() {
    let dir = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let track = dir.path().join("01.wav");
    write_wav_seeded(&track, 44_100, 16, 2, 44_100, 1);
    common::tag(&track, "Track", "Artist", "Album");
    write_cover(&dir.path().join("folder.jpg"), 500, 200);

    let mut library = Library::open_in_memory().unwrap();
    index::sync(&mut library, dir.path()).unwrap();
    artwork::build_cache(&mut library, &ArtworkCache::new(cache_dir.path())).unwrap();

    let rows = dubplate_library::query::list_tracks(&library).unwrap();
    let hash = rows[0].art_hash.as_deref().unwrap();
    assert!(!hash.is_empty(), "art_hash should be a real key");
}

fn walkdir_count(root: &std::path::Path) -> usize {
    let mut count = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                count += 1;
            }
        }
    }
    count
}
