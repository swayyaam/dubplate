use std::ffi::OsStr;
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

/// Directory extensions that mark a macOS package rather than a folder of music.
///
/// These are opaque application bundles. Logic's sample library alone holds
/// thousands of single-note WAVs, and a DAW project package holds stems and
/// takes -- none of it is a track anyone wants in a player. Descending into
/// them is how a 1,300 track library turns into a 2,800 track one.
pub const PACKAGE_EXTENSIONS: &[&str] = &[
    "bundle", "app", "framework", "component", "plugin", "vst", "vst3", "audiounit",
    "logicx", "band", "ptx", "photoslibrary", "musiclibrary", "tvlibrary", "imovielibrary",
    "aplibrary", "fcpbundle",
];

fn is_package_dir(name: &OsStr) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let lower = e.to_ascii_lowercase();
            PACKAGE_EXTENSIONS.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

fn has_audio_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let lower = e.to_ascii_lowercase();
            AUDIO_EXTENSIONS.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

/// One audio file as the walk found it, before any tag has been read.
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
    /// Unix seconds. With `size`, this is the incremental-scan skip key.
    pub mtime: i64,
}

/// Walk `root` and return every audio file, without opening any of them.
///
/// Split out from `scan_folder` because an incremental sync needs the cheap
/// half: it compares (path, mtime, size) against the index and only reads tags
/// for files that actually changed.
pub fn walk(root: &Path) -> Vec<FileEntry> {
    WalkDir::new(root)
        .skip_hidden(true)
        // Prune package directories before descending, so their contents are
        // never stat'd at all.
        .process_read_dir(|_depth, _path, _state, children| {
            children.retain(|child| match child {
                Ok(entry) => !(entry.file_type().is_dir() && is_package_dir(&entry.file_name())),
                Err(_) => true,
            });
        })
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.path())
        .filter(|path| has_audio_extension(path))
        .map(|path| {
            let (size, mtime) = std::fs::metadata(&path)
                .map(|meta| (meta.len(), mtime_of(&meta)))
                .unwrap_or((0, 0));
            FileEntry { path, size, mtime }
        })
        .collect()
}

/// A cheap identity for a file's contents: size plus a hash of the first 64KB.
///
/// Enough to recognise the same audio at a new path so a rename or a
/// reorganised folder keeps its play count, without hashing gigabytes. Two
/// different files sharing a size *and* a 64KB prefix would collide, which in
/// practice means re-tagged copies of the same audio -- acceptable, since the
/// key is only ever used to match a vanished path against a new one.
/// Modification time in whole Unix seconds.
///
/// Paired with the file size this is the incremental-scan skip key, so anything
/// that rewrites a file has to compute it exactly this way or the next scan
/// will decide the file changed underneath it.
pub fn mtime_of(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn content_key(path: &Path, size: u64) -> std::io::Result<String> {
    use std::io::Read;

    let mut head = Vec::with_capacity(64 * 1024);
    std::fs::File::open(path)?
        .take(64 * 1024)
        .read_to_end(&mut head)?;

    let digest = blake3::hash(&head);
    Ok(format!("{size:x}-{}", &digest.to_hex()[..32]))
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
    let candidates = walk(root);

    let files_seen = candidates.len();

    // Phase two: read tags in parallel. This is the expensive half.
    let results: Vec<Result<ScannedTrack, ScanError>> = candidates
        .into_par_iter()
        .map(|entry| read_track(&entry.path, entry.size, entry.mtime))
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

pub(crate) fn read_track(path: &Path, size: u64, mtime: i64) -> Result<ScannedTrack, ScanError> {
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
        // Metadata by definition, so tags are the right source here -- unlike
        // format, where they are not to be trusted.
        replay_gain_db: tag.and_then(|t| {
            decibels(t.get_string(ItemKey::ReplayGainTrackGain))
                .or_else(|| decibels(t.get_string(ItemKey::ReplayGainAlbumGain)))
        }),
        replay_gain_peak: tag.and_then(|t| {
            number(t.get_string(ItemKey::ReplayGainTrackPeak))
                .or_else(|| number(t.get_string(ItemKey::ReplayGainAlbumPeak)))
        }),
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

/// "-7.50 dB", "-7.5", "+2.3 dB" -- writers disagree about the suffix and the
/// sign, so parse leniently rather than dropping a perfectly good value.
fn decibels(value: Option<&str>) -> Option<f32> {
    let text = value?.trim();
    let text = text
        .strip_suffix("dB")
        .or_else(|| text.strip_suffix("db"))
        .or_else(|| text.strip_suffix("DB"))
        .unwrap_or(text)
        .trim();
    let parsed: f32 = text.trim_start_matches('+').parse().ok()?;
    // A gain outside this range is a broken tag, not a quiet record.
    (-60.0..=30.0).contains(&parsed).then_some(parsed)
}

fn number(value: Option<&str>) -> Option<f32> {
    let parsed: f32 = value?.trim().parse().ok()?;
    (parsed.is_finite() && parsed > 0.0).then_some(parsed)
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
