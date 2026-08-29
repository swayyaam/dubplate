//! A tone generator, so the ring buffer and the audio callback can be proven
//! correct before a decoder is anywhere near them.
//!
//! Building this first is deliberate: if the output path is wrong, wiring a
//! decoder to it only makes the failure harder to see.

use crate::device::Renderer;
use crate::gain::{GainRamp, RAMP_MS};

pub struct SineRenderer {
    phase: f32,
    phase_increment: f32,
    amplitude: f32,
    gain: GainRamp,
}

impl SineRenderer {
    pub fn new(frequency: f32, sample_rate: u32, amplitude: f32) -> Self {
        Self {
            phase: 0.0,
            phase_increment: std::f32::consts::TAU * frequency / sample_rate as f32,
            amplitude,
            // Start silent and ramp in, so the very first buffer is not a step
            // from zero to full scale -- which is a click like any other.
            gain: GainRamp::new(0.0, sample_rate, RAMP_MS),
        }
    }
}

impl Renderer for SineRenderer {
    fn render(&mut self, output: &mut [f32], channels: u16) {
        let channels = channels.max(1) as usize;
        for frame in output.chunks_mut(channels) {
            let gain = self.gain.next(1.0);
            let sample = self.phase.sin() * self.amplitude * gain;
            self.phase += self.phase_increment;
            if self.phase >= std::f32::consts::TAU {
                self.phase -= std::f32::consts::TAU;
            }
            frame.fill(sample);
        }
    }
}
