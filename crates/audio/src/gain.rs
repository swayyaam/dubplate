/// Volume multiplied by ReplayGain, ramped rather than applied instantly.
///
/// A gain change of even a few dB applied between one sample and the next is a
/// step discontinuity, which is audible as a click -- "zipper noise" when a
/// slider is dragged. Sliding to the new value over a few milliseconds removes
/// it entirely and is inaudible as a fade.
#[derive(Debug, Clone)]
pub struct GainRamp {
    current: f32,
    /// Maximum change in linear gain per frame.
    step: f32,
}

/// Long enough to remove the click, short enough that pausing feels immediate.
pub const RAMP_MS: f32 = 10.0;

impl GainRamp {
    pub fn new(initial: f32, sample_rate: u32, ramp_ms: f32) -> Self {
        let frames = (ramp_ms / 1000.0 * sample_rate as f32).max(1.0);
        Self {
            current: initial,
            // Full scale traversed in `ramp_ms`, so smaller changes are quicker.
            step: 1.0 / frames,
        }
    }

    /// Advance one frame toward `target` and return the gain for that frame.
    #[inline]
    pub fn next(&mut self, target: f32) -> f32 {
        let delta = target - self.current;
        if delta.abs() <= self.step {
            self.current = target;
        } else {
            self.current += self.step.copysign(delta);
        }
        self.current
    }

    #[inline]
    pub fn current(&self) -> f32 {
        self.current
    }

    /// True once the ramp has reached zero, so the caller can stop pulling audio
    /// rather than draining the ring while inaudible.
    #[inline]
    pub fn is_silent(&self) -> bool {
        self.current <= 0.0
    }

    pub fn jump_to(&mut self, gain: f32) {
        self.current = gain;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaches_the_target_in_about_the_ramp_time() {
        let mut ramp = GainRamp::new(0.0, 48_000, RAMP_MS);
        // 10ms at 48kHz is 480 frames for a full-scale change.
        for _ in 0..480 {
            ramp.next(1.0);
        }
        assert!((ramp.current() - 1.0).abs() < 1e-6, "got {}", ramp.current());
    }

    #[test]
    fn never_steps_more_than_one_increment() {
        let mut ramp = GainRamp::new(0.0, 48_000, RAMP_MS);
        let step = 1.0 / 480.0;
        let mut previous = 0.0;
        for _ in 0..100 {
            let gain = ramp.next(1.0);
            // A jump larger than the step is exactly the click we are avoiding.
            // The slack is f32 accumulation error, not headroom in the ramp.
            assert!(
                gain - previous <= step * 1.0001,
                "stepped {} from {previous}",
                gain - previous
            );
            previous = gain;
        }
    }

    #[test]
    fn settles_exactly_rather_than_hunting() {
        let mut ramp = GainRamp::new(0.5, 48_000, RAMP_MS);
        for _ in 0..1000 {
            ramp.next(0.5);
        }
        assert_eq!(ramp.current(), 0.5);
    }
}
