//! The waveform the seek bar draws: level, body, and colour.
//!
//! A peak-only waveform of a modern master is a rectangle, and it is a
//! rectangle *honestly* -- the loudest sample in any given second of a
//! brickwalled club track is the limiter ceiling, every second. Three things
//! fix that, and all three are needed together:
//!
//!   - **RMS as well as peak.** Peak says what the limiter did; RMS says what
//!     the arrangement is doing. An intro, a build, a drop and a breakdown are
//!     four different RMS levels and one identical peak level.
//!   - **A logarithmic scale.** Hearing is roughly logarithmic, so a breakdown
//!     at -12dBFS should not draw at a quarter height.
//!   - **Band mix per column.** Which of low/mid/high is carrying the energy,
//!     so the kick dropping out and the hats coming in are visible rather than
//!     merely implied.
//!
//! Stored as bytes rather than floats. A byte across 60dB is 0.24dB per step,
//! which is far finer than a screen pixel, and it makes the whole library's
//! waveforms about 7MB instead of 100.

/// Columns per track. Roughly four times the widest sensible on-screen column
/// count, so the display always downsamples rather than interpolates.
pub const BUCKETS: usize = 1000;

/// Dynamic range covered by the stored byte. Wider than anything the display
/// uses, so the drawing floor can be re-chosen without re-analysing a library.
pub const FLOOR_DB: f32 = 60.0;

/// How far below the loudest band another band can sit and still show any
/// colour at all.
///
/// The three bands are never within a few decibels of each other: spectral
/// density in music falls roughly 30dB from the bass to the top octave, so
/// comparing them linearly makes every track on earth read as pure bass.
/// Comparing them across a fixed window, relative to whichever band is loudest
/// in that column, is what makes a breakdown look different from a drop.
const BAND_RANGE_DB: f32 = 40.0;

const MAGIC: &[u8; 4] = b"DPWF";
const VERSION: u8 = 2;
const LANES: u8 = 5;
const HEADER: usize = 12;

/// One waveform, as five parallel lanes of `BUCKETS` bytes.
///
/// Lanes rather than structs-per-bucket because that is the shape the canvas
/// wants, and because it is what goes over the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Waveform {
    /// Loudest sample in the column. The outline.
    pub peak: Vec<u8>,
    /// Root-mean-square level of the column. The filled body.
    pub rms: Vec<u8>,
    /// Share of the column's energy below 200Hz, 0-255.
    pub low: Vec<u8>,
    /// Share between 200Hz and 2kHz.
    pub mid: Vec<u8>,
    /// Share above 2kHz.
    pub high: Vec<u8>,
}

impl Waveform {
    pub fn len(&self) -> usize {
        self.peak.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peak.is_empty()
    }

    /// Build from the fine-grained series collected during the decode pass.
    ///
    /// The three series are sampled on different clocks -- peaks and squares
    /// every 256 frames, bands every FFT hop -- so each is resampled to
    /// `BUCKETS` independently rather than being forced into step. Both are
    /// uniform in time, which is all that alignment requires.
    pub fn build(peaks: &[f32], squares: &[f32], bands: &[[f32; 3]]) -> Self {
        let peak = resample(peaks, BUCKETS, Collapse::Max);
        // Energy, so the collapse happens in the square domain and the root is
        // taken once at the end.
        let rms = resample(squares, BUCKETS, Collapse::Mean);

        let mut low = Vec::with_capacity(BUCKETS);
        let mut mid = Vec::with_capacity(BUCKETS);
        let mut high = Vec::with_capacity(BUCKETS);
        for bucket in 0..BUCKETS {
            let (from, to) = span(bands.len(), BUCKETS, bucket);
            let mut sums = [0.0f64; 3];
            for entry in &bands[from..to] {
                for (sum, value) in sums.iter_mut().zip(entry.iter()) {
                    *sum += *value as f64;
                }
            }
            // The mix, not the level: the column's level is already in `rms`.
            // A silent column has no colour, so it reports an even split
            // rather than a division by zero.
            let mix = mix_bands([
                sums[0] as f32 / (to - from) as f32,
                sums[1] as f32 / (to - from) as f32,
                sums[2] as f32 / (to - from) as f32,
            ]);
            low.push(mix[0]);
            mid.push(mix[1]);
            high.push(mix[2]);
        }

        Self {
            peak: peak.iter().map(|v| encode_db(*v)).collect(),
            rms: rms.iter().map(|v| encode_db(v.max(0.0).sqrt())).collect(),
            low,
            mid,
            high,
        }
    }

    /// Serialise for the cache and for the wire. Versioned, so an older
    /// waveform is regenerated rather than misread.
    pub fn to_bytes(&self) -> Vec<u8> {
        let buckets = self.len();
        let mut out = Vec::with_capacity(HEADER + buckets * LANES as usize);
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(LANES);
        out.extend_from_slice(&0u16.to_le_bytes()); // reserved
        out.extend_from_slice(&(buckets as u32).to_le_bytes());
        // Lane-major: the reader hands each lane straight to the canvas as a
        // subarray, with no per-bucket deinterleaving.
        for lane in [&self.peak, &self.rms, &self.low, &self.mid, &self.high] {
            out.extend_from_slice(lane);
        }
        out
    }

    /// `None` for anything that is not a version 2 waveform, which is the
    /// signal to recompute rather than to display something wrong.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HEADER || &bytes[..4] != MAGIC || bytes[4] != VERSION || bytes[5] != LANES {
            return None;
        }
        let buckets = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
        if buckets == 0 || bytes.len() < HEADER + buckets * LANES as usize {
            return None;
        }
        let lane = |index: usize| {
            let from = HEADER + index * buckets;
            bytes[from..from + buckets].to_vec()
        };
        Some(Self {
            peak: lane(0),
            rms: lane(1),
            low: lane(2),
            mid: lane(3),
            high: lane(4),
        })
    }
}

/// Three band densities to a colour mix summing to 255.
///
/// Each band is scored by how far it sits below the loudest of the three, so
/// the result says which part of the spectrum is carrying this moment rather
/// than restating that bass has the most energy -- which is true of every
/// column of every track and therefore says nothing.
fn mix_bands(density: [f32; 3]) -> [u8; 3] {
    let decibels: [f32; 3] = [
        power_db(density[0]),
        power_db(density[1]),
        power_db(density[2]),
    ];
    let loudest = decibels.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !loudest.is_finite() {
        // Silence. An even split is the honest answer, and it is grey.
        return [85, 85, 85];
    }

    let weights: [f32; 3] = [
        band_weight(decibels[0], loudest),
        band_weight(decibels[1], loudest),
        band_weight(decibels[2], loudest),
    ];
    let total: f32 = weights.iter().sum();
    if total <= 0.0 {
        return [85, 85, 85];
    }
    let mut mix = [0u8; 3];
    for (slot, weight) in mix.iter_mut().zip(weights.iter()) {
        *slot = (weight / total * 255.0).round() as u8;
    }
    mix
}

fn power_db(density: f32) -> f32 {
    if density > 0.0 {
        10.0 * density.log10()
    } else {
        f32::NEG_INFINITY
    }
}

fn band_weight(db: f32, loudest: f32) -> f32 {
    if !db.is_finite() {
        return 0.0;
    }
    ((db - (loudest - BAND_RANGE_DB)) / BAND_RANGE_DB).clamp(0.0, 1.0)
}

/// Linear amplitude to a byte on a decibel scale.
///
/// Full scale is 255 and `FLOOR_DB` below it is 0. Samples above full scale --
/// legal in float, and present in real files -- clamp rather than wrap; the
/// honest measurement of those lives in the track's true peak, not here.
pub fn encode_db(linear: f32) -> u8 {
    // NaN checked explicitly rather than relying on a negated comparison: a
    // decoder that produces one should draw silence, not a full-height bar.
    if !linear.is_finite() || linear <= 0.0 {
        return 0;
    }
    let db = 20.0 * linear.log10();
    let fraction = (db + FLOOR_DB) / FLOOR_DB;
    (fraction.clamp(0.0, 1.0) * 255.0).round() as u8
}

enum Collapse {
    Max,
    Mean,
}

/// Resample a series to exactly `buckets` values.
///
/// Peaks collapse with a maximum, because a transient that survives to the
/// screen is what makes a waveform recognisable. Energy collapses with a mean,
/// because that is what energy does.
fn resample(values: &[f32], buckets: usize, how: Collapse) -> Vec<f32> {
    if values.is_empty() {
        return vec![0.0; buckets];
    }
    if values.len() <= buckets {
        // Shorter than the target: stretch, rather than leave a gap at the end.
        return (0..buckets)
            .map(|index| values[index * values.len() / buckets])
            .collect();
    }
    (0..buckets)
        .map(|index| {
            let (from, to) = span(values.len(), buckets, index);
            let slice = &values[from..to];
            match how {
                Collapse::Max => slice.iter().copied().fold(0.0f32, f32::max),
                Collapse::Mean => {
                    slice.iter().map(|v| *v as f64).sum::<f64>() as f32 / slice.len() as f32
                }
            }
        })
        .collect()
}

/// The half-open range of `len` items belonging to `bucket`, never empty.
fn span(len: usize, buckets: usize, bucket: usize) -> (usize, usize) {
    if len == 0 {
        return (0, 0);
    }
    let from = (bucket * len / buckets).min(len - 1);
    let to = ((bucket + 1) * len / buckets).clamp(from + 1, len);
    (from, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_is_the_top_of_the_range_and_silence_the_bottom() {
        assert_eq!(encode_db(1.0), 255);
        assert_eq!(encode_db(0.0), 0);
        assert_eq!(encode_db(-0.0), 0);
        // -60dB is the floor exactly.
        assert_eq!(encode_db(0.001), 0);
    }

    #[test]
    fn above_full_scale_clamps_rather_than_wrapping() {
        // Real files peak above 1.0 in float. Wrapping would draw them as
        // near-silent, which is the worst possible failure for a waveform.
        assert_eq!(encode_db(1.5), 255);
        assert_eq!(encode_db(8.0), 255);
    }

    #[test]
    fn a_broken_sample_draws_silence_rather_than_a_full_height_bar() {
        assert_eq!(encode_db(f32::NAN), 0);
        assert_eq!(encode_db(f32::INFINITY), 0);
        assert_eq!(encode_db(-1.0), 0);
    }

    #[test]
    fn halving_the_amplitude_moves_six_decibels_down_the_scale() {
        let full = encode_db(1.0) as i32;
        let half = encode_db(0.5) as i32;
        // 6dB of 60 is a tenth of the range, so 25-26 steps of 255.
        assert!((24..=27).contains(&(full - half)), "moved {} steps", full - half);
    }

    #[test]
    fn a_loud_quiet_track_keeps_its_shape_instead_of_flattening() {
        // The failure this whole module exists to fix: a signal that is at the
        // ceiling half the time and 20dB down the other half must not render
        // as one flat level.
        let peaks: Vec<f32> = (0..1000).map(|i| if i < 500 { 1.0 } else { 0.1 }).collect();
        let squares: Vec<f32> = peaks.iter().map(|p| p * p * 0.5).collect();
        let waveform = Waveform::build(&peaks, &squares, &[]);

        let loud = waveform.rms[100];
        let quiet = waveform.rms[900];
        assert!(
            loud > quiet + 60,
            "loud {loud} and quiet {quiet} should be far apart on the scale"
        );
    }

    #[test]
    fn the_band_mix_reports_where_the_energy_actually_is() {
        let peaks = vec![1.0f32; 100];
        let squares = vec![0.5f32; 100];
        // All energy in the high band.
        let bands = vec![[0.0f32, 0.0, 1.0]; 100];
        let waveform = Waveform::build(&peaks, &squares, &bands);
        assert_eq!(waveform.high[500], 255);
        assert_eq!(waveform.low[500], 0);
        assert_eq!(waveform.mid[500], 0);
    }

    #[test]
    fn a_silent_column_has_no_colour_rather_than_a_division_by_zero() {
        let waveform = Waveform::build(&[0.0; 10], &[0.0; 10], &[[0.0; 3]; 10]);
        assert_eq!(waveform.rms[0], 0);
        assert_eq!(waveform.low[0], waveform.high[0], "an even, meaningless split");
    }

    #[test]
    fn a_waveform_survives_a_round_trip_through_bytes() {
        let peaks: Vec<f32> = (0..5000).map(|i| (i % 100) as f32 / 100.0).collect();
        let squares: Vec<f32> = peaks.iter().map(|p| p * p).collect();
        let bands: Vec<[f32; 3]> = (0..3000)
            .map(|i| [i as f32 % 3.0, i as f32 % 5.0, i as f32 % 7.0])
            .collect();
        let waveform = Waveform::build(&peaks, &squares, &bands);

        let bytes = waveform.to_bytes();
        assert_eq!(bytes.len(), HEADER + BUCKETS * 5);
        assert_eq!(Waveform::from_bytes(&bytes), Some(waveform));
    }

    #[test]
    fn an_older_or_damaged_cache_file_is_refused_rather_than_misread() {
        // Version 1 was a bare array of f32 with no header. Reading one as a
        // version 2 waveform would draw noise, so it must fail cleanly and be
        // regenerated.
        let v1: Vec<u8> = (0..4000f32.to_le_bytes().len() * 1000).map(|i| i as u8).collect();
        assert_eq!(Waveform::from_bytes(&v1), None);
        assert_eq!(Waveform::from_bytes(b"DPWF"), None);
        assert_eq!(Waveform::from_bytes(&[]), None);

        // Right header, truncated body.
        let mut short = Waveform::build(&[1.0; 10], &[1.0; 10], &[]).to_bytes();
        short.truncate(HEADER + 100);
        assert_eq!(Waveform::from_bytes(&short), None);
    }

    #[test]
    fn every_bucket_is_filled_even_from_a_track_shorter_than_the_bucket_count() {
        let waveform = Waveform::build(&[0.5; 7], &[0.25; 7], &[[1.0, 1.0, 1.0]; 3]);
        assert_eq!(waveform.len(), BUCKETS);
        assert_eq!(waveform.rms.len(), BUCKETS);
        assert!(waveform.rms.iter().all(|v| *v > 0), "no gaps at the end");
    }
}
