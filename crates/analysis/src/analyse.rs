//! One decode pass per track, six answers out of it.
//!
//! Decoding is the expensive part by a wide margin, so everything that needs
//! samples reads from the same pass: loudness, waveform peaks, bit depth,
//! spectrum, onsets and chroma.

use std::path::Path;

use dubplate_audio::decode::{DecodeError, TrackDecoder};

use crate::depth::DepthProbe;
use crate::key::{self, KeyEstimate};
use crate::spectral::SpectrumCollector;
use crate::tempo;
use crate::waveform::Waveform;

/// ReplayGain 2.0 targets -18 LUFS.
const REFERENCE_LUFS: f64 = -18.0;

/// Frames per fine-grained waveform sample before it is collapsed into
/// buckets. 256 at 44.1kHz is ~6ms, comfortably finer than a drum hit.
const PEAK_WINDOW: usize = 256;

/// Everything one pass produces.
#[derive(Debug, Clone, Default)]
pub struct TrackAnalysis {
    /// Integrated loudness, EBU R128.
    pub loudness_lufs: Option<f64>,
    /// The gain that would bring this track to the reference level.
    pub replay_gain_db: Option<f32>,
    /// True peak, 1.0 being full scale. Above 1.0 is legal in float and will
    /// clip a converter, which is exactly why the gain is limited by it.
    pub true_peak: Option<f32>,
    /// Level, body and band mix per column of the seek bar.
    pub waveform: Waveform,
    pub declared_bits: Option<u32>,
    /// What the file actually uses. Below `declared_bits` means padding.
    pub effective_bits: Option<u32>,
    pub spectral_cutoff: Option<u32>,
    pub rolloff_db: f32,
    /// A suspicion between 0 and 1, never a verdict.
    pub transcode_score: f32,
    pub bpm: Option<f32>,
    pub bpm_confidence: f32,
    pub key: Option<KeyEstimate>,
    pub duration_ms: u64,
    pub sample_rate: u32,
    pub codec: String,
}

impl TrackAnalysis {
    /// True when the container claims more bits than the audio uses.
    pub fn is_padded(&self) -> bool {
        match (self.declared_bits, self.effective_bits) {
            (Some(declared), Some(effective)) => effective < declared,
            _ => false,
        }
    }
}

pub fn analyse(path: &Path) -> Result<TrackAnalysis, DecodeError> {
    let mut decoder = TrackDecoder::open(path)?;
    let format = decoder.format().clone();
    let channels = format.channels.max(1) as usize;
    let rate = format.sample_rate.max(1);

    let mut loudness = ebur128::EbuR128::new(
        channels as u32,
        rate,
        ebur128::Mode::I | ebur128::Mode::TRUE_PEAK,
    )
    .ok();
    let mut depth = DepthProbe::new(format.bits_per_sample);
    let mut spectrum = SpectrumCollector::new(rate);

    // Fine series first, collapsed to buckets at the end: the track length is
    // not known reliably in advance for every container.
    let mut fine_peaks: Vec<f32> = Vec::new();
    let mut fine_squares: Vec<f32> = Vec::new();
    let mut window_peak = 0.0f32;
    let mut window_square = 0.0f64;
    let mut window_frames = 0usize;

    let mut mono = Vec::with_capacity(8192);
    let mut frames_total = 0u64;

    while let Some(block) = decoder.next_block()? {
        if let Some(meter) = loudness.as_mut() {
            let _ = meter.add_frames_f32(block);
        }
        if let Some(probe) = depth.as_mut() {
            probe.feed(block);
        }

        mono.clear();
        for frame in block.chunks(channels) {
            let mut peak = 0.0f32;
            let mut sum = 0.0f32;
            let mut square = 0.0f32;
            for sample in frame {
                peak = peak.max(sample.abs());
                sum += *sample;
                // Squared per channel rather than off the mono mix, so a
                // hard-panned or out-of-phase moment keeps its energy instead
                // of cancelling itself into a hole in the waveform.
                square += *sample * *sample;
            }
            mono.push(sum / channels as f32);

            window_peak = window_peak.max(peak);
            window_square += (square / channels as f32) as f64;
            window_frames += 1;
            if window_frames == PEAK_WINDOW {
                fine_peaks.push(window_peak);
                fine_squares.push((window_square / PEAK_WINDOW as f64) as f32);
                window_peak = 0.0;
                window_square = 0.0;
                window_frames = 0;
            }
        }
        frames_total += (block.len() / channels) as u64;
        spectrum.feed(&mono);
    }
    if window_frames > 0 {
        fine_peaks.push(window_peak);
        fine_squares.push((window_square / window_frames as f64) as f32);
    }

    let summary = spectrum.summarise();
    let tempo = tempo::estimate(spectrum.onset_envelope(), spectrum.hops_per_second());

    let lufs = loudness
        .as_ref()
        .and_then(|meter| meter.loudness_global().ok())
        // A silent or near-silent track reports -inf, which is not a level.
        .filter(|value| value.is_finite());
    let true_peak = loudness.as_ref().and_then(|meter| {
        (0..channels as u32)
            .filter_map(|channel| meter.true_peak(channel).ok())
            .fold(None::<f64>, |acc, peak| Some(acc.map_or(peak, |a| a.max(peak))))
    });

    Ok(TrackAnalysis {
        loudness_lufs: lufs,
        replay_gain_db: lufs.map(|value| (REFERENCE_LUFS - value) as f32),
        true_peak: true_peak.map(|value| value as f32),
        waveform: Waveform::build(&fine_peaks, &fine_squares, spectrum.band_trace()),
        declared_bits: depth.as_ref().map(|probe| probe.declared_bits()),
        effective_bits: depth.as_ref().and_then(|probe| probe.effective_bits()),
        spectral_cutoff: summary.cutoff_hz,
        rolloff_db: summary.rolloff_db,
        transcode_score: summary.transcode_score,
        bpm: tempo.map(|estimate| estimate.bpm),
        bpm_confidence: tempo.map(|estimate| estimate.confidence).unwrap_or(0.0),
        key: key::estimate(&summary.chroma),
        duration_ms: frames_total * 1000 / rate as u64,
        sample_rate: rate,
        codec: format.codec.clone(),
    })
}
