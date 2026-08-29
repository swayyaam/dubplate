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

/// Room for a handful of queued track changes. Only one can be pending in
/// practice, since the decoder pre-rolls exactly one track ahead.
const BOUNDARY_SLOTS: usize = 8;

/// Marks the frame where one track ends and the next begins inside the ring.
///
/// Gapless means both tracks' audio is already interleaved in the same buffer,
/// so the moment playback crosses into the next track is a position in the
/// ring, not an event the decoder can announce. Announcing it when the decoder
/// switches would flip "now playing" up to 150ms early -- the length of the
/// buffer still waiting to be heard.
#[derive(Debug, Clone, Copy)]
pub struct Boundary {
    /// Ignored if it does not match the callback's generation: a seek
    /// invalidates any boundary queued before it.
    pub generation: u64,
    /// Frames into this generation at which the new track starts.
    pub at_frame: u64,
    /// Matches the control thread's record of which track this is.
    pub seq: u64,
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
    /// Frames sitting in the ring, published by the decode thread so the
    /// control thread can wait for a buffer before starting the device.
    buffered_frames: AtomicU64,
    /// The most recent boundary the callback has actually played through.
    boundary_seq: AtomicU64,
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
            buffered_frames: AtomicU64::new(0),
            boundary_seq: AtomicU64::new(0),
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

    pub fn set_buffered_frames(&self, frames: u64) {
        self.buffered_frames.store(frames, Ordering::Relaxed);
    }

    /// How much audio is queued. Used to hold the device closed until there is
    /// something to play, rather than starting it into an empty ring.
    pub fn buffered_frames(&self) -> u64 {
        self.buffered_frames.load(Ordering::Relaxed)
    }

    /// The last gapless track change the listener has actually reached.
    pub fn boundary_seq(&self) -> u64 {
        self.boundary_seq.load(Ordering::Acquire)
    }
}

/// Create the ring and the renderer that drains it.
pub fn open(
    sample_rate: u32,
    channels: u16,
    shared: Arc<PlaybackShared>,
) -> (Producer<f32>, Producer<Boundary>, RingRenderer) {
    let capacity = ring_capacity_frames(sample_rate) * channels as usize;
    let (producer, consumer) = RingBuffer::<f32>::new(capacity);
    let (boundary_tx, boundary_rx) = RingBuffer::<Boundary>::new(BOUNDARY_SLOTS);
    // A brand new ring is already in the state the generation-change path would
    // put it in: empty, and starting at the seek target. Say so explicitly.
    //
    // Doing this by pretending to be a generation behind would work too, but it
    // would make the first callback discard whatever is in the ring -- harmless
    // today only because the decoder waits for the acknowledgement below before
    // writing. Setting the two values directly has no such dependency.
    let generation = shared.generation();
    shared
        .frames_played
        .store(shared.seek_target_frames(), Ordering::Relaxed);
    shared
        .drained_generation
        .store(generation, Ordering::Release);

    let renderer = RingRenderer {
        consumer,
        boundary_rx,
        next_boundary: None,
        channels: channels.max(1) as usize,
        consumed_frames: 0,
        position: shared.seek_target_frames(),
        gain: GainRamp::new(0.0, sample_rate, RAMP_MS),
        local_generation: generation,
        shared,
    };
    (producer, boundary_tx, renderer)
}

/// Drains the ring, applies the gain ramp, and reports position. This runs on
/// the realtime thread and does nothing else.
pub struct RingRenderer {
    consumer: Consumer<f32>,
    boundary_rx: Consumer<Boundary>,
    next_boundary: Option<Boundary>,
    channels: usize,
    /// Frames taken from the ring in this generation, including discarded ones,
    /// so it stays aligned with the decoder's count of frames written.
    consumed_frames: u64,
    /// Position within the track currently being heard.
    position: u64,
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

    /// Take the next boundary belonging to the current generation, dropping any
    /// left over from before a seek.
    fn take_boundary(&mut self) {
        while self.next_boundary.is_none() {
            match self.boundary_rx.pop() {
                Ok(boundary) if boundary.generation == self.local_generation => {
                    self.next_boundary = Some(boundary)
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    }
}

impl Renderer for RingRenderer {
    fn render(&mut self, output: &mut [f32], channels: u16) {
        let channels = channels.max(1) as usize;
        self.channels = channels;

        // A seek happened. Everything queued predates it, so it goes -- and the
        // frame counters on both sides restart from zero together, which is what
        // keeps boundary positions meaningful.
        let generation = self.shared.generation();
        if generation != self.local_generation {
            self.discard_all();
            self.local_generation = generation;
            self.consumed_frames = 0;
            self.position = self.shared.seek_target_frames();
            self.next_boundary = None;
            while self.boundary_rx.pop().is_ok() {}
            self.shared
                .frames_played
                .store(self.position, Ordering::Relaxed);
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

        // Fetched before the ring is borrowed below. At most one track change
        // can fall inside one device buffer, which is a few milliseconds.
        self.take_boundary();
        let boundary = self.next_boundary;
        let mut crossed = None;

        let frames_wanted = output.len() / channels;
        let frames_available = self.consumer.slots() / channels;
        let frames = frames_wanted.min(frames_available);

        let mut written = 0usize;
        if frames > 0 {
            if let Ok(chunk) = self.consumer.read_chunk(frames * channels) {
                let (first, second) = chunk.as_slices();
                let mut source = first.iter().chain(second.iter());
                for _ in 0..frames {
                    if let Some(boundary) = boundary {
                        if self.consumed_frames == boundary.at_frame {
                            // This frame is the first of the next track, and it
                            // is being heard now rather than merely decoded.
                            self.position = 0;
                            crossed = Some(boundary.seq);
                        }
                    }
                    let gain = self.gain.next(target);
                    for _ in 0..channels {
                        let sample = source.next().copied().unwrap_or(0.0);
                        output[written] = sample * gain;
                        written += 1;
                    }
                    self.consumed_frames += 1;
                    self.position += 1;
                }
                chunk.commit_all();
            }
        }

        if let Some(seq) = crossed {
            self.next_boundary = None;
            self.shared.boundary_seq.store(seq, Ordering::Release);
        }

        if written < output.len() {
            output[written..].fill(0.0);
            // Silence while paused is expected; silence while playing is not.
            if playing {
                self.shared.underruns.fetch_add(1, Ordering::Relaxed);
            }
        }

        self.shared
            .frames_played
            .store(self.position, Ordering::Relaxed);
    }
}
