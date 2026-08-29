use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use image::imageops::FilterType;
use lofty::config::ParseOptions;
use lofty::picture::PictureType;
use lofty::prelude::*;
use lofty::probe::Probe;
use rayon::prelude::*;
use rusqlite::params;
use serde::Serialize;

use crate::db::Library;
use crate::track::ScanError;

/// Widths written for every cover: list thumbnail, grid tile, now-playing.
pub const VARIANTS: [u32; 3] = [64, 300, 1000];

/// Lossy WebP. Lossless would be faithful and roughly ten times the size, which
/// is the wrong trade for a cache that exists to be thrown away and rebuilt.
const QUALITY: f32 = 82.0;

/// Sibling filenames checked when a file carries no embedded art.
const COVER_STEMS: &[&str] = &["cover", "folder", "front", "album", "artwork"];
const COVER_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "gif", "tif", "tiff"];

/// Written into `albums.art_hash` for an album we looked at and found no art
/// for, so the next sync does not open its files again. `refresh_missing`
/// clears these when the user has added covers.
pub const NO_ART: &str = "";

/// On-disk cache of pre-resized covers, keyed by a hash of the source image.
///
/// Nothing here is ever decoded during scroll: the UI loads a file that is
/// already the right size. Identical covers across an album -- or across a
/// whole discography -- collapse to one set of files, because the key is the
/// image content rather than the album.
pub struct ArtworkCache {
    root: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtworkReport {
    pub albums_checked: usize,
    pub art_found: usize,
    pub art_missing: usize,
    pub errors: Vec<ScanError>,
    pub elapsed_ms: u64,
}

impl ArtworkCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Where a given variant lives. Sharded by the first two hex characters so
    /// no single directory holds thousands of entries.
    pub fn variant_path(&self, hash: &str, width: u32) -> PathBuf {
        self.root
            .join(&hash[..2.min(hash.len())])
            .join(format!("{hash}-{width}.webp"))
    }

    pub fn is_complete(&self, hash: &str) -> bool {
        VARIANTS
            .iter()
            .all(|width| self.variant_path(hash, *width).exists())
    }
}

/// Fill in `albums.art_hash` for every album that has not been looked at yet.
///
/// Cover extraction is a separate pass from the tag scan on purpose: the scan
/// reads tags with cover art switched off, because decoding every embedded
/// image while walking a library costs far more than it saves.
pub fn build_cache(library: &mut Library, cache: &ArtworkCache) -> Result<ArtworkReport> {
    let started = std::time::Instant::now();

    let pending: Vec<(i64, String)> = {
        let conn = library.connection();
        let mut stmt = conn.prepare(
            "SELECT al.id, MIN(t.path)
             FROM albums al
             JOIN tracks t ON t.album_id = al.id
             WHERE al.art_hash IS NULL
             GROUP BY al.id",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let results: Vec<(i64, Result<Option<String>, ScanError>)> = pending
        .par_iter()
        .map(|(album_id, path)| {
            let outcome = ingest(cache, Path::new(path)).map_err(|err| ScanError {
                path: path.clone(),
                message: err.to_string(),
            });
            (*album_id, outcome)
        })
        .collect();

    let mut report = ArtworkReport {
        albums_checked: pending.len(),
        ..Default::default()
    };

    let tx = library.connection_mut().transaction()?;
    for (album_id, outcome) in results {
        match outcome {
            Ok(Some(hash)) => {
                tx.execute(
                    "UPDATE albums SET art_hash = ?1 WHERE id = ?2",
                    params![hash, album_id],
                )?;
                report.art_found += 1;
            }
            Ok(None) => {
                tx.execute(
                    "UPDATE albums SET art_hash = ?1 WHERE id = ?2",
                    params![NO_ART, album_id],
                )?;
                report.art_missing += 1;
            }
            Err(err) => {
                // Leave art_hash NULL so a later run tries again.
                report.errors.push(err);
            }
        }
    }
    tx.commit()?;

    report.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// Forget "this album has no art" markers, so covers added since the last run
/// are picked up on the next `build_cache`.
pub fn refresh_missing(library: &Library) -> Result<usize> {
    let cleared = library.connection().execute(
        "UPDATE albums SET art_hash = NULL WHERE art_hash = ?1",
        params![NO_ART],
    )?;
    Ok(cleared)
}

/// Find art for one track's album, write every variant, and return its hash.
pub fn ingest(cache: &ArtworkCache, source: &Path) -> Result<Option<String>> {
    let Some(bytes) = load_art(source)? else {
        return Ok(None);
    };

    let hash = hash_bytes(&bytes);
    // Two albums sharing a cover share its files. Nothing to redo.
    if cache.is_complete(&hash) {
        return Ok(Some(hash));
    }

    let image = image::load_from_memory(&bytes)
        .with_context(|| format!("decoding cover art from {}", source.display()))?;

    for width in VARIANTS {
        write_variant(cache, &hash, &image, width)?;
    }
    Ok(Some(hash))
}

fn write_variant(
    cache: &ArtworkCache,
    hash: &str,
    image: &image::DynamicImage,
    width: u32,
) -> Result<()> {
    let path = cache.variant_path(hash, width);
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Fit inside a width x width box, preserving aspect. Never upscale: a 300px
    // source blown up to 1000px is a bigger file that looks worse.
    let longest = image.width().max(image.height());
    let resized = if longest <= width {
        image.clone()
    } else {
        image.resize(width, width, FilterType::Lanczos3)
    };

    let rgb = resized.to_rgb8();
    let encoder = webp::Encoder::from_rgb(rgb.as_raw(), rgb.width(), rgb.height());
    let encoded = encoder.encode(QUALITY);

    // Write to a temporary file and rename, so a crash mid-write cannot leave a
    // truncated image that later runs would treat as cached.
    let temp = path.with_extension("webp.tmp");
    std::fs::write(&temp, &*encoded)?;
    std::fs::rename(&temp, &path)?;
    Ok(())
}

/// Embedded art first, then a cover file sitting next to the track.
fn load_art(source: &Path) -> Result<Option<Vec<u8>>> {
    if let Some(bytes) = embedded_art(source)? {
        return Ok(Some(bytes));
    }
    Ok(sibling_art(source))
}

fn embedded_art(source: &Path) -> Result<Option<Vec<u8>>> {
    let tagged = match Probe::open(source)
        .and_then(|probe| Ok(probe.guess_file_type()?))
        .and_then(|probe| probe.options(ParseOptions::new().read_properties(false)).read())
    {
        Ok(tagged) => tagged,
        // A file we cannot parse simply has no art to offer.
        Err(_) => return Ok(None),
    };

    let mut fallback: Option<&lofty::picture::Picture> = None;
    for tag in tagged.tags() {
        for picture in tag.pictures() {
            if picture.pic_type() == PictureType::CoverFront {
                return Ok(Some(picture.data().to_vec()));
            }
            fallback.get_or_insert(picture);
        }
    }
    Ok(fallback.map(|picture| picture.data().to_vec()))
}

fn sibling_art(source: &Path) -> Option<Vec<u8>> {
    let dir = source.parent()?;
    let entries = std::fs::read_dir(dir).ok()?;

    let mut best: Option<(usize, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let stem = path.file_stem()?.to_string_lossy().to_ascii_lowercase();
        let ext = match path.extension() {
            Some(ext) => ext.to_string_lossy().to_ascii_lowercase(),
            None => continue,
        };
        if !COVER_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        // Prefer "cover" over "folder" over "front", in the listed order.
        if let Some(rank) = COVER_STEMS.iter().position(|name| stem == *name) {
            if best.as_ref().map_or(true, |(current, _)| rank < *current) {
                best = Some((rank, path));
            }
        }
    }

    std::fs::read(best?.1).ok()
}

fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex()[..32].to_string()
}
