use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use jwalk::WalkDir;
use lofty::config::{ParseOptions, ParsingMode};
use lofty::file::{FileType, TaggedFile};
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::Tag;
use rayon::prelude::*;

use crate::track::{Lossiness, ScanError, ScanReport, ScannedTrack};

/// Extensions worth opening. Anything else is skipped without a syscall.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "wav", "wave", "aiff", "aif", "aifc", "mp3", "m4a", "m4b", "mp4", "aac", "ogg", "oga",
    "opus", "wv", "ape", "mpc",
];

fn has_audio_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let lower = e.to_ascii_lowercase();
            AUDIO_EXTENSIONS.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

/// Walk `root` in parallel and read tags from every audio file found.
///
/// The walk and the tag reads are both parallel because a 10,000 file scan is
/// meant to finish in single-digit seconds. Unreadable files land in
/// `ScanReport::errors` instead of stopping the scan.
pub fn scan_folder(root: &Path) -> ScanReport {
    let started = Instant::now();

    // Phase one: walk. jwalk parallelises the directory traversal; we take size
    // and mtime from the walk's own stat so the tag pass does not stat again.
    let candidates: Vec<(PathBuf, u64, i64)> = WalkDir::new(root)
        .skip_hidden(true)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.path())
        .filter(|path| has_audio_extension(path))
        .map(|path| {
            let (size, mtime) = std::fs::metadata(&path)
                .map(|meta| {
                    let mtime = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    (meta.len(), mtime)
                })
                .unwrap_or((0, 0));
            (path, size, mtime)
        })
        .collect();

    let files_seen = candidates.len();

    // Phase two: read tags in parallel. This is the expensive half.
    let results: Vec<Result<ScannedTrack, ScanError>> = candidates
        .into_par_iter()
        .map(|(path, size, mtime)| read_track(&path, size, mtime))
        .collect();

    let mut tracks = Vec::with_capacity(results.len());
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok(track) => tracks.push(track),
            Err(err) => errors.push(err),
        }
    }

    sort_tracks(&mut tracks);

    ScanReport {
        root: root.to_string_lossy().into_owned(),
        tracks,
        errors,
        files_seen,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

/// Album order, the way a tracklist is meant to read: artist, album, disc,
/// track. Untagged files sort last within their group rather than first.
fn sort_tracks(tracks: &mut [ScannedTrack]) {
    tracks.sort_by(|a, b| {
        let artist = |t: &ScannedTrack| {
            t.album_artist
                .clone()
                .or_else(|| t.artist.clone())
                .unwrap_or_default()
                .to_lowercase()
        };
        artist(a)
            .cmp(&artist(b))
            .then_with(|| {
                a.album
                    .clone()
                    .unwrap_or_default()
                    .to_lowercase()
                    .cmp(&b.album.clone().unwrap_or_default().to_lowercase())
            })
            .then_with(|| a.disc_no.unwrap_or(u32::MAX).cmp(&b.disc_no.unwrap_or(u32::MAX)))
            .then_with(|| a.track_no.unwrap_or(u32::MAX).cmp(&b.track_no.unwrap_or(u32::MAX)))
            .then_with(|| a.display_title().cmp(b.display_title()))
    });
}

fn read_track(path: &Path, size: u64, mtime: i64) -> Result<ScannedTrack, ScanError> {
    let tagged = probe(path).map_err(|message| ScanError {
        path: path.to_string_lossy().into_owned(),
        message,
    })?;

    let properties = tagged.properties();
    let file_type = tagged.file_type();
    let (codec, lossiness) = describe_codec(&file_type);

    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    // Lossy codecs have no bit depth. Whatever lofty reports here, drop it.
    let bit_depth = match lossiness {
        Lossiness::Lossy => None,
        _ => properties.bit_depth().filter(|d| *d > 0),
    };

    Ok(ScannedTrack {
        path: path.to_string_lossy().into_owned(),
        file_name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        size,
        mtime,

        title: tag.and_then(|t| t.title().map(|s| s.into_owned())),
        artist: tag.and_then(|t| t.artist().map(|s| s.into_owned())),
        album: tag.and_then(|t| t.album().map(|s| s.into_owned())),
        album_artist: tag.and_then(album_artist),
        track_no: tag.and_then(Accessor::track),
        disc_no: tag.and_then(Accessor::disk),
        year: tag.and_then(year),
        genre: tag.and_then(|t| t.genre().map(|s| s.into_owned())),

        duration_ms: properties.duration().as_millis() as u64,
        codec: codec.to_string(),
        lossiness,
        // Relaxed parsing zeroes fields it cannot recover. Zero is not a real
        // sample rate, so report it as absent rather than showing "0 Hz".
        sample_rate: properties.sample_rate().filter(|r| *r > 0),
        bit_depth,
        channels: properties.channels().filter(|c| *c > 0),
        bitrate: properties
            .audio_bitrate()
            .or_else(|| properties.overall_bitrate())
            .filter(|b| *b > 0),
    })
}

/// Read a file, retrying in relaxed mode before giving up.
///
/// A malformed ID3 frame or metadata block must never hide a perfectly good
/// audio file: the filesystem is the source of truth, so anything decodable
/// belongs in the library even if its tags are a mess. `BestAttempt` runs first
/// because it recovers more fields; `Relaxed` discards the bad parts and keeps
/// going. Only a file that fails both is reported as unreadable.
fn probe(path: &Path) -> Result<TaggedFile, String> {
    let attempt = |mode: ParsingMode| -> Result<TaggedFile, String> {
        let options = ParseOptions::new()
            .parsing_mode(mode)
            // Artwork is extracted in its own pass when the cache is built.
            // Decoding every cover here would balloon a full scan for nothing.
            .read_cover_art(false);

        Probe::open(path)
            .map_err(|e| e.to_string())?
            // Sniff magic bytes rather than trusting the extension. Mislabelled
            // files are common in a collection assembled from many sources.
            .guess_file_type()
            .map_err(|e| e.to_string())?
            .options(options)
            .read()
            .map_err(|e| e.to_string())
    };

    attempt(ParsingMode::BestAttempt).or_else(|first| attempt(ParsingMode::Relaxed).map_err(|_| first))
}

fn album_artist(tag: &Tag) -> Option<String> {
    tag.get_string(ItemKey::AlbumArtist)
        .map(str::to_owned)
        .filter(|s| !s.trim().is_empty())
}

/// Prefer the structured date, fall back to a bare year frame.
fn year(tag: &Tag) -> Option<u32> {
    if let Some(date) = tag.date() {
        return Some(u32::from(date.year));
    }
    tag.get_string(ItemKey::Year)
        .or_else(|| tag.get_string(ItemKey::RecordingDate))
        .and_then(|s| s.get(..4).unwrap_or(s).trim().parse().ok())
}

fn describe_codec(file_type: &FileType) -> (&str, Lossiness) {
    match file_type {
        FileType::Flac => ("flac", Lossiness::Lossless),
        FileType::Wav => ("wav", Lossiness::Lossless),
        FileType::Aiff => ("aiff", Lossiness::Lossless),
        FileType::Ape => ("ape", Lossiness::Lossless),
        FileType::WavPack => ("wavpack", Lossiness::Lossless),
        FileType::Mpeg => ("mp3", Lossiness::Lossy),
        FileType::Aac => ("aac", Lossiness::Lossy),
        FileType::Vorbis => ("vorbis", Lossiness::Lossy),
        FileType::Opus => ("opus", Lossiness::Lossy),
        FileType::Speex => ("speex", Lossiness::Lossy),
        FileType::Mpc => ("musepack", Lossiness::Lossy),
        // AAC or ALAC, and a tag read does not say which. See `Lossiness`.
        FileType::Mp4 => ("m4a", Lossiness::Unknown),
        FileType::Custom(name) => (name, Lossiness::Unknown),
        _ => ("unknown", Lossiness::Unknown),
    }
}
