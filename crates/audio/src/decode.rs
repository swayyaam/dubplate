//! Turning files into interleaved f32, via Symphonia.
//!
//! Everything reported here comes from the container and frame headers, never
//! from tags. In a library assembled from many sources tags are wrong often
//! enough to be useless for format questions, and the signal path panel in
//! phase 5 is built on [`SourceFormat`] for exactly that reason.

use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::units::{TimeBase, Timestamp};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("cannot open {path}: {source}")]
    Open {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("no playable audio track in {0}")]
    NoAudioTrack(String),
    #[error("unsupported or damaged audio: {0}")]
    Codec(#[from] SymphoniaError),
}

/// What the file actually is, read out of the stream.
#[derive(Debug, Clone)]
pub struct SourceFormat {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// `None` for lossy codecs. MP3, AAC and Vorbis store frequency
    /// coefficients rather than samples, so they have no bit depth at all --
    /// they decode straight to 32-bit float. Reporting "16 bit" for an MP3 is
    /// meaningless, and plenty of players do it anyway.
    pub bits_per_sample: Option<u32>,
    /// s16, s24, s32, f32 ... whatever the container declares.
    pub sample_format: Option<String>,
    pub total_frames: Option<u64>,
}

impl SourceFormat {
    pub fn duration(&self) -> Option<std::time::Duration> {
        let frames = self.total_frames?;
        Some(std::time::Duration::from_secs_f64(
            frames as f64 / self.sample_rate.max(1) as f64,
        ))
    }
}

/// One open file, decoded a packet at a time.
///
/// Deliberately pull-based: the decode thread asks for the next block when the
/// ring has room, rather than being handed a callback that might block.
pub struct TrackDecoder {
    /// Kept so the reader can be rebuilt. See `seek`.
    path: PathBuf,
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    /// Needed to turn a frame index into the timestamp the container seeks by.
    /// Most audio uses 1/sample_rate, but MP4 does not have to.
    time_base: Option<TimeBase>,
    format: SourceFormat,
    /// Reused across packets so decoding does not allocate per block.
    interleaved: Vec<f32>,
    /// Frames handed out so far, which is where playback is in the file.
    position_frames: u64,
    exhausted: bool,
}

impl TrackDecoder {
    pub fn open(path: &Path) -> Result<Self, DecodeError> {
        let parts = Self::build(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            reader: parts.reader,
            decoder: parts.decoder,
            track_id: parts.track_id,
            time_base: parts.time_base,
            format: parts.format,
            interleaved: Vec::new(),
            position_frames: 0,
            exhausted: false,
        })
    }

    fn build(path: &Path) -> Result<DecoderParts, DecodeError> {
        let file = File::open(path).map_err(|source| DecodeError::Open {
            path: path.display().to_string(),
            source,
        })?;

        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(extension);
        }

        let stream = MediaSourceStream::new(Box::new(file), Default::default());
        let reader = symphonia::default::get_probe().probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )?;

        let track = reader
            .default_track(TrackType::Audio)
            .ok_or_else(|| DecodeError::NoAudioTrack(path.display().to_string()))?;
        let track_id = track.id;
        let total_frames = track.num_frames;
        let time_base = track.time_base;

        let Some(CodecParameters::Audio(params)) = track.codec_params.clone() else {
            return Err(DecodeError::NoAudioTrack(path.display().to_string()));
        };

        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())?;

        let format = SourceFormat {
            codec: decoder.codec_info().short_name.to_string(),
            sample_rate: params.sample_rate.unwrap_or(44_100),
            channels: params.channels.as_ref().map(|c| c.count() as u16).unwrap_or(2),
            bits_per_sample: params.bits_per_sample,
            sample_format: params.sample_format.map(|f| format!("{f:?}").to_lowercase()),
            total_frames,
        };

        Ok(DecoderParts {
            reader,
            decoder,
            track_id,
            time_base,
            format,
        })
    }

    pub fn format(&self) -> &SourceFormat {
        &self.format
    }

    pub fn position_frames(&self) -> u64 {
        self.position_frames
    }

    /// Decode the next block of interleaved f32, or `None` at end of track.
    ///
    /// Damaged packets are skipped rather than ending playback: one bad frame
    /// in the middle of a file should be a glitch, not a stop.
    pub fn next_block(&mut self) -> Result<Option<&[f32]>, DecodeError> {
        if self.exhausted {
            return Ok(None);
        }

        loop {
            let packet = match self.reader.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => {
                    self.exhausted = true;
                    return Ok(None);
                }
                Err(SymphoniaError::IoError(err))
                    if err.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    self.exhausted = true;
                    return Ok(None);
                }
                Err(err) => return Err(err.into()),
            };

            if packet.track_id != self.track_id {
                continue;
            }

            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let frames = decoded.frames();
                    if frames == 0 {
                        continue;
                    }
                    decoded.copy_to_vec_interleaved(&mut self.interleaved);
                    self.position_frames += frames as u64;
                    return Ok(Some(&self.interleaved));
                }
                // A corrupt packet is recoverable; the next one usually decodes.
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(err) => return Err(err.into()),
            }
        }
    }

    fn frames_to_timestamp(&self, frames: u64) -> Timestamp {
        match self.time_base {
            Some(base) => {
                let seconds = frames as f64 / self.format.sample_rate.max(1) as f64;
                let ticks = seconds * base.denom.get() as f64 / base.numer.get() as f64;
                Timestamp::new(ticks.round() as i64)
            }
            None => Timestamp::new(frames as i64),
        }
    }

    fn timestamp_to_frames(&self, ts: Timestamp) -> u64 {
        let ticks = ts.get().max(0) as f64;
        match self.time_base {
            Some(base) => {
                let seconds = ticks * base.numer.get() as f64 / base.denom.get() as f64;
                (seconds * self.format.sample_rate as f64).round() as u64
            }
            None => ticks as u64,
        }
    }

    /// Seek to `frames` into the track. Returns where the reader actually
    /// landed, which is not always what was asked for.
    pub fn seek(&mut self, frames: u64) -> Result<u64, DecodeError> {
        // An MP4 reader that has run to the end of the stream cannot be seeked
        // back: it fails with "no atom pending read". Rebuilding is cheap next
        // to the alternative, which is a player that breaks when you drag the
        // seek bar back after a track finishes.
        if self.exhausted {
            let parts = Self::build(&self.path)?;
            self.reader = parts.reader;
            self.decoder = parts.decoder;
            self.track_id = parts.track_id;
            self.time_base = parts.time_base;
        }

        let seeked = self.reader.seek(
            SeekMode::Accurate,
            SeekTo::Timestamp {
                ts: self.frames_to_timestamp(frames),
                track_id: self.track_id,
            },
        )?;

        // The decoder holds state from before the seek, and that state now
        // describes a different part of the file.
        self.decoder.reset();
        self.exhausted = false;
        self.position_frames = self.timestamp_to_frames(seeked.actual_ts);
        Ok(self.position_frames)
    }
}

/// The pieces of an open file, so `open` and the rebuild in `seek` share code.
struct DecoderParts {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    format: SourceFormat,
}
