//! The control and decode threads, and the commands that drive them.
//!
//! Three participants, as laid out in the design document: the control thread
//! owns the queue and the output stream, the decode thread owns the open file
//! and feeds the ring, and the audio callback drains it. They share only
//! atomics and the ring itself.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rtrb::Producer;
use serde::{Deserialize, Serialize};

use crate::backend::CpalBackend;
use crate::decode::TrackDecoder;
use crate::device::{AudioBackend, DeviceInfo, OutputStream, StreamRequest};
use crate::ring::{self, PlaybackShared};

/// How long the decode thread will wait for the callback to acknowledge a seek
/// before writing anyway. Only reached if the stream is not running, in which
/// case nothing is draining the ring and there is nothing stale to protect.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(200);

/// How long the control thread will wait for the decoder to put something in
/// the ring before starting the device. Decoding runs at hundreds of times
/// realtime, so this is normally a couple of milliseconds -- well inside the
/// "sound in under 100ms" rule.
const PRIME_TIMEOUT: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub track_id: i64,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    /// Wrap round to the start of the queue.
    All,
    /// Repeat the current track.
    One,
}

pub enum Command {
    SetQueue { items: Vec<QueueItem>, start: usize },
    Play,
    Pause,
    TogglePlay,
    Stop,
    Next,
    Previous,
    Seek { ms: u64 },
    SetVolume(f32),
    SetRepeat(RepeatMode),
    SetShuffle(bool),
    Shutdown,
}

/// What the file is, straight from the stream. The basis of the phase 5 panel.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSnapshot {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// Null for lossy codecs, which have no bit depth at all.
    pub bits_per_sample: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerState {
    pub playing: bool,
    pub track_id: Option<i64>,
    pub queue: Vec<i64>,
    pub queue_index: usize,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub volume: f32,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    pub source: Option<SourceSnapshot>,
    pub device: Option<String>,
    /// What the device is running at, which is not always the file's rate.
    pub device_sample_rate: Option<u32>,
    /// Non-zero means the decoder fell behind and the device ran dry.
    pub underruns: u64,
    pub error: Option<String>,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            playing: false,
            track_id: None,
            queue: Vec::new(),
            queue_index: 0,
            position_ms: 0,
            duration_ms: 0,
            volume: 1.0,
            repeat: RepeatMode::Off,
            shuffle: false,
            source: None,
            device: None,
            device_sample_rate: None,
            underruns: 0,
            error: None,
        }
    }
}

/// Handle to a running engine. Cheap to clone the sender side.
pub struct Engine {
    commands: Sender<Command>,
    shared: Arc<PlaybackShared>,
    state: Arc<Mutex<PlayerState>>,
    /// The rate the current stream runs at, for turning frames into milliseconds.
    sample_rate: Arc<AtomicU32>,
}

impl Engine {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(PlaybackShared::new());
        let state = Arc::new(Mutex::new(PlayerState::default()));
        let sample_rate = Arc::new(AtomicU32::new(44_100));

        // Built inside the thread, not moved into it: the output stream handle
        // is not Send on macOS, so whichever thread opens one must own it.
        std::thread::Builder::new()
            .name("dubplate-control".into())
            .spawn({
                let shared = Arc::clone(&shared);
                let state = Arc::clone(&state);
                let sample_rate = Arc::clone(&sample_rate);
                move || Control::new(rx, shared, state, sample_rate).run()
            })
            .expect("spawning the control thread");

        Self {
            commands: tx,
            shared,
            state,
            sample_rate,
        }
    }

    pub fn send(&self, command: Command) {
        // A closed channel means the control thread is gone, which only happens
        // during shutdown.
        let _ = self.commands.send(command);
    }

    /// Current state, with position read live from the callback's counter.
    ///
    /// Position never comes from the decoder: it runs ahead by whatever the
    /// ring holds, so asking it where we are is always wrong by 150ms.
    pub fn snapshot(&self) -> PlayerState {
        let mut state = self.state.lock().map(|s| s.clone()).unwrap_or_default();
        let rate = self.sample_rate.load(Ordering::Relaxed).max(1) as u64;
        state.position_ms = self.shared.frames_played() * 1000 / rate;
        state.underruns = self.shared.underruns();
        state
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.send(Command::Shutdown);
    }
}

// ── decode thread ───────────────────────────────────────────────────────────

enum DecodeCommand {
    /// A new track. `producer` is `Some` only when the ring was rebuilt, which
    /// happens when the output stream had to change rate or channel count.
    Play {
        decoder: Box<TrackDecoder>,
        producer: Option<Producer<f32>>,
        generation: u64,
    },
    Seek {
        frames: u64,
        generation: u64,
    },
    Stop,
    Shutdown,
}

enum DecodeEvent {
    /// The track played to its end. Not an error.
    Finished,
    Failed(String),
}

/// Owns the open file and keeps the ring fed. Never blocks on the callback.
fn decode_thread(rx: Receiver<DecodeCommand>, tx: Sender<DecodeEvent>, shared: Arc<PlaybackShared>) {
    let mut decoder: Option<Box<TrackDecoder>> = None;
    let mut producer: Option<Producer<f32>> = None;
    // Samples decoded but not yet written, because the ring was full.
    let mut pending: Vec<f32> = Vec::new();
    let mut pending_at = 0usize;

    loop {
        match rx.try_recv() {
            Ok(DecodeCommand::Play {
                decoder: new_decoder,
                producer: new_producer,
                generation,
            }) => {
                decoder = Some(new_decoder);
                if let Some(new_producer) = new_producer {
                    producer = Some(new_producer);
                }
                pending.clear();
                pending_at = 0;
                await_drain(&shared, generation);
            }
            Ok(DecodeCommand::Seek { frames, generation }) => {
                if let Some(decoder) = decoder.as_mut() {
                    if let Err(err) = decoder.seek(frames) {
                        let _ = tx.send(DecodeEvent::Failed(err.to_string()));
                    }
                }
                pending.clear();
                pending_at = 0;
                // Do not write until the callback has thrown away the audio
                // belonging to where we were, or it will discard this too.
                await_drain(&shared, generation);
            }
            Ok(DecodeCommand::Stop) => {
                decoder = None;
                pending.clear();
                pending_at = 0;
            }
            Ok(DecodeCommand::Shutdown) | Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {}
        }

        let (Some(active), Some(sink)) = (decoder.as_mut(), producer.as_mut()) else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };

        let capacity = sink.buffer().capacity();
        shared.set_buffered_frames((capacity - sink.slots()) as u64);

        let free = sink.slots();
        if free == 0 {
            // The ring is full, which is exactly where a healthy decoder sits.
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        if pending_at < pending.len() {
            let take = free.min(pending.len() - pending_at);
            for sample in &pending[pending_at..pending_at + take] {
                let _ = sink.push(*sample);
            }
            pending_at += take;
            continue;
        }

        match active.next_block() {
            Ok(Some(block)) => {
                pending.clear();
                pending.extend_from_slice(block);
                pending_at = 0;
            }
            Ok(None) => {
                decoder = None;
                let _ = tx.send(DecodeEvent::Finished);
            }
            Err(err) => {
                decoder = None;
                let _ = tx.send(DecodeEvent::Failed(err.to_string()));
            }
        }
    }
}

/// Wait for the callback to acknowledge the new generation.
///
/// Bounded: if the stream is not running, nothing is draining the ring, and
/// there is no stale audio to protect anyway.
fn await_drain(shared: &PlaybackShared, generation: u64) {
    let deadline = Instant::now() + DRAIN_TIMEOUT;
    while shared.drained_generation() < generation && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
}

// ── control thread ──────────────────────────────────────────────────────────

struct Control {
    commands: Receiver<Command>,
    shared: Arc<PlaybackShared>,
    state: Arc<Mutex<PlayerState>>,
    sample_rate: Arc<AtomicU32>,

    backend: CpalBackend,
    device: Option<DeviceInfo>,
    stream: Option<Box<dyn OutputStream>>,

    decode_tx: Sender<DecodeCommand>,
    decode_rx: Receiver<DecodeEvent>,

    queue: Vec<QueueItem>,
    /// Play order. The identity permutation unless shuffle is on.
    order: Vec<usize>,
    cursor: usize,
    repeat: RepeatMode,
    shuffle: bool,
    volume: f32,
    rng: u64,
}

impl Control {
    fn new(
        commands: Receiver<Command>,
        shared: Arc<PlaybackShared>,
        state: Arc<Mutex<PlayerState>>,
        sample_rate: Arc<AtomicU32>,
    ) -> Self {
        let (decode_tx, decode_command_rx) = mpsc::channel();
        let (decode_event_tx, decode_rx) = mpsc::channel();
        let decode_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("dubplate-decode".into())
            .spawn(move || decode_thread(decode_command_rx, decode_event_tx, decode_shared))
            .expect("spawning the decode thread");

        Self {
            commands,
            shared,
            state,
            sample_rate,
            backend: CpalBackend::new(),
            device: None,
            stream: None,
            decode_tx,
            decode_rx,
            queue: Vec::new(),
            order: Vec::new(),
            cursor: 0,
            repeat: RepeatMode::Off,
            shuffle: false,
            volume: 1.0,
            rng: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x2545F4914F6CDD1D)
                | 1,
        }
    }

    fn run(mut self) {
        loop {
            match self.commands.recv_timeout(Duration::from_millis(50)) {
                Ok(Command::Shutdown) => break,
                Ok(command) => self.handle(command),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }

            while let Ok(event) = self.decode_rx.try_recv() {
                match event {
                    DecodeEvent::Finished => self.on_track_finished(),
                    DecodeEvent::Failed(message) => {
                        self.set_error(Some(message));
                        self.advance(1, false);
                    }
                }
            }
            self.publish();
        }
        let _ = self.decode_tx.send(DecodeCommand::Shutdown);
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::SetQueue { items, start } => {
                self.queue = items;
                self.rebuild_order(start.min(self.queue.len().saturating_sub(1)));
                if !self.queue.is_empty() {
                    self.start_current(0);
                }
            }
            Command::Play => {
                if self.stream.is_some() {
                    self.resume();
                } else if !self.queue.is_empty() {
                    self.start_current(0);
                }
            }
            Command::Pause => self.pause(),
            Command::TogglePlay => {
                if self.shared.is_playing() {
                    self.pause();
                } else {
                    self.handle(Command::Play);
                }
            }
            Command::Stop => {
                self.shared.set_playing(false);
                let _ = self.decode_tx.send(DecodeCommand::Stop);
                self.stream = None;
                self.set_source(None);
            }
            Command::Next => self.advance(1, true),
            Command::Previous => {
                // Restart the track first, like every other player: only jump
                // back if you press it again near the beginning.
                if self.position_ms() > 3_000 {
                    self.seek_ms(0);
                } else {
                    self.advance(-1, true);
                }
            }
            Command::Seek { ms } => self.seek_ms(ms),
            Command::SetVolume(volume) => {
                self.volume = volume.clamp(0.0, 1.0);
                self.shared.set_target_gain(self.volume);
            }
            Command::SetRepeat(mode) => self.repeat = mode,
            Command::SetShuffle(on) => {
                self.shuffle = on;
                let current = self.current_index().unwrap_or(0);
                self.rebuild_order(current);
            }
            Command::Shutdown => {}
        }
    }

    fn current_index(&self) -> Option<usize> {
        self.order.get(self.cursor).copied()
    }

    fn rebuild_order(&mut self, current: usize) {
        self.order = (0..self.queue.len()).collect();
        if self.shuffle && !self.order.is_empty() {
            // Fisher-Yates, then bring the playing track to the front so
            // toggling shuffle does not interrupt what is playing.
            for i in (1..self.order.len()).rev() {
                let j = (self.next_random() % (i as u64 + 1)) as usize;
                self.order.swap(i, j);
            }
            if let Some(at) = self.order.iter().position(|index| *index == current) {
                self.order.swap(0, at);
            }
            self.cursor = 0;
        } else {
            self.cursor = current.min(self.order.len().saturating_sub(1));
        }
    }

    fn next_random(&mut self) -> u64 {
        // xorshift64*: enough for shuffling a playlist, and no dependency.
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        self.rng.wrapping_mul(0x2545F4914F6CDD1D)
    }

    fn on_track_finished(&mut self) {
        match self.repeat {
            RepeatMode::One => self.start_current(0),
            _ => self.advance(1, false),
        }
    }

    /// Move by `step` in the play order. `manual` distinguishes pressing next
    /// from a track ending, which matters at the end of the queue.
    fn advance(&mut self, step: i64, manual: bool) {
        if self.queue.is_empty() {
            return;
        }
        let length = self.order.len() as i64;
        let next = self.cursor as i64 + step;

        let wrapped = if next < 0 {
            if manual || self.repeat == RepeatMode::All {
                length - 1
            } else {
                return;
            }
        } else if next >= length {
            if self.repeat == RepeatMode::All || manual {
                0
            } else {
                // End of the queue with repeat off: stop rather than loop.
                self.shared.set_playing(false);
                self.publish();
                return;
            }
        } else {
            next
        };

        self.cursor = wrapped as usize;
        self.start_current(0);
    }

    fn start_current(&mut self, start_frames: u64) {
        let Some(index) = self.current_index() else {
            return;
        };
        let Some(item) = self.queue.get(index).cloned() else {
            return;
        };

        let decoder = match TrackDecoder::open(Path::new(&item.path)) {
            Ok(decoder) => decoder,
            Err(err) => {
                self.set_error(Some(err.to_string()));
                return;
            }
        };
        let format = decoder.format().clone();

        // The stream follows the file's rate. Phase 5 adds the fixed-rate and
        // follow-album modes, with a resampler; until then CoreAudio converts in
        // shared mode, which the signal path panel will have to report honestly.
        let needs_stream = match self.stream.as_ref() {
            Some(stream) => {
                stream.info().sample_rate != format.sample_rate
                    || stream.info().channels != format.channels
            }
            None => true,
        };

        let generation = self.shared.begin_seek(start_frames);
        let producer = if needs_stream {
            match self.open_stream(format.sample_rate, format.channels) {
                Ok(producer) => Some(producer),
                Err(err) => {
                    self.set_error(Some(err));
                    return;
                }
            }
        } else {
            None
        };

        self.sample_rate
            .store(format.sample_rate, Ordering::Relaxed);
        self.shared.set_target_gain(self.volume);

        let duration_ms = format
            .duration()
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let source = SourceSnapshot {
            codec: format.codec.clone(),
            sample_rate: format.sample_rate,
            channels: format.channels,
            bits_per_sample: format.bits_per_sample,
        };

        let _ = self.decode_tx.send(DecodeCommand::Play {
            decoder: Box::new(decoder),
            producer,
            generation,
        });

        // Start the device only once there is audio to give it. Starting into
        // an empty ring is an underrun by construction, and it is audible as a
        // click at the beginning of every track.
        self.await_prime(format.sample_rate, format.channels);

        if let Some(stream) = self.stream.as_mut() {
            let _ = stream.play();
        }
        self.shared.set_playing(true);

        if let Ok(mut state) = self.state.lock() {
            state.track_id = Some(item.track_id);
            state.duration_ms = duration_ms;
            state.source = Some(source);
            state.error = None;
        }
    }

    fn open_stream(&mut self, sample_rate: u32, channels: u16) -> Result<Producer<f32>, String> {
        // Drop the old stream before opening a new one: two streams on the same
        // device is a good way to get neither.
        self.stream = None;

        let device = match self.device.clone() {
            Some(device) => device,
            None => {
                let device = self.backend.default_device().map_err(|e| e.to_string())?;
                self.device = Some(device.clone());
                device
            }
        };

        let (producer, renderer) = ring::open(sample_rate, channels, Arc::clone(&self.shared));
        let stream = self
            .backend
            .open(
                &device.id,
                StreamRequest {
                    sample_rate,
                    channels,
                    buffer_frames: None,
                },
                Box::new(renderer),
            )
            .map_err(|e| e.to_string())?;

        if let Ok(mut state) = self.state.lock() {
            state.device = Some(stream.info().device.name.clone());
            state.device_sample_rate = Some(stream.info().sample_rate);
        }
        self.stream = Some(stream);
        Ok(producer)
    }

    /// Wait until the ring holds enough to survive the first few callbacks.
    fn await_prime(&self, sample_rate: u32, channels: u16) {
        let _ = channels;
        let target = (ring::ring_capacity_frames(sample_rate) / 2) as u64;
        let deadline = Instant::now() + PRIME_TIMEOUT;
        while self.shared.buffered_frames() < target && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn resume(&mut self) {
        if let Some(stream) = self.stream.as_mut() {
            let _ = stream.play();
        }
        self.shared.set_target_gain(self.volume);
        self.shared.set_playing(true);
    }

    fn pause(&mut self) {
        // Leave the stream running so the gain ramp can fade out rather than
        // cutting; the callback stops pulling once it reaches silence.
        self.shared.set_playing(false);
    }

    fn seek_ms(&mut self, ms: u64) {
        let rate = self.sample_rate.load(Ordering::Relaxed).max(1) as u64;
        let frames = ms * rate / 1000;
        let generation = self.shared.begin_seek(frames);
        let _ = self.decode_tx.send(DecodeCommand::Seek { frames, generation });
    }

    fn position_ms(&self) -> u64 {
        let rate = self.sample_rate.load(Ordering::Relaxed).max(1) as u64;
        self.shared.frames_played() * 1000 / rate
    }

    fn set_error(&mut self, message: Option<String>) {
        if let Ok(mut state) = self.state.lock() {
            state.error = message;
        }
    }

    fn set_source(&mut self, source: Option<SourceSnapshot>) {
        if let Ok(mut state) = self.state.lock() {
            state.source = source;
            if source_is_none(&state.source) {
                state.track_id = None;
                state.duration_ms = 0;
            }
        }
    }

    fn publish(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.playing = self.shared.is_playing();
            state.queue = self.queue.iter().map(|item| item.track_id).collect();
            state.queue_index = self.current_index().unwrap_or(0);
            state.volume = self.volume;
            state.repeat = self.repeat;
            state.shuffle = self.shuffle;
            state.underruns = self.shared.underruns();
        }
    }
}

fn source_is_none(source: &Option<SourceSnapshot>) -> bool {
    source.is_none()
}

/// Where the engine keeps its own copy of a path, for restoring a queue.
pub fn queue_item(track_id: i64, path: impl Into<PathBuf>) -> QueueItem {
    QueueItem {
        track_id,
        path: path.into().to_string_lossy().into_owned(),
    }
}
