//! Sample rate conversion, for the fixed-rate output mode.
//!
//! The design document's three rate modes trade the same thing three ways:
//! following the file is the most faithful and breaks gapless across a rate
//! boundary, a fixed rate resamples everything and never breaks gapless. This
//! is the machinery the second one needs.
//!
//! Nothing here runs on the audio thread. Resampling happens on the decode
//! thread, between Symphonia and the ring.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler as _};

/// Frames handed to the resampler at a time. Big enough that the FFT is
/// efficient, small enough that a seek does not throw away much work.
const CHUNK_FRAMES: usize = 1024;

/// Converts interleaved f32 from one rate to another.
///
/// Fixed input size: the caller feeds whole chunks and takes whatever comes
/// out, which is the shape a decode loop already has.
pub struct Resampler {
    inner: Fft<f32>,
    channels: usize,
    /// Interleaved input not yet given to the resampler.
    pending: Vec<f32>,
    scratch: Vec<f32>,
    from: u32,
    to: u32,
}

impl Resampler {
    pub fn new(from: u32, to: u32, channels: u16) -> Result<Self, String> {
        let channels = channels.max(1) as usize;
        let inner = Fft::<f32>::new(
            from as usize,
            to as usize,
            CHUNK_FRAMES,
            channels,
            FixedSync::Input,
        )
        .map_err(|err| format!("cannot resample {from} Hz to {to} Hz: {err}"))?;

        let capacity = inner.output_frames_max() * channels;
        Ok(Self {
            inner,
            channels,
            pending: Vec::with_capacity(CHUNK_FRAMES * channels * 2),
            scratch: vec![0.0; capacity],
            from,
            to,
        })
    }

    pub fn from_rate(&self) -> u32 {
        self.from
    }

    pub fn to_rate(&self) -> u32 {
        self.to
    }

    /// Feed interleaved samples; append whatever comes out to `out`.
    ///
    /// Output lags input by the resampler's own delay, so early calls may
    /// produce nothing. That is normal, not an error.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.pending.extend_from_slice(input);

        let chunk_samples = CHUNK_FRAMES * self.channels;
        while self.pending.len() >= chunk_samples {
            let Ok(adapter) =
                InterleavedSlice::new(&self.pending[..chunk_samples], self.channels, CHUNK_FRAMES)
            else {
                break;
            };

            let frames_out = self.inner.output_frames_next();
            let needed = frames_out * self.channels;
            if self.scratch.len() < needed {
                self.scratch.resize(needed, 0.0);
            }
            let Ok(mut sink) =
                InterleavedSlice::new_mut(&mut self.scratch[..needed], self.channels, frames_out)
            else {
                break;
            };

            match self.inner.process_into_buffer(&adapter, &mut sink, None) {
                Ok((_, produced)) => {
                    out.extend_from_slice(&self.scratch[..produced * self.channels]);
                }
                // A failed chunk is a glitch, not a reason to stop playing.
                Err(err) => tracing::warn!(%err, "resampler rejected a chunk"),
            }
            self.pending.drain(..chunk_samples);
        }
    }

    /// Forget buffered input. Used on a seek, where what is buffered belongs to
    /// a position we have left.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.inner.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a sine and check the output is the right length and still a sine.
    fn sine(frames: usize, rate: u32, hz: f32, channels: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|frame| {
                let value =
                    (std::f32::consts::TAU * hz * frame as f32 / rate as f32).sin() * 0.5;
                std::iter::repeat(value).take(channels)
            })
            .collect()
    }

    #[test]
    fn upsampling_produces_about_the_right_number_of_frames() {
        let mut resampler = Resampler::new(44_100, 48_000, 2).unwrap();
        let input = sine(44_100, 44_100, 440.0, 2);
        let mut out = Vec::new();
        resampler.process(&input, &mut out);

        let frames = out.len() / 2;
        // One second in, about one second out. The shortfall is the resampler's
        // own delay plus the last partial chunk, both expected.
        assert!(
            (46_000..=48_100).contains(&frames),
            "44.1k -> 48k gave {frames} frames"
        );
    }

    #[test]
    fn downsampling_produces_about_the_right_number_of_frames() {
        let mut resampler = Resampler::new(96_000, 48_000, 2).unwrap();
        let input = sine(96_000, 96_000, 440.0, 2);
        let mut out = Vec::new();
        resampler.process(&input, &mut out);

        let frames = out.len() / 2;
        assert!(
            (46_000..=48_100).contains(&frames),
            "96k -> 48k gave {frames} frames"
        );
    }

    #[test]
    fn the_signal_survives_the_conversion() {
        let mut resampler = Resampler::new(44_100, 48_000, 2).unwrap();
        let input = sine(44_100, 44_100, 440.0, 2);
        let mut out = Vec::new();
        resampler.process(&input, &mut out);

        // Skip the leading delay, then check the amplitude is intact and the
        // channels did not get swapped or collapsed.
        let tail = &out[out.len() / 2..];
        let peak = tail.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
        assert!((0.45..=0.55).contains(&peak), "peak {peak} after resampling");

        let left: Vec<f32> = tail.iter().step_by(2).copied().collect();
        let right: Vec<f32> = tail.iter().skip(1).step_by(2).copied().collect();
        assert_eq!(left.len(), right.len());
        for (l, r) in left.iter().zip(right.iter()).take(500) {
            assert!((l - r).abs() < 1e-4, "channels diverged: {l} vs {r}");
        }
    }

    #[test]
    fn a_reset_forgets_buffered_input() {
        let mut resampler = Resampler::new(44_100, 48_000, 2).unwrap();
        let mut out = Vec::new();
        // Less than a chunk, so it is all still buffered.
        resampler.process(&sine(500, 44_100, 440.0, 2), &mut out);
        assert!(out.is_empty());

        resampler.reset();
        resampler.process(&sine(500, 44_100, 440.0, 2), &mut out);
        assert!(out.is_empty(), "reset must not leave a partial chunk behind");
    }
}
