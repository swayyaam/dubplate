//! Tempo from the onset envelope.
//!
//! The design document is explicit that 80% right is the goal here and 95% is
//! a research project. So: autocorrelate the onsets, fold octave errors into
//! the range people actually count in, and report a confidence rather than
//! pretending certainty.

#[derive(Debug, Clone, Copy)]
pub struct TempoEstimate {
    pub bpm: f32,
    /// How much the winning period stands out from the rest. Low means the
    /// track has no steady pulse, or several.
    pub confidence: f32,
}

/// Where most music people would count. Estimates outside it are usually the
/// same tempo counted at half or double speed.
const MIN_BPM: f32 = 60.0;
const MAX_BPM: f32 = 200.0;
/// Tempos are folded into this range, which is where dance music lives.
const FOLD_LOW: f32 = 80.0;
const FOLD_HIGH: f32 = 165.0;
/// How much better a longer period must score before it beats a shorter one.
const HARMONIC_MARGIN: f32 = 1.05;

pub fn estimate(onsets: &[f32], hops_per_second: f32) -> Option<TempoEstimate> {
    if onsets.len() < 64 || hops_per_second <= 0.0 {
        return None;
    }

    // Emphasise change rather than level: a loud passage is not an onset, a
    // sudden one is. The window has to be shorter than the beat it is looking
    // for -- at half a second it swallows a fast pulse and invents structure
    // that is not there.
    let envelope = rectify(onsets, (hops_per_second * 0.15) as usize);

    let min_lag = (hops_per_second * 60.0 / MAX_BPM).round().max(1.0) as usize;
    let max_lag = (hops_per_second * 60.0 / MIN_BPM).round() as usize;
    if max_lag >= envelope.len() {
        return None;
    }

    let mut best_lag = 0usize;
    let mut best = 0.0f32;
    let mut total = 0.0f64;
    let mut count = 0usize;

    for lag in min_lag..=max_lag {
        let mut sum = 0.0f32;
        for index in lag..envelope.len() {
            sum += envelope[index] * envelope[index - lag];
        }
        let score = sum / (envelope.len() - lag) as f32;
        total += score as f64;
        count += 1;
        // A periodic signal correlates just as well at twice and three times
        // its period, and the three scores come out equal to several decimal
        // places. Requiring a clear margin, while walking lags from short to
        // long, keeps the fundamental rather than whichever harmonic won on
        // floating-point noise.
        if best_lag == 0 || score > best * HARMONIC_MARGIN {
            best = score;
            best_lag = lag;
        }
    }
    if best_lag == 0 || count == 0 || !best.is_finite() || best <= 0.0 {
        return None;
    }

    let mean = (total / count as f64) as f32;
    let confidence = if mean > 0.0 {
        ((best / mean - 1.0) / 2.0).clamp(0.0, 1.0)
    } else {
        0.0
    };

    Some(TempoEstimate {
        bpm: fold(hops_per_second * 60.0 / best_lag as f32),
        confidence,
    })
}

/// Subtract a moving average and keep only the rises.
fn rectify(onsets: &[f32], width: usize) -> Vec<f32> {
    let width = width.max(3);
    let half = width / 2;
    (0..onsets.len())
        .map(|index| {
            let from = index.saturating_sub(half);
            let to = (index + half + 1).min(onsets.len());
            let mean = onsets[from..to].iter().sum::<f32>() / (to - from) as f32;
            (onsets[index] - mean).max(0.0)
        })
        .collect()
}

/// Halve or double until the tempo lands where it would be counted.
///
/// Autocorrelation cannot tell 75 from 150: both are true statements about the
/// same pulse. Picking the one a person would say is the best available answer.
fn fold(mut bpm: f32) -> f32 {
    if !bpm.is_finite() || bpm <= 0.0 {
        return 0.0;
    }
    while bpm < FOLD_LOW {
        bpm *= 2.0;
    }
    while bpm > FOLD_HIGH {
        bpm /= 2.0;
    }
    bpm
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An onset envelope with a click every `period` frames.
    fn pulses(frames: usize, period: usize) -> Vec<f32> {
        (0..frames)
            .map(|index| if index % period == 0 { 1.0 } else { 0.05 })
            .collect()
    }

    #[test]
    fn a_steady_pulse_is_measured() {
        // 86 frames per second, a beat every 60 frames -> 86 BPM.
        let hops = 86.13;
        let estimate = estimate(&pulses(2000, 60), hops).unwrap();
        assert!(
            (estimate.bpm - 86.0).abs() < 2.0,
            "got {} BPM",
            estimate.bpm
        );
        assert!(estimate.confidence > 0.2, "confidence {}", estimate.confidence);
    }

    #[test]
    fn a_fast_pulse_is_folded_into_the_range_people_count_in() {
        // A beat every 26 frames is about 199 BPM. Nobody counts that; they
        // count the half-time, which is what gets reported.
        let hops = 86.13;
        let estimate = estimate(&pulses(2000, 26), hops).unwrap();
        assert!(
            (estimate.bpm - 99.4).abs() < 4.0,
            "got {} BPM, expected it folded to about 99",
            estimate.bpm
        );
    }

    #[test]
    fn folding_maps_octaves_onto_one_answer() {
        // The same pulse counted at four different speeds gives one number.
        for bpm in [30.0, 60.0, 120.0, 240.0] {
            assert!((fold(bpm) - 120.0).abs() < 0.001, "{bpm} folded to {}", fold(bpm));
        }
        // Already in range: left alone.
        assert!((fold(128.0) - 128.0).abs() < 0.001);
    }

    #[test]
    fn a_pulse_slower_than_the_search_range_is_not_invented() {
        // A beat every two seconds is 30 BPM, below the range searched, and its
        // harmonics do not land inside it either. Nothing correlates, so
        // nothing is reported -- which beats naming a number that came from
        // noise.
        assert!(estimate(&pulses(4000, 172), 86.13).is_none());
    }

    #[test]
    fn noise_reports_low_confidence_rather_than_a_confident_wrong_answer() {
        let mut seed = 12345u32;
        let noise: Vec<f32> = (0..2000)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 16) as f32 / 65535.0
            })
            .collect();
        let estimate = estimate(&noise, 86.13).unwrap();
        assert!(
            estimate.confidence < 0.3,
            "confidence {} on noise",
            estimate.confidence
        );
    }

    #[test]
    fn a_track_too_short_to_judge_gives_nothing() {
        assert!(estimate(&pulses(20, 5), 86.13).is_none());
    }
}
