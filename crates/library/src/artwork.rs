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

/// Pull a usable accent colour out of a cover.
///
/// "One accent colour, sampled from the current album art" is the whole visual
/// idea, so the result has to be usable rather than merely accurate: a muddy
/// average of an entire sleeve is grey, and grey is not an accent. Pixels are
/// weighted by how colourful they are, near-black and near-white are ignored
/// entirely, and the winning hue is re-lit to sit legibly on a near-black page.
///
/// Reads the 64px variant, which is already in the cache and is plenty: a
/// dominant hue does not need resolution.
/// How many hue buckets the sleeve is reduced to.
///
/// 24 is fine enough to separate red from orange, coarse enough that noise in
/// a photograph does not split one colour across two buckets.
const HUE_BUCKETS: usize = 24;

/// Weight and mean colour of every hue bucket in a sleeve.
fn hue_buckets(cache: &ArtworkCache, hash: &str) -> Option<([f64; HUE_BUCKETS], [[f64; 3]; HUE_BUCKETS])> {
    if hash.is_empty() {
        return None;
    }
    let image = image::open(cache.variant_path(hash, VARIANTS[0])).ok()?;
    let rgb = image.to_rgb8();

    let mut weights = [0.0f64; HUE_BUCKETS];
    let mut sums = [[0.0f64; 3]; HUE_BUCKETS];

    for pixel in rgb.pixels() {
        let (r, g, b) = (pixel[0] as f64, pixel[1] as f64, pixel[2] as f64);
        let (hue, saturation, lightness) = to_hsl(r, g, b);
        // Ignore the parts of a sleeve that carry no colour: black borders,
        // white text, and the grey in between.
        if saturation < 0.15 || !(0.08..0.94).contains(&lightness) {
            continue;
        }
        // Colourful, mid-lit pixels count for more than pale or dark ones.
        let weight = saturation * (1.0 - (lightness - 0.5).abs() * 1.2);
        let bucket = ((hue / 360.0) * HUE_BUCKETS as f64) as usize % HUE_BUCKETS;
        weights[bucket] += weight;
        sums[bucket][0] += r * weight;
        sums[bucket][1] += g * weight;
        sums[bucket][2] += b * weight;
    }
    Some((weights, sums))
}

fn bucket_hsl(sums: &[[f64; 3]; HUE_BUCKETS], weights: &[f64; HUE_BUCKETS], bucket: usize) -> (f64, f64, f64) {
    let weight = weights[bucket];
    to_hsl(
        sums[bucket][0] / weight,
        sums[bucket][1] / weight,
        sums[bucket][2] / weight,
    )
}

pub fn accent(cache: &ArtworkCache, hash: &str) -> Option<String> {
    let (weights, sums) = hue_buckets(cache, hash)?;

    let (best, weight) = weights
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))?;
    if *weight <= f64::EPSILON {
        // A greyscale sleeve genuinely has no accent to offer.
        return None;
    }

    let (hue, saturation, _) = bucket_hsl(&sums, &weights, best);

    // Re-light it. Whatever the sleeve's own lightness was, the accent has to
    // read against near-black and stay legible as small text.
    let (r, g, b) = from_hsl(hue, saturation.clamp(0.45, 0.85), 0.62);
    Some(format!("#{:02x}{:02x}{:02x}", r, g, b))
}

/// The now playing backdrop: one colour that fills the screen, and a few
/// faint washes that move across it.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Backdrop {
    /// The sleeve's dominant colour, deep enough that pale grey text still
    /// reads against it. Empty when the sleeve has no colour to offer.
    pub base: String,
    /// Slow-moving modulation, drawn faintly over the base.
    pub washes: Vec<String>,
}

/// Colours from a sleeve for the now playing backdrop.
///
/// One dominant colour carries the screen and the rest only modulate it. Three
/// equally weighted hues sweeping around read as a lava lamp: what a record
/// actually looks like from across the room is one colour, moving a little.
///
/// The washes are picked from buckets separated on the hue wheel, because
/// adjacent buckets on a photograph are usually the same colour split in two
/// and three shades of one orange modulate nothing.
pub fn backdrop(cache: &ArtworkCache, hash: &str) -> Backdrop {
    const WASHES: usize = 3;
    /// Minimum separation between chosen buckets, in buckets.
    const APART: usize = 3;

    let Some((weights, sums)) = hue_buckets(cache, hash) else {
        return Backdrop::default();
    };

    let mut order: Vec<usize> = (0..HUE_BUCKETS).filter(|b| weights[*b] > f64::EPSILON).collect();
    order.sort_by(|a, b| weights[*b].partial_cmp(&weights[*a]).unwrap_or(std::cmp::Ordering::Equal));

    let mut chosen: Vec<usize> = Vec::new();
    for bucket in order {
        let clash = chosen.iter().any(|taken| {
            let gap = (bucket as isize - *taken as isize).unsigned_abs();
            gap.min(HUE_BUCKETS - gap) < APART
        });
        if !clash {
            chosen.push(bucket);
        }
        if chosen.len() == WASHES {
            break;
        }
    }
    let Some(&dominant) = chosen.first() else {
        return Backdrop::default();
    };

    // Deep, not bright. This fills most of a screen that also carries small
    // grey text, and a vivid field behind 12px type is unreadable.
    let (dominant_hue, saturation, _) = bucket_hsl(&sums, &weights, dominant);
    let (r, g, b) = from_hsl(dominant_hue, saturation.clamp(0.45, 0.8), 0.19);
    let base = format!("#{:02x}{:02x}{:02x}", r, g, b);

    // Each wash is pulled most of the way back to the dominant hue. Left where
    // the sleeve put them, a secondary colour parks a patch of a different hue
    // on one side of the screen and the field stops reading as one colour --
    // which is the whole idea. A third of the way out is enough to keep the
    // sleeve's character without breaking it into panels.
    const PULL: f64 = 0.34;
    let washes = (0..WASHES)
        .map(|index| {
            let bucket = chosen[index % chosen.len()];
            let (hue, saturation, _) = bucket_hsl(&sums, &weights, bucket);
            // Shortest way round the wheel, so a hue at 350 and one at 10 are
            // twenty degrees apart rather than three hundred and forty.
            let delta = (hue - dominant_hue + 540.0).rem_euclid(360.0) - 180.0;
            // A sleeve with only one real colour modulates with itself.
            let spread = if index < chosen.len() { 0.0 } else { 14.0 * index as f64 };
            let (r, g, b) = from_hsl(
                (dominant_hue + delta * PULL + spread).rem_euclid(360.0),
                saturation.clamp(0.4, 0.75),
                0.30 - 0.03 * index as f64,
            );
            format!("#{:02x}{:02x}{:02x}", r, g, b)
        })
        .collect();

    Backdrop { base, washes }
}

fn to_hsl(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let (r, g, b) = (r / 255.0, g / 255.0, b / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = (max + min) / 2.0;
    let delta = max - min;

    if delta.abs() < f64::EPSILON {
        return (0.0, 0.0, lightness);
    }
    let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs()).max(f64::EPSILON);
    let hue = if (max - r).abs() < f64::EPSILON {
        60.0 * (((g - b) / delta) % 6.0)
    } else if (max - g).abs() < f64::EPSILON {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    ((hue + 360.0) % 360.0, saturation.clamp(0.0, 1.0), lightness)
}

fn from_hsl(hue: f64, saturation: f64, lightness: f64) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let x = c * (1.0 - (((hue / 60.0) % 2.0) - 1.0).abs());
    let m = lightness - c / 2.0;
    let (r, g, b) = match hue as u32 / 60 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}
