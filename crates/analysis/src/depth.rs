//! Is this "hi-res" file actually hi-res?
//!
//! A lot of 24-bit downloads are 16-bit content sitting in a 24-bit container.
//! Nothing about them is wrong, exactly -- the audio is what it is -- but a
//! player that shows "24 bit" is telling you something the file does not
//! support. Checking is cheap: if the low 8 bits are zero in every sample, the
//! content is 16-bit.

/// Recovers integer sample values from the decoder's floats and watches which
/// low bits are ever set.
pub struct DepthProbe {
    bits: u32,
    scale: f32,
    /// Every magnitude seen, OR'd together. Its trailing zeros are exactly the
    /// low bits that no sample in the file ever used.
    seen: u32,
    samples: u64,
}

impl DepthProbe {
    /// `None` when the question does not apply or cannot be answered.
    ///
    /// Lossy codecs have no bit depth at all. Above 24 bits the check stops
    /// being sound: f32 carries a 24-bit mantissa, so the integer cannot be
    /// recovered exactly and the low bits would be rounding noise rather than
    /// evidence.
    pub fn new(bits_per_sample: Option<u32>) -> Option<Self> {
        let bits = bits_per_sample?;
        if !(2..=24).contains(&bits) {
            return None;
        }
        Some(Self {
            bits,
            scale: (1u32 << (bits - 1)) as f32,
            seen: 0,
            samples: 0,
        })
    }

    pub fn feed(&mut self, samples: &[f32]) {
        for sample in samples {
            // Exact for 24 bits and below: the decoder divided by this same
            // scale, and f32 holds the result without loss.
            let value = (sample * self.scale).round();
            if !value.is_finite() {
                continue;
            }
            self.seen |= (value.abs() as u32) & u32::MAX;
        }
        self.samples += samples.len() as u64;
    }

    /// Bits the file actually uses, or `None` if there was nothing to judge.
    pub fn effective_bits(&self) -> Option<u32> {
        // Digital silence sets no bits at all, so it says nothing about depth.
        if self.samples == 0 || self.seen == 0 {
            return None;
        }
        let unused = self.seen.trailing_zeros();
        Some(self.bits.saturating_sub(unused).max(1))
    }

    pub fn declared_bits(&self) -> u32 {
        self.bits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Turn integer sample values into the floats a decoder would produce.
    fn as_floats(values: &[i32], bits: u32) -> Vec<f32> {
        let scale = (1u32 << (bits - 1)) as f32;
        values.iter().map(|v| *v as f32 / scale).collect()
    }

    #[test]
    fn sixteen_bit_content_in_a_twenty_four_bit_container_is_caught() {
        let mut probe = DepthProbe::new(Some(24)).unwrap();
        // 16-bit values shifted up into a 24-bit range: the low 8 bits are
        // always zero, which is exactly what padding looks like.
        let values: Vec<i32> = (1..500).map(|n| (n * 37 % 32768) << 8).collect();
        probe.feed(&as_floats(&values, 24));
        assert_eq!(probe.effective_bits(), Some(16));
    }

    #[test]
    fn genuine_twenty_four_bit_content_is_left_alone() {
        let mut probe = DepthProbe::new(Some(24)).unwrap();
        // i64 while generating: the products overshoot i32 long before the
        // modulo brings them back into range.
        let values: Vec<i32> = (1..500i64)
            .map(|n| (n * 8_388_593 % 8_388_607) as i32)
            .collect();
        probe.feed(&as_floats(&values, 24));
        assert_eq!(probe.effective_bits(), Some(24));
    }

    #[test]
    fn one_dithered_sample_is_enough_to_prove_it_is_not_padded() {
        let mut probe = DepthProbe::new(Some(24)).unwrap();
        let mut values: Vec<i32> = (1..500).map(|n| (n * 37 % 32768) << 8).collect();
        // A single sample using the bottom bit means the file is not padded,
        // however much of it looks like it is.
        values.push((1234 << 8) | 1);
        probe.feed(&as_floats(&values, 24));
        assert_eq!(probe.effective_bits(), Some(24));
    }

    #[test]
    fn silence_gives_no_answer_rather_than_a_wrong_one() {
        let mut probe = DepthProbe::new(Some(24)).unwrap();
        probe.feed(&vec![0.0; 1000]);
        assert_eq!(probe.effective_bits(), None);
    }

    #[test]
    fn lossy_and_oversized_depths_are_not_judged() {
        // MP3 and friends have no bit depth to check.
        assert!(DepthProbe::new(None).is_none());
        // Above 24 bits the float cannot carry the integer exactly, so the low
        // bits would be rounding noise rather than evidence.
        assert!(DepthProbe::new(Some(32)).is_none());
    }
}
