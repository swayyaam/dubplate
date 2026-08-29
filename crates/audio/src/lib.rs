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
pub mod decode;
pub mod device;
pub mod engine;
pub mod gain;
pub mod ring;
pub mod sine;

pub use backend::CpalBackend;
pub use decode::{DecodeError, SourceFormat, TrackDecoder};
pub use engine::{Command, Engine, PlayerState, QueueItem, RepeatMode};
pub use device::{
    AudioBackend, AudioError, DeviceId, DeviceInfo, OutputStream, Renderer, StreamInfo,
    StreamRequest,
};
pub use gain::GainRamp;
pub use ring::{ring_capacity_frames, PlaybackShared, RingRenderer};
pub use sine::SineRenderer;
