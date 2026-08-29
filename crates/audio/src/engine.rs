//! The control and decode threads, and the commands that drive them.
//!
//! Three participants, as laid out in the design document: the control thread
//! owns the queue and the output stream, the decode thread owns the open file
//! and feeds the ring, and the audio callback drains it. They share only
//! atomics and the ring itself.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rtrb::Producer;
use serde::{Deserialize, Serialize};

use crate::backend::CpalBackend;
use crate::decode::TrackDecoder;
use crate::device::{AudioBackend, AudioError, DeviceFormat, DeviceInfo, OutputStream, StreamRequest};
use crate::ring::{self, Boundary, PlaybackShared};

/// How long the decode thread will wait for the callback to acknowledge a seek
/// before writing anyway. Only reached if the stream is not running, in which
/// case nothing is draining the ring and there is nothing stale to protect.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(200);

/// How long the control thread will wait for the decoder to put something in
/// the ring before starting the device. Decoding runs at hundreds of times
/// realtime, so this is normally a couple of milliseconds -- well inside the
/// "sound in under 100ms" rule.
const PRIME_TIMEOUT: Duration = Duration::from_millis(120);

/// How much of the current track must be left before the next one is opened and
/// decoded into the same ring. Long enough to absorb a slow disk, short enough
/// that skipping around does not keep opening files nobody will hear.
const PREROLL_SECONDS: f64 = 5.0;

/// A track is counted as played once half of it has been heard.
const COMPLETION_FRACTION: f64 = 0.5;

/// How often the default output device is re-checked, in control-loop ticks of
/// 50ms. cpal reports a device *disappearing* through the stream's error
/// callback, but not the system default *moving* -- plugging in headphones
/// while the interface stays connected -- so that half is polled.
const DEVICE_CHECK_TICKS: u8 = 10;

/// How long a paused player keeps exclusive access before giving the device
/// back. Long enough that pausing to answer the door does not cost a device
/// switch, short enough that nobody is left wondering why YouTube is silent.
const HOG_RELEASE_AFTER: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub track_id: i64,
    pub path: String,
    /// Used by "follow album": the rate is only changed at album boundaries.
    pub album_id: Option<i64>,
    /// ReplayGain in dB, from tags or from the analysis pass.
    pub replay_gain_db: Option<f32>,
    /// Sample peak, so the gain can be held back rather than clip.
    pub replay_gain_peak: Option<f32>,
}

/// How the output device's rate is chosen.
///
/// The three the design document lays out, and the trade between them is real:
/// switching per track is the most faithful and breaks gapless across a rate
/// boundary; a fixed rate resamples everything and never breaks gapless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "mode", content = "rate")]
pub enum RateMode {
    /// Switch the device to each track's rate. Best fidelity, small gap on a
    /// rate change. The default when exclusive mode is on.
    FollowFile,
    /// One rate for everything, resampled to fit. Gapless always works.
    Fixed(u32),
    /// Switch only at album boundaries, since tracks in an album share a rate.
    FollowAlbum,
}

impl Default for RateMode {
    fn default() -> Self {
        Self::FollowFile
    }
}

/// Per-device output settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputSettings {
    /// Take the device exclusively. Per-device opt-in and never a default: an
    /// interface deserves it, laptop speakers never do, and while it is held
    /// every other application on the machine goes silent.
    pub exclusive: bool,
    pub rate_mode: RateMode,
    pub replay_gain: bool,
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
    SetOutputSettings(OutputSettings),
    /// Move playback to the current default output device, rebuilding the ring
    /// and the stream but keeping the open file and its position.
    ///
    /// Normally driven by a device change being detected. Exposed because it is
    /// also the honest answer to "audio has got stuck", and because it is the
    /// only way to exercise the swap on a machine with one output device.
    ReopenOutput,
    Shutdown,
}

/// One finished listen, ready to be written to the history table.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayEvent {
    pub track_id: i64,
    /// The furthest point reached, not wall-clock time: seeking back and forth
    /// should not inflate how much of a track was heard.
    pub ms_played: u64,
    /// Crossed the halfway mark. Anything less counts as a skip.
    pub completed: bool,
}

/// One thing that could have altered the audio on its way out.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stage {
    pub name: String,
    /// False means grey in the panel. Three inactive lines is the point.
    pub active: bool,
    /// What it did, when it did something.
    pub detail: Option<String>,
}

/// The full signal path: what the file is, what came out of the decoder, every
/// stage that could have touched the samples, and what the hardware is running.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalPath {
    pub source: SourceSnapshot,
    /// What Symphonia produced. Always 32-bit float: that is what decoders emit.
    pub decoder_sample_rate: u32,
    pub decoder_format: String,
    pub processing: Vec<Stage>,
    pub device_name: Option<String>,
    /// Read back from the hardware, not echoed from the request. `None` when it
    /// could not be read, which is reported as unknown rather than assumed.
    pub device_format: Option<DeviceFormatView>,
    pub exclusive: bool,
    /// Green only when nothing altered the audio and the device rate matches.
    pub bit_perfect: bool,
    /// How many processing stages fired, for the "altered, N stages" badge.
    pub altered_stages: usize,
    /// Why it is not bit-perfect, in one line.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceFormatView {
    pub sample_rate: u32,
    pub bits_per_channel: u32,
    pub channels: u32,
    pub sample_format: String,
}

impl From<&DeviceFormat> for DeviceFormatView {
    fn from(format: &DeviceFormat) -> Self {
        Self {
            sample_rate: format.sample_rate,
            bits_per_channel: format.bits_per_channel,
            channels: format.channels,
            sample_format: format.sample_format.clone(),
        }
    }
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
    pub settings: OutputSettings,
    pub signal: Option<SignalPath>,
    /// Rates the current device will accept, for the settings UI.
    pub device_rates: Vec<u32>,
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
            settings: OutputSettings::default(),
            signal: None,
            device_rates: Vec::new(),
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
    plays: Arc<Mutex<Vec<PlayEvent>>>,
    /// Joined on drop. Quitting must not race the control thread: it is the
    /// thing that hands the device back, and a process that exits still holding
    /// one leaves the machine silent for everything else.
    control: Option<std::thread::JoinHandle<()>>,
}

impl Engine {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(PlaybackShared::new());
        let state = Arc::new(Mutex::new(PlayerState::default()));
        let sample_rate = Arc::new(AtomicU32::new(44_100));
        let plays = Arc::new(Mutex::new(Vec::new()));

        // Built inside the thread, not moved into it: the output stream handle
        // is not Send on macOS, so whichever thread opens one must own it.
        let control = std::thread::Builder::new()
            .name("dubplate-control".into())
            .spawn({
                let shared = Arc::clone(&shared);
                let state = Arc::clone(&state);
                let sample_rate = Arc::clone(&sample_rate);
                let plays = Arc::clone(&plays);
                move || Control::new(rx, shared, state, sample_rate, plays).run()
            })
            .expect("spawning the control thread");

        Self {
            commands: tx,
            shared,
            state,
            sample_rate,
            plays,
            control: Some(control),
        }
    }

    /// Take the listens finished since the last call, for the history table.
    /// Draining rather than reading means nothing is counted twice.
    pub fn take_play_events(&self) -> Vec<PlayEvent> {
        self.plays
            .lock()
            .map(|mut plays| std::mem::take(&mut *plays))
            .unwrap_or_default()
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
        // Wait for it. The control thread releases exclusive access and puts
        // the device's rate back on the way out, and none of that happens if
        // the process exits first.
        if let Some(handle) = self.control.take() {
            let _ = handle.join();
        }
    }
}

// ── decode thread ───────────────────────────────────────────────────────────

enum DecodeCommand {
    /// A new track, starting now. `sinks` is `Some` only when the ring was
    /// rebuilt: a rate change, a channel-count change, or a device switch.
    Play {
        decoder: Box<TrackDecoder>,
        sinks: Option<(Producer<f32>, Producer<Boundary>)>,
        generation: u64,
    },
    /// The next track, decoded into the same ring behind the current one so
    /// there is no gap when the current one ends.
    Preroll { decoder: Box<TrackDecoder>, seq: u64 },
    /// Nothing follows the current track; let it end.
    NoFollowUp,
    Seek {
        frames: u64,
        generation: u64,
    },
    /// Keep decoding the same file into a new ring, after a device change.
    Rebind {
        sinks: (Producer<f32>, Producer<Boundary>),
        frames: u64,
        generation: u64,
    },
    Stop,
    Shutdown,
}

enum DecodeEvent {
    /// The current track is nearly decoded; send the next one to pre-roll.
    NeedNext,
    /// The queue ran out with nothing pre-rolled. Not an error.
    Finished,
    Failed(String),
}

/// Owns the open files and keeps the ring fed. Never blocks on the callback.
///
/// Holds two decoders once a track is nearly through: the one playing and the
/// one behind it. Both write into the same ring, and the moment the listener
/// crosses from one to the other is marked with a [`Boundary`] rather than
/// announced when the decoder switches -- which would be up to 150ms early.
fn decode_thread(rx: Receiver<DecodeCommand>, tx: Sender<DecodeEvent>, shared: Arc<PlaybackShared>) {
    let mut current: Option<Box<TrackDecoder>> = None;
    let mut queued: Option<(Box<TrackDecoder>, u64)> = None;
    let mut audio: Option<Producer<f32>> = None;
    let mut boundaries: Option<Producer<Boundary>> = None;
    // Samples decoded but not yet written, because the ring was full.
    let mut pending: Vec<f32> = Vec::new();
    let mut pending_at = 0usize;
    let mut frames_written = 0u64;
    let mut generation = 0u64;
    let mut asked_for_next = false;

    loop {
        match rx.try_recv() {
            Ok(DecodeCommand::Play {
                decoder,
                sinks,
                generation: new_generation,
            }) => {
                current = Some(decoder);
                queued = None;
                asked_for_next = false;
                if let Some((new_audio, new_boundaries)) = sinks {
                    audio = Some(new_audio);
                    boundaries = Some(new_boundaries);
                }
                pending.clear();
                pending_at = 0;
                frames_written = 0;
                generation = new_generation;
                await_drain(&shared, new_generation);
            }
            Ok(DecodeCommand::Preroll { decoder, seq }) => {
                queued = Some((decoder, seq));
            }
            Ok(DecodeCommand::NoFollowUp) => {
                // Leave `asked_for_next` set so we do not keep asking.
                queued = None;
            }
            Ok(DecodeCommand::Seek {
                frames,
                generation: new_generation,
            }) => {
                if let Some(decoder) = current.as_mut() {
                    if let Err(err) = decoder.seek(frames) {
                        let _ = tx.send(DecodeEvent::Failed(err.to_string()));
                    }
                }
                // A seek invalidates the pre-roll: the track may no longer be
                // about to end, and any boundary already queued is meaningless.
                queued = None;
                asked_for_next = false;
                pending.clear();
                pending_at = 0;
                frames_written = 0;
                generation = new_generation;
                // Do not write until the callback has thrown away the audio
                // belonging to where we were, or it will discard this too.
                await_drain(&shared, new_generation);
            }
            Ok(DecodeCommand::Rebind {
                sinks,
                frames,
                generation: new_generation,
            }) => {
                // The device changed underneath us. The file stays open and
                // keeps its position; only the ring and the stream are new.
                if let Some(decoder) = current.as_mut() {
                    let _ = decoder.seek(frames);
                }
                audio = Some(sinks.0);
                boundaries = Some(sinks.1);
                queued = None;
                asked_for_next = false;
                pending.clear();
                pending_at = 0;
                frames_written = 0;
                generation = new_generation;
                await_drain(&shared, new_generation);
            }
            Ok(DecodeCommand::Stop) => {
                current = None;
                queued = None;
                asked_for_next = false;
                pending.clear();
                pending_at = 0;
            }
            Ok(DecodeCommand::Shutdown) | Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {}
        }

        let (Some(active), Some(sink)) = (current.as_mut(), audio.as_mut()) else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };

        // Ask for the next track while there is still audio to cover opening it.
        if !asked_for_next && queued.is_none() {
            if let Some(total) = active.format().total_frames {
                let rate = active.format().sample_rate.max(1) as f64;
                let remaining = total.saturating_sub(active.position_frames()) as f64 / rate;
                if remaining <= PREROLL_SECONDS {
                    asked_for_next = true;
                    let _ = tx.send(DecodeEvent::NeedNext);
                }
            }
        }

        let channels = active.format().channels.max(1) as u64;
        let capacity = sink.buffer().capacity();
        shared.set_buffered_frames(((capacity - sink.slots()) as u64) / channels);

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
            frames_written += take as u64 / channels;
            continue;
        }

        match active.next_block() {
            Ok(Some(block)) => {
                pending.clear();
                pending.extend_from_slice(block);
                pending_at = 0;
            }
            Ok(None) => match queued.take() {
                // Gapless: the next track's audio goes straight into the ring
                // behind this one, with a marker where the listener crosses over.
                Some((next, seq)) => {
                    if let Some(marks) = boundaries.as_mut() {
                        let _ = marks.push(Boundary {
                            generation,
                            at_frame: frames_written,
                            seq,
                        });
                    }
                    current = Some(next);
                    asked_for_next = false;
                }
                None => {
                    current = None;
                    let _ = tx.send(DecodeEvent::Finished);
                }
            },
            Err(err) => {
                current = None;
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

/// A track handed to the decoder to pre-roll, waiting for the listener to
/// actually reach it.
struct PendingTrack {
    seq: u64,
    cursor: usize,
    track_id: i64,
    duration_ms: u64,
    source: SourceSnapshot,
}

/// How much of the current track has been heard, for the history table.
struct PlayProgress {
    track_id: i64,
    furthest_ms: u64,
    duration_ms: u64,
}

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
    settings: OutputSettings,
    /// The album the open stream's rate was chosen for, for "follow album".
    stream_album: Option<i64>,
    /// The device's rate before we ever opened a stream on it.
    ///
    /// Captured before, not during: opening a stream can move the nominal rate
    /// by itself, so asking afterwards records our own change as the original.
    original_rate: Option<(crate::device::DeviceId, u32)>,
    /// ReplayGain of the track playing, so the gain can be recomputed when the
    /// volume moves without reopening anything.
    current_gain_db: Option<f32>,
    current_peak: Option<f32>,
    current_source: Option<SourceSnapshot>,
    paused_since: Option<Instant>,

    plays: Arc<Mutex<Vec<PlayEvent>>>,
    progress: Option<PlayProgress>,
    pending: VecDeque<PendingTrack>,
    next_seq: u64,
    seen_boundary: u64,

    /// Set from the stream's error callback when the device goes away.
    device_lost: Arc<AtomicBool>,
    /// True while there is no usable device, so playback resumes on reconnect.
    awaiting_device: bool,
    device_check: u8,
}

impl Control {
    fn new(
        commands: Receiver<Command>,
        shared: Arc<PlaybackShared>,
        state: Arc<Mutex<PlayerState>>,
        sample_rate: Arc<AtomicU32>,
        plays: Arc<Mutex<Vec<PlayEvent>>>,
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
            settings: OutputSettings::default(),
            stream_album: None,
            original_rate: None,
            current_gain_db: None,
            current_peak: None,
            current_source: None,
            paused_since: None,
            rng: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x2545F4914F6CDD1D)
                | 1,
            plays,
            progress: None,
            pending: VecDeque::new(),
            next_seq: 1,
            seen_boundary: 0,
            device_lost: Arc::new(AtomicBool::new(false)),
            awaiting_device: false,
            device_check: 0,
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
                    DecodeEvent::NeedNext => self.preroll_next(),
                    DecodeEvent::Finished => self.on_track_finished(),
                    DecodeEvent::Failed(message) => {
                        self.set_error(Some(message));
                        self.advance(1, false);
                    }
                }
            }
            self.observe_boundary();
            self.check_output_device();
            self.maybe_release_hog();
            self.publish();
        }
        // Never exit still holding the device.
        self.release_output();
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
                self.apply_gain();
            }
            Command::ReopenOutput => {
                self.device = None;
                self.check_output_device();
            }
            Command::SetOutputSettings(settings) => {
                let reopen = settings.exclusive != self.settings.exclusive
                    || settings.rate_mode != self.settings.rate_mode;
                self.settings = settings;
                self.apply_gain();
                if reopen && self.stream.is_some() {
                    // Exclusive access and the device rate are both decided when
                    // the stream opens, so changing either means opening again.
                    self.rebuild_output();
                }
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

    /// Where the queue goes next, without moving there yet.
    fn peek_next_cursor(&self) -> Option<usize> {
        if self.order.is_empty() {
            return None;
        }
        match self.repeat {
            RepeatMode::One => Some(self.cursor),
            _ => {
                let next = self.cursor + 1;
                if next < self.order.len() {
                    Some(next)
                } else if self.repeat == RepeatMode::All {
                    Some(0)
                } else {
                    None
                }
            }
        }
    }

    /// Open the next track and hand it to the decoder to run into the same ring.
    ///
    /// Only when it matches the open stream. Gapless and per-track rate
    /// switching are mutually exclusive across a rate boundary, so a track at a
    /// different rate or channel count gets an ordinary, gapped change instead.
    fn preroll_next(&mut self) {
        let Some(next_cursor) = self.peek_next_cursor() else {
            let _ = self.decode_tx.send(DecodeCommand::NoFollowUp);
            return;
        };
        let Some(index) = self.order.get(next_cursor).copied() else {
            let _ = self.decode_tx.send(DecodeCommand::NoFollowUp);
            return;
        };
        let Some(item) = self.queue.get(index).cloned() else {
            let _ = self.decode_tx.send(DecodeCommand::NoFollowUp);
            return;
        };

        let decoder = match TrackDecoder::open(Path::new(&item.path)) {
            Ok(decoder) => decoder,
            Err(_) => {
                // Unreadable files are dealt with when we actually get there.
                let _ = self.decode_tx.send(DecodeCommand::NoFollowUp);
                return;
            }
        };
        let format = decoder.format().clone();

        let matches_stream = self
            .stream
            .as_ref()
            .map(|stream| {
                stream.info().sample_rate == format.sample_rate
                    && stream.info().channels == format.channels
            })
            .unwrap_or(false);
        if !matches_stream {
            let _ = self.decode_tx.send(DecodeCommand::NoFollowUp);
            return;
        }

        let seq = self.next_seq;
        self.next_seq += 1;
        self.pending.push_back(PendingTrack {
            seq,
            cursor: next_cursor,
            track_id: item.track_id,
            duration_ms: format.duration().map(|d| d.as_millis() as u64).unwrap_or(0),
            source: SourceSnapshot {
                codec: format.codec.clone(),
                sample_rate: format.sample_rate,
                channels: format.channels,
                bits_per_sample: format.bits_per_sample,
            },
        });
        let _ = self.decode_tx.send(DecodeCommand::Preroll {
            decoder: Box::new(decoder),
            seq,
        });
    }

    /// Has the listener actually crossed into a pre-rolled track yet?
    ///
    /// The callback publishes this, not the decoder, which is already up to
    /// 150ms into the next track by the time the current one is still playing.
    fn observe_boundary(&mut self) {
        let seq = self.shared.boundary_seq();
        if seq == self.seen_boundary {
            return;
        }
        self.seen_boundary = seq;

        while let Some(track) = self.pending.pop_front() {
            if track.seq != seq {
                continue;
            }
            self.finish_play();
            self.cursor = track.cursor;
            if let Some(item) = self.order.get(track.cursor).and_then(|i| self.queue.get(*i)) {
                self.current_gain_db = item.replay_gain_db;
                self.current_peak = item.replay_gain_peak;
            }
            self.current_source = Some(track.source.clone());
            self.apply_gain();
            self.progress = Some(PlayProgress {
                track_id: track.track_id,
                furthest_ms: 0,
                duration_ms: track.duration_ms,
            });
            if let Ok(mut state) = self.state.lock() {
                state.track_id = Some(track.track_id);
                state.duration_ms = track.duration_ms;
                state.source = Some(track.source);
                state.error = None;
            }
            break;
        }
    }

    /// Bank what was heard of the current track.
    fn finish_play(&mut self) {
        let Some(progress) = self.progress.take() else {
            return;
        };
        if progress.duration_ms == 0 && progress.furthest_ms == 0 {
            return;
        }
        let completed = progress.duration_ms > 0
            && progress.furthest_ms as f64 >= progress.duration_ms as f64 * COMPLETION_FRACTION;
        if let Ok(mut plays) = self.plays.lock() {
            plays.push(PlayEvent {
                track_id: progress.track_id,
                ms_played: progress.furthest_ms,
                completed,
            });
        }
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

        // Anything pre-rolled is now irrelevant: we are jumping somewhere else.
        self.pending.clear();
        self.finish_play();

        let generation = self.shared.begin_seek(start_frames);
        let sinks = if needs_stream {
            match self.open_stream(format.sample_rate, format.channels) {
                Ok(sinks) => Some(sinks),
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
        self.current_gain_db = item.replay_gain_db;
        self.current_peak = item.replay_gain_peak;
        self.apply_gain();
        self.engage_output(format.sample_rate, item.album_id);

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
        self.current_source = Some(source.clone());

        let _ = self.decode_tx.send(DecodeCommand::Play {
            decoder: Box::new(decoder),
            sinks,
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

        self.progress = Some(PlayProgress {
            track_id: item.track_id,
            furthest_ms: 0,
            duration_ms,
        });

        if let Ok(mut state) = self.state.lock() {
            state.track_id = Some(item.track_id);
            state.duration_ms = duration_ms;
            state.source = Some(source);
            state.error = None;
        }
    }

    fn open_stream(
        &mut self,
        sample_rate: u32,
        channels: u16,
    ) -> Result<(Producer<f32>, Producer<Boundary>), String> {
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

        // Look at the device before anything of ours touches it.
        if self.original_rate.as_ref().map(|(id, _)| id) != Some(&device.id) {
            self.original_rate = self
                .backend
                .device_rate(&device.id)
                .ok()
                .map(|rate| (device.id.clone(), rate));
        }

        let (producer, boundaries, renderer) =
            ring::open(sample_rate, channels, Arc::clone(&self.shared));
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
                {
                    let flag = Arc::clone(&self.device_lost);
                    Arc::new(move |err: AudioError| {
                        if matches!(err, AudioError::DeviceLost) {
                            flag.store(true, Ordering::Release);
                        }
                    })
                },
            )
            .map_err(|e| format!("{e} ({})", device.id.0))?;

        if let Ok(mut state) = self.state.lock() {
            state.device = Some(stream.info().device.name.clone());
            state.device_sample_rate = Some(stream.info().sample_rate);
        }
        self.stream = Some(stream);
        Ok((producer, boundaries))
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

    /// Notice the output device moving or disappearing, and follow it.
    ///
    /// macOS changes the default output constantly: headphones out, interface
    /// in, Bluetooth connects. A player that assumes one fixed device is wrong
    /// within minutes of real use.
    fn check_output_device(&mut self) {
        let lost = self.device_lost.swap(false, Ordering::AcqRel);

        self.device_check = self.device_check.wrapping_add(1);
        let due = self.device_check % DEVICE_CHECK_TICKS == 0;
        if !lost && !due && !self.awaiting_device {
            return;
        }

        // Ask for the id alone: full enumeration stops working while we hold
        // the device exclusively, and a failure there would look identical to
        // the device vanishing.
        let default_id = self.backend.default_device_id().ok();
        let moved = match (&self.device, &default_id) {
            (Some(current), Some(now)) => current.id != *now,
            _ => false,
        };

        let forced = self.device.is_none() && self.stream.is_some();
        if !lost && !moved && !forced && !self.awaiting_device {
            return;
        }

        if default_id.is_none() && !lost {
            // Cannot tell what the default is. Doing nothing is strictly better
            // than tearing down a stream that is playing perfectly well.
            return;
        }

        let Some(default) = self.backend.default_device().ok().filter(|info| {
            // A device we cannot address is not a device we can move to.
            default_id
                .as_ref()
                .map(|id| info.id == *id)
                .unwrap_or(false)
        }) else {
            if lost && self.stream.is_some() {
                self.shared.set_playing(false);
                self.awaiting_device = true;
                self.set_error(Some("No output device available".into()));
            }
            return;
        };

        self.device = Some(default);
        self.rebuild_output();
    }

    /// Move the running stream to the current default device.
    ///
    /// The open file and its position survive: only the ring and the stream are
    /// rebuilt, so the gap is the device open plus a prime, not a track restart.
    fn rebuild_output(&mut self) {
        let Some(stream) = self.stream.as_ref() else {
            self.awaiting_device = false;
            return;
        };
        let rate = stream.info().sample_rate;
        let channels = stream.info().channels;
        let frames = self.shared.frames_played();
        let was_playing = self.shared.is_playing() || self.awaiting_device;

        // Anything pre-rolled belongs to the ring that is about to be replaced.
        self.pending.clear();
        let generation = self.shared.begin_seek(frames);

        match self.open_stream(rate, channels) {
            Ok(sinks) => {
                // A rebuilt stream is a new stream: it has to be taken and put
                // at the right rate again, exactly like a freshly opened one.
                // Forgetting this is how "exclusive" silently stays shared.
                let album = self.stream_album;
                self.engage_output(rate, album);
                let _ = self.decode_tx.send(DecodeCommand::Rebind {
                    sinks,
                    frames,
                    generation,
                });
                self.await_prime(rate, channels);
                if let Some(stream) = self.stream.as_mut() {
                    let _ = stream.play();
                }
                self.shared.set_playing(was_playing);
                self.awaiting_device = false;
                self.set_error(None);
            }
            Err(err) => {
                // Most often the new device will not take the track's sample
                // rate. Without a resampler that is genuinely unplayable, so say
                // so instead of playing something wrong.
                self.shared.set_playing(false);
                self.awaiting_device = true;
                self.set_error(Some(format!("Output device unavailable: {err}")));
            }
        }
    }

    /// Volume multiplied by ReplayGain, held back so it cannot clip.
    ///
    /// Several real files already peak above full scale, so a positive gain
    /// applied blindly would clip at the DAC. The peak is what stops that.
    fn apply_gain(&self) {
        let mut gain = self.volume;
        if self.settings.replay_gain {
            if let Some(db) = self.current_gain_db {
                let linear = 10f32.powf(db / 20.0);
                let limited = match self.current_peak {
                    Some(peak) if peak > 0.0 => linear.min(1.0 / peak),
                    _ => linear,
                };
                gain *= limited;
            }
        }
        self.shared.set_target_gain(gain);
    }

    /// True when ReplayGain is actually changing the audio right now.
    fn replay_gain_active(&self) -> bool {
        self.settings.replay_gain
            && self.current_gain_db.map(|db| db.abs() > 0.01).unwrap_or(false)
    }

    /// What the device should be running at for this track.
    fn desired_device_rate(&self, file_rate: u32, album_id: Option<i64>) -> Option<u32> {
        match self.settings.rate_mode {
            RateMode::FollowFile => Some(file_rate),
            RateMode::Fixed(rate) => Some(rate),
            // Tracks in an album share a rate, so switching only when the album
            // changes gets almost all of the fidelity without a gap per track.
            RateMode::FollowAlbum => {
                if album_id.is_some() && album_id == self.stream_album {
                    None
                } else {
                    Some(file_rate)
                }
            }
        }
    }

    /// Take the device and put it at the right rate, in that order: the rate
    /// only sticks once nothing else can move it.
    fn engage_output(&mut self, file_rate: u32, album_id: Option<i64>) {
        let exclusive = self.settings.exclusive;
        let desired = self.desired_device_rate(file_rate, album_id);

        if let Some(stream) = self.stream.as_mut() {
            if exclusive {
                if let Err(err) = stream.set_exclusive(true) {
                    // Another application already owns it, or the device does
                    // not allow it. Shared mode still plays.
                    tracing::warn!(%err, "could not take exclusive access");
                }
            }
            if let Some(rate) = desired {
                if let Err(err) = stream.set_rate(rate) {
                    tracing::debug!(%err, "device kept its own rate");
                }
            }
        }
        self.stream_album = album_id;
    }

    /// Give the device back, and leave it as we found it.
    ///
    /// Restoring the rate matters: a system-wide setting changed and abandoned
    /// means the next application inherits whatever the last track wanted.
    fn release_output(&mut self) {
        if let Some(stream) = self.stream.as_mut() {
            let _ = stream.set_exclusive(false);
        }
        if let Some((id, rate)) = self.original_rate.clone() {
            let _ = self.backend.set_device_rate(&id, rate);
        }
    }

    fn resume(&mut self) {
        if let Some(stream) = self.stream.as_mut() {
            let _ = stream.play();
        }
        self.paused_since = None;
        if self.settings.exclusive {
            if let Some(stream) = self.stream.as_mut() {
                let _ = stream.set_exclusive(true);
            }
        }
        self.apply_gain();
        self.shared.set_playing(true);
    }

    fn pause(&mut self) {
        // Leave the stream running so the gain ramp can fade out rather than
        // cutting; the callback stops pulling once it reaches silence.
        self.shared.set_playing(false);
        self.paused_since = Some(Instant::now());
    }

    /// Hand the device back if we have been paused a while holding it.
    fn maybe_release_hog(&mut self) {
        if !self.settings.exclusive || self.shared.is_playing() {
            return;
        }
        let Some(since) = self.paused_since else {
            return;
        };
        if since.elapsed() >= HOG_RELEASE_AFTER {
            self.paused_since = None;
            self.release_output();
        }
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

    /// Assemble the signal path: what the file is, everything that could have
    /// touched it, and what the hardware is actually doing.
    fn signal_path(&self) -> Option<SignalPath> {
        let source = self.current_source.clone()?;
        let stream = self.stream.as_ref();
        let physical = stream.and_then(|s| s.info().physical.as_ref());

        // Something resampled if the hardware is not running at the file's rate.
        // Whether it was us or CoreAudio, the audio was altered.
        let device_rate = physical.map(|format| format.sample_rate);
        let resampled = device_rate.map(|rate| rate != source.sample_rate).unwrap_or(false);

        let volume_active = (self.volume - 1.0).abs() > 0.001;
        let replay_gain = self.replay_gain_active();

        let processing = vec![
            Stage {
                name: "Resampling".into(),
                active: resampled,
                detail: resampled.then(|| {
                    format!(
                        "{} Hz to {} Hz",
                        source.sample_rate,
                        device_rate.unwrap_or(0)
                    )
                }),
            },
            Stage {
                name: "Volume".into(),
                active: volume_active,
                detail: volume_active.then(|| format!("{:.0}%", self.volume * 100.0)),
            },
            Stage {
                name: "ReplayGain".into(),
                active: replay_gain,
                detail: replay_gain
                    .then(|| self.current_gain_db.map(|db| format!("{db:+.2} dB")))
                    .flatten(),
            },
            // Crossfade shares most of gapless's machinery and lands after it.
            Stage {
                name: "Crossfade".into(),
                active: false,
                detail: None,
            },
        ];

        let altered_stages = processing.iter().filter(|stage| stage.active).count();
        let exclusive = stream.map(|s| s.info().exclusive).unwrap_or(false);

        // Green only when nothing fired and the hardware really is at the
        // file's rate. An unreadable device format is not a pass.
        let bit_perfect = altered_stages == 0 && device_rate == Some(source.sample_rate);
        let reason = if bit_perfect {
            None
        } else if device_rate.is_none() {
            Some("The device would not report its format".into())
        } else if altered_stages > 0 {
            Some(format!(
                "{} of 4 stages altered the audio",
                altered_stages
            ))
        } else {
            Some("The device is not running at the file's rate".into())
        };

        Some(SignalPath {
            source,
            // Decoders emit 32-bit float. That is not a choice we make.
            decoder_sample_rate: self.sample_rate.load(Ordering::Relaxed),
            decoder_format: "f32".into(),
            processing,
            device_name: stream.map(|s| s.info().device.name.clone()),
            device_format: physical.map(DeviceFormatView::from),
            exclusive,
            bit_perfect,
            altered_stages,
            reason,
        })
    }

    fn publish(&mut self) {
        // Furthest point reached, not wall clock: seeking back and forth must
        // not inflate how much of a track counts as heard.
        if let Some(progress) = self.progress.as_mut() {
            progress.furthest_ms = progress.furthest_ms.max(
                self.shared.frames_played() * 1000
                    / self.sample_rate.load(Ordering::Relaxed).max(1) as u64,
            );
        }
        if let Ok(mut state) = self.state.lock() {
            state.playing = self.shared.is_playing();
            state.queue = self.queue.iter().map(|item| item.track_id).collect();
            state.queue_index = self.current_index().unwrap_or(0);
            state.volume = self.volume;
            state.repeat = self.repeat;
            state.shuffle = self.shuffle;
            state.underruns = self.shared.underruns();
            state.settings = self.settings.clone();
            state.device_rates = self
                .device
                .as_ref()
                .map(|device| device.sample_rates.clone())
                .unwrap_or_default();
        }
        // Built outside the lock: it reads the stream, which the lock does not
        // protect anyway.
        let signal = self.signal_path();
        if let Ok(mut state) = self.state.lock() {
            state.signal = signal;
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
        ..Default::default()
    }
}
