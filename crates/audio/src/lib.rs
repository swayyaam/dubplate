//! Realtime audio engine for dubplate.
//!
//! Three participants, and the rules between them are the whole design:
//!
//! - the **control thread** owns queue and playback state and talks to the UI,
//! - the **decode thread** turns files into interleaved f32 and writes the ring,
//! - the **audio callback** drains the ring, applies gain, and touches nothing else.
//!
//! The callback is sacred: no allocation, no locks, no I/O. They communicate
//! through atomics in [`ring::PlaybackShared`] and through the ring buffer
//! itself, never a `Mutex`.

pub mod backend;
#[cfg(target_os = "macos")]
pub mod coreaudio;
pub mod decode;
pub mod device;
pub mod engine;
pub mod gain;
pub mod peaks;
pub mod ring;
pub mod sine;

pub use backend::CpalBackend;
pub use decode::{DecodeError, SourceFormat, TrackDecoder};
pub use engine::{
    Command, Engine, OutputSettings, PlayerState, QueueItem, RateMode, RepeatMode, SignalPath,
};
pub use device::{
    AudioBackend, AudioError, DeviceFormat, DeviceId, DeviceInfo, OutputStream, Renderer,
    StreamInfo, StreamRequest,
};
pub use gain::GainRamp;
pub use ring::{ring_capacity_frames, Boundary, PlaybackShared, RingRenderer};
pub use sine::SineRenderer;
