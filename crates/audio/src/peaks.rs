//! Waveform peaks for the seek bar.
//!
//! One full decode pass per track, which at 1500x realtime costs about a tenth
//! of a second. Phase 6 folds this into the analysis pipeline, where the same
//! decode also feeds loudness, BPM, key and spectral analysis; until then it is
//! computed on demand for whatever is playing.

use std::path::Path;

use crate::decode::{DecodeError, TrackDecoder};

/// Frames per fine-grained peak before downsampling. Small enough that a short
/// track still has detail, large enough that a long one does not need much
/// memory: ten minutes of 44.1kHz audio is about 400KB of intermediate peaks.
const WINDOW: usize = 256;

/// Peak amplitude per bucket, left to right.
///
/// Values are the true sample peaks, not normalised. Several real files peak
/// above 1.0 -- legal in float, and the caller should know rather than have it
/// quietly scaled away.
pub fn compute(path: &Path, buckets: usize) -> Result<Vec<f32>, DecodeError> {
    let buckets = buckets.max(1);
    let mut decoder = TrackDecoder::open(path)?;
    let channels = decoder.format().channels.max(1) as usize;

    let mut fine: Vec<f32> = Vec::new();
    let mut window_peak = 0.0f32;
    let mut window_frames = 0usize;

    while let Some(block) = decoder.next_block()? {
        for frame in block.chunks(channels) {
            // The loudest channel, so a hard-panned moment still shows up.
            let mut peak = 0.0f32;
            for sample in frame {
                peak = peak.max(sample.abs());
            }
            window_peak = window_peak.max(peak);
            window_frames += 1;
            if window_frames == WINDOW {
                fine.push(window_peak);
                window_peak = 0.0;
                window_frames = 0;
            }
        }
    }
    if window_frames > 0 {
        fine.push(window_peak);
    }

    Ok(downsample(&fine, buckets))
}

/// Collapse fine peaks into `buckets`, keeping the maximum of each span.
///
/// Maximum rather than mean: averaging a waveform turns transients into mush,
/// and the transients are what makes a waveform recognisable as a track.
fn downsample(fine: &[f32], buckets: usize) -> Vec<f32> {
    if fine.is_empty() {
        return vec![0.0; buckets];
    }
    if fine.len() <= buckets {
        // Shorter than the target: stretch rather than leave a gap.
        return (0..buckets)
            .map(|index| fine[index * fine.len() / buckets])
            .collect();
    }

    let mut out = Vec::with_capacity(buckets);
    for index in 0..buckets {
        let start = index * fine.len() / buckets;
        let end = ((index + 1) * fine.len() / buckets).max(start + 1);
        let peak = fine[start..end.min(fine.len())]
            .iter()
            .copied()
            .fold(0.0f32, f32::max);
        out.push(peak);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsampling_keeps_peaks_rather_than_averaging_them_away() {
        // A single transient in an otherwise quiet stretch must survive.
        let mut fine = vec![0.1f32; 100];
        fine[42] = 1.0;
        let out = downsample(&fine, 10);
        assert_eq!(out.len(), 10);
        assert_eq!(out[4], 1.0, "the transient must not be averaged out");
        assert!(out.iter().filter(|v| **v > 0.9).count() == 1);
    }

    #[test]
    fn a_short_track_still_fills_the_bar() {
        let out = downsample(&[0.5, 1.0], 8);
        assert_eq!(out.len(), 8);
        assert!(out.iter().all(|v| *v > 0.0));
    }

    #[test]
    fn silence_is_a_flat_line_not_an_empty_vec() {
        assert_eq!(downsample(&[], 5), vec![0.0; 5]);
    }
}
