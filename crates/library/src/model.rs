use serde::Serialize;

use crate::track::Lossiness;

/// A track as the UI sees it, with artist and album names already joined.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackRow {
    pub id: i64,
    pub path: String,
    pub file_name: String,
    pub size: u64,

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
    /// Always null for lossy codecs -- they have no bit depth.
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
    pub bitrate: Option<u32>,

    /// Key into the artwork cache, or null if this album has no art yet.
    /// ReplayGain in dB, from the file's tags or the analysis pass.
    pub replay_gain_db: Option<f32>,
    pub replay_gain_peak: Option<f32>,

    /// Bits the audio actually uses. Below the declared depth means the file is
    /// a smaller recording padded into a bigger container.
    pub effective_bits: Option<u32>,
    /// Highest frequency carrying real energy, in Hz.
    pub spectral_cutoff: Option<u32>,
    /// 0 to 1. A suspicion that a lossless container holds lossy audio, never a
    /// verdict, and never acted on automatically.
    pub transcode_score: Option<f32>,
    pub bpm: Option<f32>,
    /// Camelot notation, e.g. "8A".
    pub music_key: Option<String>,
    pub analyzed_at: Option<i64>,

    pub art_hash: Option<String>,
    pub play_count: i64,
    pub loved: bool,
    pub added_at: i64,
    pub last_played: Option<i64>,
}

/// Every column `TrackRow::from_row` expects, in order. Shared by the list and
/// search queries so the two cannot drift apart.
pub(crate) const TRACK_COLUMNS: &str = "
    t.id, t.path, t.size, t.title, ar.name, al.title, aa.name,
    t.track_no, t.disc_no, t.year, t.genre,
    t.duration_ms, t.codec, t.is_lossy, t.sample_rate, t.bit_depth, t.channels, t.bitrate,
    t.rg_track_gain, t.rg_track_peak,
    t.effective_bits, t.spectral_cutoff, t.transcode_score, t.bpm, t.music_key, t.analyzed_at,
    al.art_hash, t.play_count, t.loved, t.added_at, t.last_played
";

pub(crate) const TRACK_JOINS: &str = "
    FROM tracks t
    LEFT JOIN artists ar ON ar.id = t.artist_id
    LEFT JOIN albums  al ON al.id = t.album_id
    LEFT JOIN artists aa ON aa.id = al.album_artist_id
";

impl TrackRow {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let path: String = row.get(1)?;
        let file_name = std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let is_lossy: Option<i64> = row.get(13)?;

        Ok(Self {
            id: row.get(0)?,
            path,
            file_name,
            size: row.get::<_, i64>(2)? as u64,
            title: row.get(3)?,
            artist: row.get(4)?,
            album: row.get(5)?,
            album_artist: row.get(6)?,
            track_no: row.get(7)?,
            disc_no: row.get(8)?,
            year: row.get(9)?,
            genre: row.get(10)?,
            duration_ms: row.get::<_, i64>(11)? as u64,
            codec: row.get::<_, Option<String>>(12)?.unwrap_or_default(),
            lossiness: match is_lossy {
                Some(0) => Lossiness::Lossless,
                Some(_) => Lossiness::Lossy,
                None => Lossiness::Unknown,
            },
            sample_rate: row.get(14)?,
            bit_depth: row.get(15)?,
            channels: row.get(16)?,
            bitrate: row.get(17)?,
            replay_gain_db: row.get(18)?,
            replay_gain_peak: row.get(19)?,
            effective_bits: row.get(20)?,
            spectral_cutoff: row.get(21)?,
            transcode_score: row.get(22)?,
            bpm: row.get(23)?,
            music_key: row.get(24)?,
            analyzed_at: row.get(25)?,
            art_hash: row.get(26)?,
            play_count: row.get(27)?,
            loved: row.get::<_, i64>(28)? != 0,
            added_at: row.get(29)?,
            last_played: row.get(30)?,
        })
    }
}

/// An album as the art grid sees it: enough to draw a tile without a second
/// query per cover.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumRow {
    pub id: i64,
    pub title: String,
    pub artist: Option<String>,
    pub year: Option<u32>,
    /// Key into the artwork cache. Empty string means "checked, no art found".
    pub art_hash: Option<String>,
    pub track_count: i64,
    pub duration_ms: i64,
    /// One badge for the whole album when every track agrees, e.g. "FLAC 24/96".
    /// `None` when the album is a mix of formats, which is worth seeing.
    pub codec: Option<String>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u8>,
    pub lossless: bool,
}

impl AlbumRow {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let art_hash: Option<String> = row.get(4)?;
        Ok(Self {
            id: row.get(0)?,
            title: row.get(1)?,
            artist: row.get(2)?,
            year: row.get(3)?,
            // The empty-string sentinel means "looked, found nothing"; the UI
            // only cares whether there is a cover to draw.
            art_hash: art_hash.filter(|hash| !hash.is_empty()),
            track_count: row.get(5)?,
            duration_ms: row.get(6)?,
            codec: row.get(7)?,
            sample_rate: row.get(8)?,
            bit_depth: row.get(9)?,
            lossless: row.get::<_, Option<i64>>(10)?.unwrap_or(1) == 0,
        })
    }
}
