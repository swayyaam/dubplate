//! Ask the hardware what it is actually doing.
//!
//!     cargo run --release -p dubplate-audio --example device
//!
//! Everything here is read back from CoreAudio rather than echoed from a
//! request, which is the difference between a bit-perfect claim and a guess.

use dubplate_audio::backend::CpalBackend;
use dubplate_audio::coreaudio;
use dubplate_audio::device::AudioBackend;

fn main() -> anyhow::Result<()> {
    let backend = CpalBackend::new();
    let try_hog = std::env::args().any(|arg| arg == "--hog");
    let force_rate: Option<u32> = std::env::args()
        .skip_while(|a| a != "--set-rate")
        .nth(1)
        .and_then(|r| r.parse().ok());

    match backend.default_device() {
        Ok(info) => println!("default_device() -> name {:?} id {:?}\n", info.name, info.id.0),
        Err(err) => println!("default_device() -> ERROR {err}\n"),
    }

    for info in backend.enumerate()? {
        let uid = coreaudio::uid_from_cpal_id(&info.id.0);
        println!("{} {}", if info.is_default { "*" } else { " " }, info.name);
        println!("    uid            {uid}");

        let Some(device) = coreaudio::device_by_uid(uid) else {
            println!("    (not resolvable through CoreAudio)\n");
            continue;
        };
        println!("    AudioDeviceID  {device}");

        match coreaudio::nominal_sample_rate(device) {
            Ok(rate) => println!("    nominal rate   {rate} Hz"),
            Err(err) => println!("    nominal rate   — ({err})"),
        }
        match coreaudio::available_sample_rates(device) {
            Ok(rates) => println!(
                "    accepts        {}",
                rates
                    .iter()
                    .map(|r| format!("{}", *r as f64 / 1000.0))
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
            Err(err) => println!("    accepts        — ({err})"),
        }
        match coreaudio::physical_format(device) {
            Ok(format) => println!(
                "    PHYSICAL       {} Hz · {} · {} bit · {} ch",
                format.sample_rate, format.sample_format, format.bits_per_channel, format.channels
            ),
            Err(err) => println!("    PHYSICAL       — ({err})"),
        }
        match coreaudio::virtual_format(device) {
            Ok(format) => println!(
                "    virtual        {} Hz · {} · {} ch",
                format.sample_rate, format.sample_format, format.channels
            ),
            Err(err) => println!("    virtual        — ({err})"),
        }
        match coreaudio::hog_owner(device) {
            Ok(Some(pid)) => println!("    exclusive      held by pid {pid}"),
            Ok(None) => println!("    exclusive      free"),
            Err(err) => println!("    exclusive      — ({err})"),
        }
        if let (Some(rate), true) = (force_rate, info.is_default) {
            match coreaudio::set_nominal_sample_rate(device, rate) {
                Ok(()) => println!("    set nominal rate to {rate} Hz"),
                Err(err) => println!("    set rate failed: {err}"),
            }
        }
        if try_hog && info.is_default {
            println!("    --- exclusive access test ---");
            let before = coreaudio::nominal_sample_rate(device).ok();
            match coreaudio::take_hog(device) {
                Ok(pid) => {
                    println!("    took hog mode as pid {pid}");
                    for rate in [48_000u32, 96_000] {
                        match coreaudio::set_nominal_sample_rate(device, rate) {
                            Ok(()) => {
                                std::thread::sleep(std::time::Duration::from_millis(400));
                                match coreaudio::physical_format(device) {
                                    Ok(f) => println!(
                                        "    asked for {rate} Hz, hardware reports {} Hz · {}",
                                        f.sample_rate, f.sample_format
                                    ),
                                    Err(err) => println!("    readback failed: {err}"),
                                }
                            }
                            Err(err) => println!("    set {rate} Hz failed: {err}"),
                        }
                    }
                    if let Some(rate) = before {
                        let _ = coreaudio::set_nominal_sample_rate(device, rate);
                    }
                    match coreaudio::release_hog(device) {
                        Ok(()) => println!("    released hog mode"),
                        Err(err) => println!("    release failed: {err}"),
                    }
                }
                Err(err) => println!("    could not take hog mode: {err}"),
            }
        }
        println!();
    }
    Ok(())
}
