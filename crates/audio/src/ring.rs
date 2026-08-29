//! The lock-free path from the decoder to the device.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::device::Renderer;
use crate::gain::{GainRamp, RAMP_MS};

/// How much audio the ring holds. Enough that a slow decode or a scheduling
/// hiccup cannot starve the device, small enough that a seek does not have to
/// throw away a noticeable amount of work.
pub const RING_MS: u32 = 150;

pub fn ring_capacity_frames(sample_rate: u32) -> usize {
    (sample_rate as usize * RING_MS as usize).div_ceil(1000)
}

/// State the control thread, the decoder and the audio callback all see.
///
/// Every field is an atomic. The callback may never take a lock, so this is the
/// only way the three of them are allowed to talk.
#[derive(Debug)]
pub struct PlaybackShared {
    /// Bumped by the control thread on every seek and track change.
    generation: AtomicU64,
    /// Where the new generation starts. Written before `generation` is bumped.
    seek_target_frames: AtomicU64,
    /// Set by the callback once it has discarded everything older.
    drained_generation: AtomicU64,
    /// Frames handed to the device. The only honest source of playback position.
    frames_played: AtomicU64,
    /// Linear gain, as f32 bits. Volume multiplied by ReplayGain.
    target_gain_bits: AtomicU32,
    playing: AtomicBool,
    /// Callbacks that ran dry. Anything above zero means the decoder fell behind.
    underruns: AtomicU64,
}

impl Default for PlaybackShared {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackShared {
    pub fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            seek_target_frames: AtomicU64::new(0),
            drained_generation: AtomicU64::new(0),
            frames_played: AtomicU64::new(0),
            target_gain_bits: AtomicU32::new(1.0f32.to_bits()),
            playing: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
        }
    }

    /// Start a new generation at `frames`, invalidating everything already in
    /// the ring. Ordering matters: the target must be visible before the
    /// generation that refers to it.
    pub fn begin_seek(&self, frames: u64) -> u64 {
        self.seek_target_frames.store(frames, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn seek_target_frames(&self) -> u64 {
        self.seek_target_frames.load(Ordering::Acquire)
    }

    /// The decoder waits for this to catch up before writing post-seek audio,
    /// so the callback cannot throw away samples it has just been given.
    pub fn drained_generation(&self) -> u64 {
        self.drained_generation.load(Ordering::Acquire)
    }

    pub fn frames_played(&self) -> u64 {
        self.frames_played.load(Ordering::Relaxed)
    }

    pub fn set_target_gain(&self, gain: f32) {
        self.target_gain_bits
            .store(gain.max(0.0).to_bits(), Ordering::Relaxed);
    }

    pub fn target_gain(&self) -> f32 {
        f32::from_bits(self.target_gain_bits.load(Ordering::Relaxed))
    }

    pub fn set_playing(&self, playing: bool) {
        self.playing.store(playing, Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
}

/// Create the ring and the renderer that drains it.
pub fn open(
    sample_rate: u32,
    channels: u16,
    shared: Arc<PlaybackShared>,
) -> (Producer<f32>, RingRenderer) {
    let capacity = ring_capacity_frames(sample_rate) * channels as usize;
    let (producer, consumer) = RingBuffer::<f32>::new(capacity);
    let renderer = RingRenderer {
        consumer,
        gain: GainRamp::new(0.0, sample_rate, RAMP_MS),
        local_generation: shared.generation(),
        shared,
    };
    (producer, renderer)
}

/// Drains the ring, applies the gain ramp, and reports position. This runs on
/// the realtime thread and does nothing else.
pub struct RingRenderer {
    consumer: Consumer<f32>,
    shared: Arc<PlaybackShared>,
    gain: GainRamp,
    local_generation: u64,
}

impl RingRenderer {
    /// Throw away everything queued: it belongs to a position we have left.
    fn discard_all(&mut self) {
        let waiting = self.consumer.slots();
        if waiting > 0 {
            if let Ok(chunk) = self.consumer.read_chunk(waiting) {
                chunk.commit_all();
            }
        }
    }
}

impl Renderer for RingRenderer {
    fn render(&mut self, output: &mut [f32], channels: u16) {
        let channels = channels.max(1) as usize;

        // A seek happened. Everything queued predates it, so it goes.
        let generation = self.shared.generation();
        if generation != self.local_generation {
            self.discard_all();
            self.local_generation = generation;
            self.shared
                .frames_played
                .store(self.shared.seek_target_frames(), Ordering::Relaxed);
            // Only now may the decoder write for this generation.
            self.shared
                .drained_generation
                .store(generation, Ordering::Release);
        }

        let playing = self.shared.is_playing();
        let target = if playing { self.shared.target_gain() } else { 0.0 };

        // Faded out and paused: hold position and leave the ring untouched, so
        // resuming continues from exactly where it stopped.
        if !playing && self.gain.is_silent() {
            output.fill(0.0);
            return;
        }

        let frames_wanted = output.len() / channels;
        let frames_available = self.consumer.slots() / channels;
        let frames = frames_wanted.min(frames_available);

        let mut written = 0usize;
        if frames > 0 {
            if let Ok(chunk) = self.consumer.read_chunk(frames * channels) {
                let (first, second) = chunk.as_slices();
                let mut source = first.iter().chain(second.iter());
                for _ in 0..frames {
                    let gain = self.gain.next(target);
                    for _ in 0..channels {
                        let sample = source.next().copied().unwrap_or(0.0);
                        output[written] = sample * gain;
                        written += 1;
                    }
                }
                chunk.commit_all();
            }
        }

        if written < output.len() {
            output[written..].fill(0.0);
            // Silence while paused is expected; silence while playing is not.
            if playing {
                self.shared.underruns.fetch_add(1, Ordering::Relaxed);
            }
        }

        if frames > 0 {
            self.shared
                .frames_played
                .fetch_add(frames as u64, Ordering::Relaxed);
        }
    }
}
