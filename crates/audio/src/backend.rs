//! CoreAudio output via cpal.
//!
//! Everything platform-specific lives behind [`crate::device::AudioBackend`].
//! Phase 5 replaces parts of this with direct `coreaudio-rs` calls, for hog mode
//! and for reading the device's real format back rather than trusting cpal's
//! negotiated view -- see the note on [`CpalStream`].

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, StreamConfig};

use crate::device::{
    AudioBackend, AudioError, DeviceId, DeviceInfo, OutputStream, Renderer, Result, StreamInfo,
    StreamRequest,
};

pub struct CpalBackend;

impl CpalBackend {
    pub fn new() -> Self {
        Self
    }

    fn describe(device: &cpal::Device, default_id: Option<&str>) -> DeviceInfo {
        let name = device
            .description()
            .map(|description| description.name().to_owned())
            .unwrap_or_else(|_| "Unknown device".into());

        let mut sample_rates: Vec<u32> = device
            .supported_output_configs()
            .map(|configs| {
                configs
                    .flat_map(|config| [config.min_sample_rate(), config.max_sample_rate()])
                    .collect()
            })
            .unwrap_or_default();
        sample_rates.sort_unstable();
        sample_rates.dedup();

        let max_channels = device
            .supported_output_configs()
            .map(|configs| configs.map(|config| config.channels()).max().unwrap_or(2))
            .unwrap_or(2);

        // cpal's DeviceId round-trips through Display/FromStr, so this is
        // something phase 3 can persist and reopen after a reconnect. The name
        // is not: two identical interfaces share one.
        let id = device
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|_| name.clone());

        DeviceInfo {
            is_default: default_id == Some(id.as_str()),
            id: DeviceId(id),
            name,
            sample_rates,
            max_channels,
        }
    }

    fn find(host: &cpal::Host, id: &DeviceId) -> Result<cpal::Device> {
        if let Ok(parsed) = id.0.parse::<cpal::DeviceId>() {
            if let Some(device) = host.device_by_id(&parsed) {
                return Ok(device);
            }
        }
        Err(AudioError::DeviceNotFound(id.0.clone()))
    }
}

impl Default for CpalBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioBackend for CpalBackend {
    fn name(&self) -> &'static str {
        "coreaudio (cpal)"
    }

    fn enumerate(&self) -> Result<Vec<DeviceInfo>> {
        let host = cpal::default_host();
        let default_id = host
            .default_output_device()
            .and_then(|device| device.id().ok())
            .map(|id| id.to_string());
        let devices = host
            .output_devices()
            .map_err(|err| AudioError::Device(err.to_string()))?;
        Ok(devices
            .map(|device| Self::describe(&device, default_id.as_deref()))
            .collect())
    }

    fn default_device(&self) -> Result<DeviceInfo> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(AudioError::NoDevice)?;
        let id = device.id().ok().map(|id| id.to_string());
        Ok(Self::describe(&device, id.as_deref()))
    }

    fn open(
        &self,
        id: &DeviceId,
        request: StreamRequest,
        mut renderer: Box<dyn Renderer>,
    ) -> Result<Box<dyn OutputStream>> {
        let host = cpal::default_host();
        let device = Self::find(&host, id)?;

        let default_config = device
            .default_output_config()
            .map_err(|err| AudioError::Device(err.to_string()))?;

        // macOS hands us float samples. Anything else would need conversion in
        // the callback, and would not be bit-perfect anyway.
        if default_config.sample_format() != SampleFormat::F32 {
            return Err(AudioError::Unsupported("non-float output sample formats"));
        }

        let channels = request.channels.min(default_config.channels()).max(1);
        let config = StreamConfig {
            channels,
            sample_rate: request.sample_rate,
            buffer_size: match request.buffer_frames {
                // Ask for a small buffer; the device is free to refuse.
                Some(frames) => BufferSize::Fixed(frames),
                None => BufferSize::Default,
            },
        };

        let stream = device
            .build_output_stream(
                config.clone(),
                move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    renderer.render(output, channels);
                },
                |err| tracing::error!(%err, "output stream error"),
                None,
            )
            .map_err(|err| AudioError::Device(err.to_string()))?;

        let info = StreamInfo {
            device: Self::describe(&device, None),
            sample_rate: config.sample_rate,
            channels,
            buffer_frames: request.buffer_frames,
            exclusive: false,
        };

        Ok(Box::new(CpalStream { stream, info }))
    }
}

/// # A caveat that matters for phase 5
///
/// `info()` reports the configuration cpal negotiated, which is what we asked
/// for, not what the hardware is running. The signal path panel must not build
/// a bit-perfect verdict on this: that needs
/// `kAudioStreamPropertyPhysicalFormat` read back from the device itself.
struct CpalStream {
    stream: cpal::Stream,
    info: StreamInfo,
}

impl OutputStream for CpalStream {
    fn info(&self) -> &StreamInfo {
        &self.info
    }

    fn play(&mut self) -> Result<()> {
        self.stream
            .play()
            .map_err(|err| AudioError::Device(err.to_string()))
    }

    fn pause(&mut self) -> Result<()> {
        self.stream
            .pause()
            .map_err(|err| AudioError::Device(err.to_string()))
    }
}
