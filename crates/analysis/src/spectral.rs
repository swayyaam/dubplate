//! One FFT pass, four answers: where the spectrum stops, how abruptly it stops,
//! what pitch classes are present, and where the onsets are.
//!
//! Sharing the pass is the point. Decoding a library six times to compute six
//! things would take six times as long for no benefit.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// 2048 gives ~21 Hz bins, which is ample for finding a cutoff, and a 512
/// hop gives ~12ms between frames, which is fine enough for onsets.
const FFT_SIZE: usize = 2048;
const HOP: usize = 512;

/// How far below the musical reference level a bin still counts as real energy.
const FLOOR_DB: f32 = 60.0;

/// Where the waveform's three colour bands meet, in Hz.
///
/// 200Hz keeps the kick and the bassline together and everything else out;
/// 2kHz puts hats, snare crack and air above it. These are display bands, not
/// crossover points -- the aim is that a breakdown looks different from a drop,
/// not that the split is defensible as filter design.
const BAND_SPLIT_HZ: [f32; 2] = [200.0, 2_000.0];

pub struct SpectrumCollector {
    fft: Arc<dyn Fft<f32>>,
    rate: u32,
    window: Vec<f32>,
    pending: Vec<f32>,
    scratch: Vec<Complex<f32>>,
    magnitude_sum: Vec<f64>,
    windows: u64,
    previous: Vec<f32>,
    flux: Vec<f32>,
    chroma: [f64; 12],
    /// Low/mid/high spectral density per window, in step with `flux`. The
    /// waveform colours its columns from this.
    bands: Vec<[f32; 3]>,
    band_edges: [usize; 2],
    /// Bins in each band. The high band spans a hundred times as many bins as
    /// the low one, so a plain sum would measure bandwidth rather than energy.
    band_bins: [f32; 3],
}

/// What the spectrum says about the file.
#[derive(Debug, Clone)]
pub struct SpectrumSummary {
    /// Highest frequency still carrying real energy, averaged over the track.
    pub cutoff_hz: Option<u32>,
    /// How far the level falls in the 2kHz above the cutoff. A brick wall drops
    /// tens of dB; a natural rolloff drifts down.
    pub rolloff_db: f32,
    /// 0 to 1. A suspicion, never a verdict: quiet recordings, older masters
    /// and genuinely dark mixes all have low high-frequency content without
    /// having been through a lossy encoder.
    pub transcode_score: f32,
    pub chroma: [f32; 12],
}

impl SpectrumCollector {
    pub fn new(rate: u32) -> Self {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let edges = [bin_for(BAND_SPLIT_HZ[0], rate), bin_for(BAND_SPLIT_HZ[1], rate)];
        // Hann, so leakage does not smear a cliff into a slope.
        let window = (0..FFT_SIZE)
            .map(|n| {
                0.5 * (1.0
                    - (std::f32::consts::TAU * n as f32 / FFT_SIZE as f32).cos())
            })
            .collect();
        Self {
            fft,
            rate,
            window,
            pending: Vec::with_capacity(FFT_SIZE * 2),
            scratch: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            magnitude_sum: vec![0.0; FFT_SIZE / 2 + 1],
            windows: 0,
            previous: vec![0.0; FFT_SIZE / 2 + 1],
            flux: Vec::new(),
            chroma: [0.0; 12],
            bands: Vec::new(),
            band_edges: edges,
            band_bins: [
                // Bin 0 is DC and is not counted.
                (edges[0] - 1).max(1) as f32,
                (edges[1] - edges[0]).max(1) as f32,
                (FFT_SIZE / 2 + 1 - edges[1]).max(1) as f32,
            ],
        }
    }

    /// Feed mono samples. Whole windows are consumed; the remainder is kept.
    pub fn feed(&mut self, mono: &[f32]) {
        self.pending.extend_from_slice(mono);
        while self.pending.len() >= FFT_SIZE {
            self.transform();
            self.pending.drain(..HOP);
        }
        // Keep the buffer from growing without bound on a long track.
        if self.pending.capacity() > FFT_SIZE * 8 {
            self.pending.shrink_to(FFT_SIZE * 2);
        }
    }

    fn transform(&mut self) {
        for (index, slot) in self.scratch.iter_mut().enumerate() {
            *slot = Complex::new(self.pending[index] * self.window[index], 0.0);
        }
        self.fft.process(&mut self.scratch);

        let bins = self.magnitude_sum.len();
        let mut flux = 0.0f32;
        let mut bands = [0.0f32; 3];
        for bin in 0..bins {
            let magnitude = self.scratch[bin].norm();
            self.magnitude_sum[bin] += magnitude as f64;

            // Bin 0 is DC, not sound -- skip it, or a file with any offset
            // reads as permanently bass-heavy.
            if bin > 0 {
                let band = if bin < self.band_edges[0] {
                    0
                } else if bin < self.band_edges[1] {
                    1
                } else {
                    2
                };
                // Power, not magnitude: summing magnitudes lets a thousand
                // near-silent bins outweigh a handful of loud ones.
                bands[band] += magnitude * magnitude;
            }

            // Spectral flux: only rises count, because an onset is energy
            // appearing, not energy fading.
            let rise = magnitude - self.previous[bin];
            if rise > 0.0 {
                flux += rise;
            }
            self.previous[bin] = magnitude;

            if let Some(class) = pitch_class(self.bin_hz(bin)) {
                self.chroma[class] += magnitude as f64;
            }
        }
        self.flux.push(flux);
        // Per bin, so the bands are comparable to each other regardless of how
        // wide they are: this is spectral density, not total energy.
        for (value, count) in bands.iter_mut().zip(self.band_bins.iter()) {
            *value /= count;
        }
        self.bands.push(bands);
        self.windows += 1;
    }

    fn bin_hz(&self, bin: usize) -> f32 {
        bin as f32 * self.rate as f32 / FFT_SIZE as f32
    }

    fn bin_of(&self, hz: f32) -> usize {
        ((hz * FFT_SIZE as f32 / self.rate as f32).round() as usize)
            .min(self.magnitude_sum.len() - 1)
    }

    /// Onsets per window, and how many windows there are per second.
    pub fn onset_envelope(&self) -> &[f32] {
        &self.flux
    }

    /// Low/mid/high spectral density per window, one entry per
    /// `onset_envelope` entry.
    pub fn band_trace(&self) -> &[[f32; 3]] {
        &self.bands
    }

    pub fn hops_per_second(&self) -> f32 {
        self.rate as f32 / HOP as f32
    }

    pub fn summarise(&self) -> SpectrumSummary {
        let mut chroma = [0.0f32; 12];
        let total: f64 = self.chroma.iter().sum();
        if total > 0.0 {
            for (out, sum) in chroma.iter_mut().zip(self.chroma.iter()) {
                *out = (sum / total) as f32;
            }
        }

        if self.windows == 0 {
            return SpectrumSummary {
                cutoff_hz: None,
                rolloff_db: 0.0,
                transcode_score: 0.0,
                chroma,
            };
        }

        // Average, to decibels, then smoothed: a single noisy bin should not
        // decide where a track's spectrum ends.
        let average: Vec<f32> = self
            .magnitude_sum
            .iter()
            .map(|sum| (sum / self.windows as f64) as f32)
            .collect();
        let decibels: Vec<f32> = average
            .iter()
            .map(|magnitude| 20.0 * (magnitude + 1e-12).log10())
            .collect();
        let smoothed = smooth(&decibels, 5);

        // Reference from where music actually lives, not from the loudest bin,
        // which on a bass-heavy track sits at 50 Hz and drags the floor down.
        let low = self.bin_of(300.0);
        let high = self.bin_of(4_000.0);
        let reference = smoothed[low..=high]
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        if !reference.is_finite() {
            return SpectrumSummary {
                cutoff_hz: None,
                rolloff_db: 0.0,
                transcode_score: 0.0,
                chroma,
            };
        }
        let floor = reference - FLOOR_DB;

        let Some(cutoff_bin) = (low..smoothed.len()).rev().find(|bin| smoothed[*bin] > floor)
        else {
            return SpectrumSummary {
                cutoff_hz: None,
                rolloff_db: 0.0,
                transcode_score: 0.0,
                chroma,
            };
        };
        let cutoff_hz = self.bin_hz(cutoff_bin);

        // Measure the cliff across the cutoff, not from it: the cutoff bin is
        // by definition the last one still above the floor, so starting there
        // understates the drop and makes a brick wall look like a slope.
        //
        // Bands rather than single bins on both sides. A real spectrum is full
        // of peaks and troughs, and one sample landing in a trough would report
        // a cliff that is not there -- or miss one that is.
        let below = band_max(
            &smoothed,
            self.bin_of((cutoff_hz - 2_000.0).max(300.0)),
            self.bin_of((cutoff_hz - 200.0).max(400.0)),
        );
        let above = band_max(
            &smoothed,
            self.bin_of(cutoff_hz + 500.0),
            self.bin_of(cutoff_hz + 2_500.0),
        );
        let rolloff_db = match (below, above) {
            // Max on both sides is the conservative reading: it understates the
            // drop, which for a suspicion is the right direction to err in.
            (Some(below), Some(above)) => below - above,
            _ => 0.0,
        };

        SpectrumSummary {
            cutoff_hz: Some(cutoff_hz.round() as u32),
            rolloff_db,
            transcode_score: score(cutoff_hz, rolloff_db, self.rate),
            chroma,
        }
    }
}

/// Turn a cutoff and a rolloff into a suspicion.
///
/// Two things have to be true at once for this to mean anything: the spectrum
/// stops well short of where the format could carry it, and it stops abruptly.
/// Either alone is ordinary music.
fn score(cutoff_hz: f32, rolloff_db: f32, rate: u32) -> f32 {
    let nyquist = rate as f32 / 2.0;
    let fraction = cutoff_hz / nyquist;

    // How much bandwidth is missing. Tapered rather than a threshold: a file
    // reaching 91% of Nyquist and one reaching 93% are not meaningfully
    // different, and a cliff in the score would put them either side of an
    // accusation.
    //
    // Deliberately not scaled by *how far* below Nyquist the cutoff sits: a
    // 320 kbps encoder cuts around 20kHz, which is 91% of 44.1kHz Nyquist, so
    // penalising "close to full bandwidth" would miss exactly the sources most
    // worth catching.
    let bandwidth = ((1.0 - fraction) / 0.08).clamp(0.0, 1.0);

    // A gentle slope is a mix, not an encoder. Measured across the cutoff, a
    // brick wall is tens of dB and an ordinary rolloff is single figures.
    let steepness = ((rolloff_db - 20.0) / 40.0).clamp(0.0, 1.0);

    // Lossy encoders cut between about 14 and 21 kHz. Below that, a dark or old
    // recording is at least as likely an explanation, so the suspicion fades
    // rather than growing.
    let plausibility = if cutoff_hz >= 14_000.0 {
        1.0
    } else if cutoff_hz >= 10_000.0 {
        (cutoff_hz - 10_000.0) / 4_000.0
    } else {
        0.0
    };

    (steepness * plausibility * bandwidth).clamp(0.0, 1.0)
}

/// FFT bin holding a frequency, clamped to the half-spectrum.
fn bin_for(hz: f32, rate: u32) -> usize {
    ((hz * FFT_SIZE as f32 / rate as f32).round() as usize).min(FFT_SIZE / 2)
}

/// Strongest level anywhere in a band, or `None` if the band is empty.
fn band_max(values: &[f32], from: usize, to: usize) -> Option<f32> {
    if from >= to || from >= values.len() {
        return None;
    }
    let to = to.min(values.len());
    values[from..to]
        .iter()
        .copied()
        .fold(None::<f32>, |acc, v| Some(acc.map_or(v, |a| a.max(v))))
}

fn smooth(values: &[f32], width: usize) -> Vec<f32> {
    if values.len() < width || width < 2 {
        return values.to_vec();
    }
    let half = width / 2;
    (0..values.len())
        .map(|index| {
            let from = index.saturating_sub(half);
            let to = (index + half + 1).min(values.len());
            values[from..to].iter().sum::<f32>() / (to - from) as f32
        })
        .collect()
}

/// Pitch class of a frequency, or `None` outside the range where it means
/// anything musical.
fn pitch_class(hz: f32) -> Option<usize> {
    if !(55.0..=5_000.0).contains(&hz) {
        return None;
    }
    // A4 = 440 Hz is pitch class 9.
    let semitones = 12.0 * (hz / 440.0).log2();
    let class = (semitones.round() as i64 + 9).rem_euclid(12);
    Some(class as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    /// White-ish noise, optionally brick-walled or gently rolled off.
    fn noise(samples: usize, shape: impl Fn(f32) -> f32) -> Vec<f32> {
        // A deterministic pseudo-random sequence, then shaped in the frequency
        // domain by summing sinusoids -- crude, but it lets a test state
        // exactly what the spectrum should look like.
        let mut out = vec![0.0f32; samples];
        let mut hz = 60.0f32;
        while hz < RATE as f32 / 2.0 {
            let gain = shape(hz);
            if gain > 0.0 {
                let step = std::f32::consts::TAU * hz / RATE as f32;
                let phase = hz * 0.37;
                for (index, sample) in out.iter_mut().enumerate() {
                    *sample += gain * (step * index as f32 + phase).sin();
                }
            }
            hz *= 1.004;
        }
        let peak = out.iter().fold(0.0f32, |a, s| a.max(s.abs())).max(1e-9);
        out.iter_mut().for_each(|s| *s /= peak * 1.2);
        out
    }

    fn analyse(signal: &[f32]) -> SpectrumSummary {
        let mut collector = SpectrumCollector::new(RATE);
        collector.feed(signal);
        collector.summarise()
    }

    #[test]
    fn a_full_bandwidth_signal_is_not_suspected() {
        let summary = analyse(&noise(RATE as usize, |_| 1.0));
        assert!(
            summary.cutoff_hz.unwrap() > 19_000,
            "cutoff {:?}",
            summary.cutoff_hz
        );
        assert_eq!(summary.transcode_score, 0.0, "nothing was removed");
    }

    #[test]
    fn a_brick_wall_at_16k_looks_exactly_like_a_transcode() {
        let summary = analyse(&noise(RATE as usize, |hz| if hz < 16_000.0 { 1.0 } else { 0.0 }));
        let cutoff = summary.cutoff_hz.unwrap();
        assert!(
            (15_000..=17_000).contains(&cutoff),
            "cutoff {cutoff} should sit at the wall"
        );
        assert!(
            summary.transcode_score > 0.7,
            "score {} for a hard cutoff (rolloff {} dB)",
            summary.transcode_score,
            summary.rolloff_db
        );
    }

    #[test]
    fn a_dark_mix_is_not_mistaken_for_a_transcode() {
        // Rolls off gradually from 6kHz: quiet up top, but no cliff. This is
        // the false positive the design document warns about.
        let summary = analyse(&noise(RATE as usize, |hz| {
            if hz < 6_000.0 {
                1.0
            } else {
                (1.0 - (hz - 6_000.0) / 16_000.0).max(0.02)
            }
        }));
        assert!(
            summary.transcode_score < 0.35,
            "score {} on a gentle rolloff (cutoff {:?}, rolloff {} dB)",
            summary.transcode_score,
            summary.cutoff_hz,
            summary.rolloff_db
        );
    }

    #[test]
    fn a_high_bitrate_encoder_cutoff_is_still_caught() {
        // 320 kbps cuts around 20kHz, which is 91% of 44.1kHz Nyquist. Scoring
        // must not treat "nearly full bandwidth" as innocent, or it would miss
        // the most common transcode source there is.
        let summary = analyse(&noise(RATE as usize, |hz| if hz < 20_000.0 { 1.0 } else { 0.0 }));
        assert!(
            summary.transcode_score > 0.5,
            "score {} at a 20kHz wall (cutoff {:?}, rolloff {} dB)",
            summary.transcode_score,
            summary.cutoff_hz,
            summary.rolloff_db
        );
    }

    #[test]
    fn silence_produces_no_cutoff_rather_than_a_false_one() {
        let summary = analyse(&vec![0.0; RATE as usize]);
        assert_eq!(summary.transcode_score, 0.0);
    }

    #[test]
    fn a_pure_tone_lands_in_the_right_pitch_class() {
        // 440 Hz is A, pitch class 9.
        let tone: Vec<f32> = (0..RATE as usize)
            .map(|n| (std::f32::consts::TAU * 440.0 * n as f32 / RATE as f32).sin() * 0.5)
            .collect();
        let summary = analyse(&tone);
        let strongest = summary
            .chroma
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(strongest, 9, "chroma {:?}", summary.chroma);
    }
}
