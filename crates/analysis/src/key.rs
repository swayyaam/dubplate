//! Musical key by correlating a chroma vector against Krumhansl profiles.
//!
//! Reported in Camelot notation, because that is what the wheel on every DJ
//! controller uses and this library is full of mixes.

/// Krumhansl-Kessler key profiles: how strongly each pitch class belongs to a
/// major or minor key, from listening experiments.
const MAJOR: [f32; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR: [f32; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// Camelot number for each root, indexed by pitch class with C = 0.
const MAJOR_CAMELOT: [u8; 12] = [8, 3, 10, 5, 12, 7, 2, 9, 4, 11, 6, 1];
const MINOR_CAMELOT: [u8; 12] = [5, 12, 7, 2, 9, 4, 11, 6, 1, 8, 3, 10];

#[derive(Debug, Clone)]
pub struct KeyEstimate {
    /// Camelot, e.g. "8A" for A minor.
    pub camelot: String,
    /// Readable, e.g. "A minor".
    pub name: String,
    /// Correlation with the winning profile, 0 to 1. Modal and atonal music
    /// scores low, correctly.
    pub confidence: f32,
}

pub fn estimate(chroma: &[f32; 12]) -> Option<KeyEstimate> {
    if chroma.iter().sum::<f32>() <= 0.0 {
        return None;
    }

    let mut best: Option<(f32, usize, bool)> = None;
    for root in 0..12 {
        for minor in [false, true] {
            let profile = if minor { &MINOR } else { &MAJOR };
            // Rotate the profile to the candidate root rather than the chroma,
            // so the comparison is always against the same observed data.
            let rotated: Vec<f32> = (0..12).map(|i| profile[(i + 12 - root) % 12]).collect();
            let score = correlation(chroma, &rotated);
            if best.map(|(current, _, _)| score > current).unwrap_or(true) {
                best = Some((score, root, minor));
            }
        }
    }

    let (score, root, minor) = best?;
    let number = if minor { MINOR_CAMELOT[root] } else { MAJOR_CAMELOT[root] };
    Some(KeyEstimate {
        camelot: format!("{number}{}", if minor { 'A' } else { 'B' }),
        name: format!("{} {}", NAMES[root], if minor { "minor" } else { "major" }),
        confidence: score.clamp(0.0, 1.0),
    })
}

fn correlation(a: &[f32; 12], b: &[f32]) -> f32 {
    let mean_a = a.iter().sum::<f32>() / 12.0;
    let mean_b = b.iter().sum::<f32>() / 12.0;
    let mut covariance = 0.0;
    let mut var_a = 0.0;
    let mut var_b = 0.0;
    for index in 0..12 {
        let da = a[index] - mean_a;
        let db = b[index] - mean_b;
        covariance += da * db;
        var_a += da * da;
        var_b += db * db;
    }
    let denominator = (var_a * var_b).sqrt();
    if denominator <= f32::EPSILON {
        0.0
    } else {
        covariance / denominator
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chroma vector built straight from a key profile: the ideal case.
    fn profile_chroma(root: usize, minor: bool) -> [f32; 12] {
        let profile = if minor { &MINOR } else { &MAJOR };
        let mut chroma = [0.0f32; 12];
        for index in 0..12 {
            chroma[index] = profile[(index + 12 - root) % 12];
        }
        chroma
    }

    #[test]
    fn c_major_is_eight_b() {
        let estimate = estimate(&profile_chroma(0, false)).unwrap();
        assert_eq!(estimate.camelot, "8B");
        assert_eq!(estimate.name, "C major");
    }

    #[test]
    fn a_minor_is_eight_a() {
        // The relative minor of C major shares its notes and its Camelot number.
        let estimate = estimate(&profile_chroma(9, true)).unwrap();
        assert_eq!(estimate.camelot, "8A");
        assert_eq!(estimate.name, "A minor");
    }

    #[test]
    fn every_key_round_trips_to_itself() {
        for root in 0..12 {
            for minor in [false, true] {
                let estimate = estimate(&profile_chroma(root, minor)).unwrap();
                let expected = format!(
                    "{}{}",
                    if minor { MINOR_CAMELOT[root] } else { MAJOR_CAMELOT[root] },
                    if minor { 'A' } else { 'B' }
                );
                assert_eq!(estimate.camelot, expected, "root {root} minor {minor}");
                assert!(estimate.confidence > 0.9);
            }
        }
    }

    #[test]
    fn a_flat_chroma_has_no_key_worth_claiming() {
        let estimate = estimate(&[1.0; 12]).unwrap();
        assert!(
            estimate.confidence < 0.2,
            "confidence {} on a flat chroma",
            estimate.confidence
        );
    }

    #[test]
    fn silence_gives_nothing() {
        assert!(estimate(&[0.0; 12]).is_none());
    }
}
