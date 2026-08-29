//! Direct CoreAudio, for the things cpal cannot express.
//!
//! Three of them, and they are the whole of phase 5:
//!
//! - reading the device's *physical* format back, rather than reporting what we
//!   asked for. Claiming bit-perfect because we asked politely is how players
//!   lie without intending to.
//! - setting the device's nominal sample rate, so a 96kHz file can play at
//!   96kHz instead of being resampled by the system.
//! - hog mode, which is what makes any of that meaningful: without exclusive
//!   access the system mixer is between us and the hardware regardless.
#![cfg(target_os = "macos")]
#![allow(non_upper_case_globals)]

use std::ffi::c_void;
use std::ptr;

use coreaudio_sys::{
    kAudioDevicePropertyAvailableNominalSampleRates, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyHogMode, kAudioDevicePropertyNominalSampleRate,
    kAudioDevicePropertyStreams, kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked,
    kAudioFormatFlagIsSignedInteger, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    kAudioStreamPropertyPhysicalFormat, kAudioStreamPropertyVirtualFormat,
    AudioDeviceID, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectSetPropertyData, AudioStreamBasicDescription,
    AudioValueRange, OSStatus,
};
use core_foundation_sys::base::CFRelease;
use core_foundation_sys::string::{
    kCFStringEncodingUTF8, CFStringGetCString, CFStringGetLength, CFStringRef,
};

use crate::device::{AudioError, DeviceFormat, Result};

pub use crate::device::DeviceFormat as PhysicalFormat;

fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn check(status: OSStatus, what: &'static str) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(AudioError::Device(format!("{what} failed (OSStatus {status})")))
    }
}

/// Read a fixed-size property into `T`.
fn get<T>(object: AudioObjectID, selector: u32, scope: u32, what: &'static str) -> Result<T> {
    let addr = address(selector, scope);
    let mut size = std::mem::size_of::<T>() as u32;
    let mut value = std::mem::MaybeUninit::<T>::uninit();
    // SAFETY: `size` matches the buffer we hand over, and the status is checked
    // before the value is assumed initialised.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &addr,
            0,
            ptr::null(),
            &mut size,
            value.as_mut_ptr() as *mut c_void,
        )
    };
    check(status, what)?;
    if size as usize != std::mem::size_of::<T>() {
        return Err(AudioError::Device(format!("{what} returned {size} bytes")));
    }
    // SAFETY: the call succeeded and wrote exactly the expected size.
    Ok(unsafe { value.assume_init() })
}

/// Read a variable-length array property.
fn get_array<T: Copy>(
    object: AudioObjectID,
    selector: u32,
    scope: u32,
    what: &'static str,
) -> Result<Vec<T>> {
    let addr = address(selector, scope);
    let mut size = 0u32;
    // SAFETY: querying the size only writes to `size`.
    let status = unsafe { AudioObjectGetPropertyDataSize(object, &addr, 0, ptr::null(), &mut size) };
    check(status, what)?;

    let count = size as usize / std::mem::size_of::<T>();
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::<T>::with_capacity(count);
    // SAFETY: the buffer holds exactly `size` bytes, which is what we asked for.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &addr,
            0,
            ptr::null(),
            &mut size,
            out.as_mut_ptr() as *mut c_void,
        )
    };
    check(status, what)?;
    // SAFETY: CoreAudio filled `size` bytes, i.e. `size / size_of::<T>()` items.
    unsafe { out.set_len(size as usize / std::mem::size_of::<T>()) };
    Ok(out)
}

pub fn default_output_device() -> Result<AudioDeviceID> {
    get(
        kAudioObjectSystemObject,
        kAudioHardwarePropertyDefaultOutputDevice,
        kAudioObjectPropertyScopeGlobal,
        "reading the default output device",
    )
}

/// Find a device by its CoreAudio UID, which is what cpal's `DeviceId` carries.
pub fn device_by_uid(uid: &str) -> Option<AudioDeviceID> {
    let devices: Vec<AudioDeviceID> = get_array(
        kAudioObjectSystemObject,
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
        "listing devices",
    )
    .ok()?;

    devices
        .into_iter()
        .find(|device| device_uid(*device).as_deref() == Some(uid))
}

pub fn device_uid(device: AudioDeviceID) -> Option<String> {
    let raw: CFStringRef = get(
        device,
        kAudioDevicePropertyDeviceUID,
        kAudioObjectPropertyScopeGlobal,
        "reading a device UID",
    )
    .ok()?;
    if raw.is_null() {
        return None;
    }

    // SAFETY: the UID comes back under the create rule, so we own this reference
    // and must release it. The buffer is sized from the string's own length.
    let text = unsafe {
        let length = CFStringGetLength(raw);
        let capacity = length as usize * 4 + 1;
        let mut buffer = vec![0i8; capacity];
        let ok = CFStringGetCString(raw, buffer.as_mut_ptr(), capacity as isize, kCFStringEncodingUTF8);
        CFRelease(raw as *const c_void);
        if ok == 0 {
            return None;
        }
        std::ffi::CStr::from_ptr(buffer.as_ptr())
            .to_string_lossy()
            .into_owned()
    };
    Some(text)
}

/// The format the hardware is running, not the one we requested.
///
/// This is the single most important call in the signal path panel: everything
/// above it is what we intended, and this is what is true.
pub fn physical_format(device: AudioDeviceID) -> Result<DeviceFormat> {
    let stream = output_stream(device)?;
    let description: AudioStreamBasicDescription = get(
        stream,
        kAudioStreamPropertyPhysicalFormat,
        kAudioObjectPropertyScopeGlobal,
        "reading the device physical format",
    )?;
    Ok(describe(&description))
}

/// What the device presents to software, after any conversion it does itself.
pub fn virtual_format(device: AudioDeviceID) -> Result<DeviceFormat> {
    let stream = output_stream(device)?;
    let description: AudioStreamBasicDescription = get(
        stream,
        kAudioStreamPropertyVirtualFormat,
        kAudioObjectPropertyScopeGlobal,
        "reading the device virtual format",
    )?;
    Ok(describe(&description))
}

fn output_stream(device: AudioDeviceID) -> Result<AudioObjectID> {
    let streams: Vec<AudioObjectID> = get_array(
        device,
        kAudioDevicePropertyStreams,
        kAudioObjectPropertyScopeOutput,
        "listing device streams",
    )?;
    streams
        .into_iter()
        .next()
        .ok_or_else(|| AudioError::Device("the device has no output stream".into()))
}

fn describe(description: &AudioStreamBasicDescription) -> DeviceFormat {
    let flags = description.mFormatFlags;
    let float = flags & kAudioFormatFlagIsFloat != 0;
    let signed = flags & kAudioFormatFlagIsSignedInteger != 0;
    let packed = flags & kAudioFormatFlagIsPacked != 0;
    let bits = description.mBitsPerChannel;

    // 32-bit integer and 32-bit float are both common and are different things.
    // Collapsing them to "32 bit" is exactly the sloppiness this panel exists to
    // avoid.
    let sample_format = if float {
        format!("f{bits}")
    } else if signed {
        let mut name = format!("s{bits}");
        if !packed && bits == 24 {
            // 24 bits carried in a 32-bit slot: worth saying, since it changes
            // nothing about the audio but everything about the wire format.
            name.push_str(" in 32");
        }
        name
    } else {
        format!("u{bits}")
    };

    DeviceFormat {
        sample_rate: description.mSampleRate.round() as u32,
        bits_per_channel: bits,
        channels: description.mChannelsPerFrame,
        sample_format,
    }
}

pub fn nominal_sample_rate(device: AudioDeviceID) -> Result<u32> {
    let rate: f64 = get(
        device,
        kAudioDevicePropertyNominalSampleRate,
        kAudioObjectPropertyScopeGlobal,
        "reading the nominal sample rate",
    )?;
    Ok(rate.round() as u32)
}

/// Rates the device will accept. Ranges are usually single points, but an
/// aggregate device can report a genuine span.
pub fn available_sample_rates(device: AudioDeviceID) -> Result<Vec<u32>> {
    let ranges: Vec<AudioValueRange> = get_array(
        device,
        kAudioDevicePropertyAvailableNominalSampleRates,
        kAudioObjectPropertyScopeGlobal,
        "reading available sample rates",
    )?;
    let mut rates: Vec<u32> = ranges
        .iter()
        .flat_map(|range| {
            let low = range.mMinimum.round() as u32;
            let high = range.mMaximum.round() as u32;
            if low == high {
                vec![low]
            } else {
                // Report the standard rates the span covers rather than every
                // integer between them.
                [44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000]
                    .into_iter()
                    .filter(|rate| *rate >= low && *rate <= high)
                    .collect()
            }
        })
        .collect();
    rates.sort_unstable();
    rates.dedup();
    Ok(rates)
}

/// Ask the device to run at `rate`.
///
/// Only meaningful with hog mode: in shared mode the system may refuse, or
/// change it back when another application wants something else.
pub fn set_nominal_sample_rate(device: AudioDeviceID, rate: u32) -> Result<()> {
    let addr = address(
        kAudioDevicePropertyNominalSampleRate,
        kAudioObjectPropertyScopeGlobal,
    );
    let value = rate as f64;
    // SAFETY: the property is a Float64 and that is what we pass.
    let status = unsafe {
        AudioObjectSetPropertyData(
            device,
            &addr,
            0,
            ptr::null(),
            std::mem::size_of::<f64>() as u32,
            &value as *const f64 as *const c_void,
        )
    };
    check(status, "setting the nominal sample rate")
}

/// Who currently owns the device exclusively. `None` means nobody.
pub fn hog_owner(device: AudioDeviceID) -> Result<Option<i32>> {
    let pid: i32 = get(
        device,
        kAudioDevicePropertyHogMode,
        kAudioObjectPropertyScopeGlobal,
        "reading hog mode",
    )?;
    Ok(if pid < 0 { None } else { Some(pid) })
}

/// Take exclusive access. System sounds and every other application go silent,
/// which is why this is per-device opt-in and never the default.
pub fn take_hog(device: AudioDeviceID) -> Result<i32> {
    set_hog(device, std::process::id() as i32)?;
    match hog_owner(device)? {
        Some(pid) => Ok(pid),
        None => Err(AudioError::Device("the device refused exclusive access".into())),
    }
}

/// Give the device back. Must happen on pause, on backgrounding, and on exit --
/// otherwise the user is left wondering why nothing else on the machine has
/// any sound.
pub fn release_hog(device: AudioDeviceID) -> Result<()> {
    set_hog(device, -1)
}

fn set_hog(device: AudioDeviceID, pid: i32) -> Result<()> {
    let addr = address(kAudioDevicePropertyHogMode, kAudioObjectPropertyScopeGlobal);
    let mut value = pid;
    // SAFETY: the property is a pid_t, which is i32.
    let status = unsafe {
        AudioObjectSetPropertyData(
            device,
            &addr,
            0,
            ptr::null(),
            std::mem::size_of::<i32>() as u32,
            &mut value as *mut i32 as *const c_void,
        )
    };
    check(status, "setting hog mode")
}

/// Strip the host prefix cpal puts on its device ids: `coreaudio:<UID>`.
pub fn uid_from_cpal_id(id: &str) -> &str {
    id.split_once(':').map(|(_, uid)| uid).unwrap_or(id)
}
