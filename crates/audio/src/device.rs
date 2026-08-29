//! The boundary between the engine and whatever is actually making sound.
//!
//! Everything above this file works in interleaved f32 frames and knows nothing
//! about CoreAudio. That is deliberate: WASAPI and ALSA backends have to drop in
//! later without the decoder noticing, and retrofitting that boundary into an
//! engine that assumed one API is a rewrite.

use std::fmt;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("no output device is available")]
    NoDevice,
    #[error("output device not found: {0}")]
    DeviceNotFound(String),
    #[error("{0} is not supported by this backend")]
    Unsupported(&'static str),
    #[error("output device error: {0}")]
    Device(String),
    /// The device went away underneath a running stream: an interface
    /// unplugged, headphones pulled, or the system default moved.
    #[error("the output device is no longer available")]
    DeviceLost,
}

pub type Result<T> = std::result::Result<T, AudioError>;

/// Stable-ish handle for a device. On CoreAudio this is the device UID, which
/// survives reboots and unplugging, unlike the human-readable name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId(pub String);

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub is_default: bool,
    /// Rates the device advertises. Empty means it would not say.
    pub sample_rates: Vec<u32>,
    pub max_channels: u16,
}

/// What we ask the backend for. What we get back may differ, which is why
/// `StreamInfo` exists separately.
#[derive(Debug, Clone, Copy)]
pub struct StreamRequest {
    pub sample_rate: u32,
    pub channels: u16,
    /// Preferred device buffer, in frames. The backend may ignore it.
    pub buffer_frames: Option<u32>,
}

/// A format as the hardware reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceFormat {
    pub sample_rate: u32,
    pub bits_per_channel: u32,
    pub channels: u32,
    /// s16, s24, s32, f32 -- the distinction WAV needs and most players lose.
    pub sample_format: String,
}

/// What the device is actually running, read back from the device rather than
/// echoed from the request.
///
/// This distinction is the whole basis of the signal path panel in phase 5:
/// claiming bit-perfect because we asked politely is how players lie without
/// intending to.
#[derive(Debug, Clone)]
pub struct StreamInfo {
    pub device: DeviceInfo,
    pub sample_rate: u32,
    pub channels: u16,
    pub buffer_frames: Option<u32>,
    pub exclusive: bool,
    /// What the hardware says it is running, not what we asked for.
    ///
    /// `None` when it could not be read, which the signal path must report as
    /// "unknown" rather than quietly falling back to the request.
    pub physical: Option<DeviceFormat>,
}

/// Fills device buffers. Called on the realtime audio thread.
///
/// # Realtime contract
///
/// No allocation, no locks, no I/O, no syscalls, no unbounded loops. Every
/// player that clicks and pops broke this rule. Communicate with the rest of
/// the engine through atomics and the ring buffer, never a `Mutex`.
pub trait Renderer: Send {
    /// Fill `output` with `output.len() / channels` interleaved frames.
    /// Filling it with silence is always a valid answer.
    fn render(&mut self, output: &mut [f32], channels: u16);
}

/// A live output stream.
///
/// Deliberately not `Send`: the platform stream handle is not `Send` on macOS,
/// so whichever thread opens a stream must also own and drop it. That is the
/// control thread, which is where the doc's architecture puts it anyway.
pub trait OutputStream {
    /// What the device reports, not what we requested.
    fn info(&self) -> &StreamInfo;
    fn play(&mut self) -> Result<()>;
    fn pause(&mut self) -> Result<()>;

    /// Change the device's nominal rate. Phase 5; needs exclusive access to be
    /// meaningful, so the default backend declines.
    fn set_rate(&mut self, _rate: u32) -> Result<()> {
        Err(AudioError::Unsupported("changing the device sample rate"))
    }

    /// Take or release exclusive (hog) mode. Phase 5.
    fn set_exclusive(&mut self, _exclusive: bool) -> Result<()> {
        Err(AudioError::Unsupported("exclusive mode"))
    }
}

/// Where a stream reports failures that happen after it is open.
///
/// Called from a backend thread, so it must not block. The engine uses it to
/// notice a device disappearing without polling for it.
pub type ErrorSink = std::sync::Arc<dyn Fn(AudioError) + Send + Sync>;

pub trait AudioBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn enumerate(&self) -> Result<Vec<DeviceInfo>>;
    fn default_device(&self) -> Result<DeviceInfo>;

    /// Just the current default device's id, cheaply and reliably.
    ///
    /// Separate from `default_device` because that one enumerates, and
    /// enumeration is exactly what stops working once a device is held
    /// exclusively. Change detection must keep working while we hold it.
    fn default_device_id(&self) -> Result<DeviceId> {
        self.default_device().map(|device| device.id)
    }
    fn open(
        &self,
        device: &DeviceId,
        request: StreamRequest,
        renderer: Box<dyn Renderer>,
        on_error: ErrorSink,
    ) -> Result<Box<dyn OutputStream>>;

    /// The device's nominal rate, readable without opening a stream.
    ///
    /// Needed because opening a stream can itself move the rate, so anything
    /// hoping to put the device back as it found it has to look first.
    fn device_rate(&self, _device: &DeviceId) -> Result<u32> {
        Err(AudioError::Unsupported("reading the device sample rate"))
    }

    fn set_device_rate(&self, _device: &DeviceId, _rate: u32) -> Result<()> {
        Err(AudioError::Unsupported("setting the device sample rate"))
    }
}
