//! CoreAudio output via cpal.
//!
//! Everything platform-specific lives behind [`crate::device::AudioBackend`].
//! Phase 5 replaces parts of this with direct `coreaudio-rs` calls, for hog mode
//! and for reading the device's real format back rather than trusting cpal's
//! negotiated view -- see the note on [`CpalStream`].

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, StreamConfig};

use crate::device::{
    AudioBackend, AudioError, DeviceFormat, DeviceId, DeviceInfo, ErrorSink, OutputStream, Renderer,
    Result, StreamInfo, StreamRequest,
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

    /// Read straight from `kAudioHardwarePropertyDefaultOutputDevice`.
    ///
    /// cpal's enumeration stops resolving names and ids while we hold the
    /// device exclusively, which made the device watchdog mistake exclusive
    /// mode for the device disappearing and tear playback down. This property
    /// answers regardless.
    #[cfg(target_os = "macos")]
    fn default_device_id(&self) -> Result<DeviceId> {
        use crate::coreaudio;
        let device = coreaudio::default_output_device()?;
        let uid = coreaudio::device_uid(device)
            .ok_or_else(|| AudioError::Device("the default device has no UID".into()))?;
        Ok(DeviceId(format!("coreaudio:{uid}")))
    }

    #[cfg(target_os = "macos")]
    fn device_rate(&self, device: &DeviceId) -> Result<u32> {
        use crate::coreaudio;
        let id = coreaudio::device_by_uid(coreaudio::uid_from_cpal_id(&device.0))
            .ok_or_else(|| AudioError::DeviceNotFound(device.0.clone()))?;
        coreaudio::nominal_sample_rate(id)
    }

    #[cfg(target_os = "macos")]
    fn set_device_rate(&self, device: &DeviceId, rate: u32) -> Result<()> {
        use crate::coreaudio;
        let id = coreaudio::device_by_uid(coreaudio::uid_from_cpal_id(&device.0))
            .ok_or_else(|| AudioError::DeviceNotFound(device.0.clone()))?;
        coreaudio::set_nominal_sample_rate(id, rate)
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
        on_error: ErrorSink,
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
                move |err| {
                    // cpal reports a disconnect or a reroute here rather than
                    // failing the next callback, which is the only notification
                    // macOS gives without a CoreAudio property listener.
                    let mapped = match err.kind() {
                        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::DeviceChanged => {
                            AudioError::DeviceLost
                        }
                        _ => AudioError::Device(err.to_string()),
                    };
                    tracing::warn!(%err, "output stream error");
                    on_error(mapped);
                },
                None,
            )
            .map_err(|err| AudioError::Device(err.to_string()))?;

        let info = StreamInfo {
            device: Self::describe(&device, None),
            sample_rate: config.sample_rate,
            channels,
            buffer_frames: request.buffer_frames,
            exclusive: false,
            physical: read_physical(id),
        };

        Ok(Box::new(CpalStream {
            stream,
            info,
            device_id: id.clone(),
        }))
    }
}

/// Read the hardware's own format, which is the only honest basis for a
/// bit-perfect claim.
#[cfg(target_os = "macos")]
fn read_physical(id: &DeviceId) -> Option<DeviceFormat> {
    use crate::coreaudio;
    let device = coreaudio::device_by_uid(coreaudio::uid_from_cpal_id(&id.0))?;
    coreaudio::physical_format(device).ok()
}

#[cfg(not(target_os = "macos"))]
fn read_physical(_id: &DeviceId) -> Option<DeviceFormat> {
    // Until a backend can read its device back, the signal path reports
    // "unknown" rather than echoing the request as if it were fact.
    None
}

struct CpalStream {
    stream: cpal::Stream,
    info: StreamInfo,
    device_id: DeviceId,
}

impl CpalStream {
    /// Re-read the hardware format. The device may have moved underneath us --
    /// taking hog mode and changing the rate both do exactly that.
    fn refresh_physical(&mut self) {
        self.info.physical = read_physical(&self.device_id);
    }
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

    #[cfg(target_os = "macos")]
    fn set_rate(&mut self, rate: u32) -> Result<()> {
        use crate::coreaudio;
        let uid = coreaudio::uid_from_cpal_id(&self.device_id.0);
        let device = coreaudio::device_by_uid(uid)
            .ok_or_else(|| AudioError::DeviceNotFound(self.device_id.0.clone()))?;
        coreaudio::set_nominal_sample_rate(device, rate)?;
        self.refresh_physical();
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn set_exclusive(&mut self, exclusive: bool) -> Result<()> {
        use crate::coreaudio;
        let uid = coreaudio::uid_from_cpal_id(&self.device_id.0);
        let device = coreaudio::device_by_uid(uid)
            .ok_or_else(|| AudioError::DeviceNotFound(self.device_id.0.clone()))?;

        if exclusive {
            coreaudio::take_hog(device)?;
        } else {
            coreaudio::release_hog(device)?;
        }
        self.info.exclusive = exclusive;
        self.refresh_physical();
        Ok(())
    }
}

/// Never leave the device held. A process that exits still owning it leaves the
/// machine silent for everything else until something forces it back.
#[cfg(target_os = "macos")]
impl Drop for CpalStream {
    fn drop(&mut self) {
        if !self.info.exclusive {
            return;
        }
        use crate::coreaudio;
        let uid = coreaudio::uid_from_cpal_id(&self.device_id.0);
        if let Some(device) = coreaudio::device_by_uid(uid) {
            let _ = coreaudio::release_hog(device);
        }
    }
}
