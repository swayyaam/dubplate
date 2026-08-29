use serde::Serialize;

/// Whether a codec throws information away.
///
/// `Unknown` is deliberate. An MP4 container holds either AAC (lossy) or ALAC
/// (lossless) and a tag-level read does not reliably tell them apart. Phase 2
/// gets the real answer from Symphonia's `CodecParameters`; until then we say we
/// do not know, because a wrong "lossless" badge is worse than an absent one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lossiness {
    Lossless,
    Lossy,
    Unknown,
}

/// One audio file as the scanner found it.
///
/// This is a *tag-level* view, which is enough for the library table but is NOT
/// the authority for the signal-path panel. The project rule is "read the
/// stream, never the tags" -- when phase 5 renders format information it must
/// come from Symphonia's `CodecParameters`, not from here.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScannedTrack {
    pub path: String,
    pub file_name: String,
    pub size: u64,
    /// Unix seconds. Paired with `size` this is the incremental-scan skip key.
    pub mtime: i64,

    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    pub year: Option<u32>,
    pub genre: Option<String>,

    pub duration_ms: u64,
    pub codec: String,
    pub lossiness: Lossiness,
    pub sample_rate: Option<u32>,
    /// Always `None` for lossy codecs. MP3, AAC and Vorbis store frequency
    /// coefficients rather than samples, so they have no bit depth to report --
    /// they decode straight to 32-bit float. Showing "16 bit" for an MP3 is
    /// meaningless, and plenty of players do it anyway.
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
    pub bitrate: Option<u32>,
}

impl ScannedTrack {
    /// Title if tagged, otherwise the filename without extension. A library
    /// assembled from many sources always has untagged files in it.
    pub fn display_title(&self) -> &str {
        match self.title.as_deref() {
            Some(t) if !t.trim().is_empty() => t,
            _ => self
                .file_name
                .rsplit_once('.')
                .map(|(stem, _)| stem)
                .unwrap_or(&self.file_name),
        }
    }
}

/// A file the scanner saw but could not read.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanError {
    pub path: String,
    pub message: String,
}

/// The result of one full walk. Errors are reported alongside the tracks rather
/// than aborting: one unreadable file should never sink a 10,000 file scan.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub root: String,
    pub tracks: Vec<ScannedTrack>,
    pub errors: Vec<ScanError>,
    /// Audio files encountered, including the ones that failed to parse.
    pub files_seen: usize,
    pub elapsed_ms: u64,
}
