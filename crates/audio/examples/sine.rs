//! Prove the output path before a decoder exists.
//!
//!     cargo run --release -p dubplate-audio --example sine
//!     cargo run --release -p dubplate-audio --example sine -- --direct
//!
//! Default mode pushes a tone through the real ring buffer and the real
//! callback, which is the arrangement a decoder will later use. `--direct`
//! skips the ring and generates in the callback, to separate "the device works"
//! from "the ring works" if something is wrong.
//!
//! The check that matters is not audible: if `frames played` tracks elapsed time
//! at the device rate and underruns stay at zero, the path is live and keeping
//! up. A silent bug shows here as a frame counter that does not move.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dubplate_audio::backend::CpalBackend;
use dubplate_audio::device::{AudioBackend, StreamRequest};
use dubplate_audio::ring::{self, PlaybackShared};
use dubplate_audio::sine::SineRenderer;

const FREQUENCY: f32 = 440.0;
const AMPLITUDE: f32 = 0.2;
const SECONDS: u64 = 3;

fn main() -> anyhow::Result<()> {
    let direct = std::env::args().any(|arg| arg == "--direct");
    let backend = CpalBackend::new();

    println!("backend         {}", backend.name());
    for device in backend.enumerate()? {
        println!(
            "  {} {:<38} {:>2}ch  {}",
            if device.is_default { "*" } else { " " },
            truncate(&device.name, 38),
            device.max_channels,
            summarise_rates(&device.sample_rates),
        );
    }

    let device = backend.default_device()?;
    let rate = if device.sample_rates.contains(&48_000) { 48_000 } else { 44_100 };
    let channels = device.max_channels.min(2).max(1);
    println!("\nopening         {} at {} Hz, {} ch", device.name, rate, channels);

    let shared = Arc::new(PlaybackShared::new());
    shared.set_target_gain(1.0);
    shared.set_playing(true);

    let stop = Arc::new(AtomicBool::new(false));
    let feeder = if direct {
        None
    } else {
        let (mut producer, _boundaries, renderer) = ring::open(rate, channels, Arc::clone(&shared));
        let stop_feeder = Arc::clone(&stop);
        // Stands in for the decode thread: keep the ring fed, never block the
        // callback, and back off when there is no room.
        let handle = std::thread::spawn(move || {
            let mut phase = 0.0f32;
            let increment = std::f32::consts::TAU * FREQUENCY / rate as f32;
            while !stop_feeder.load(Ordering::Relaxed) {
                let free = producer.slots();
                if free < channels as usize {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                for _ in 0..(free / channels as usize) {
                    let sample = phase.sin() * AMPLITUDE;
                    phase += increment;
                    if phase >= std::f32::consts::TAU {
                        phase -= std::f32::consts::TAU;
                    }
                    for _ in 0..channels {
                        let _ = producer.push(sample);
                    }
                }
            }
        });
        Some((handle, renderer))
    };

    let request = StreamRequest {
        sample_rate: rate,
        channels,
        buffer_frames: None,
    };

    let complain: dubplate_audio::device::ErrorSink =
        Arc::new(|err| eprintln!("stream error: {err}"));
    let mut stream = match feeder {
        Some((_, renderer)) => {
            backend.open(&device.id, request, Box::new(renderer), complain)?
        }
        None => backend.open(
            &device.id,
            request,
            Box::new(SineRenderer::new(FREQUENCY, rate, AMPLITUDE)),
            complain,
        )?,
    };

    let info = stream.info().clone();
    println!(
        "stream          {} Hz, {} ch, exclusive: {}",
        info.sample_rate, info.channels, info.exclusive
    );
    println!("mode            {}\n", if direct { "direct (no ring)" } else { "through ring buffer" });

    stream.play()?;
    let started = Instant::now();
    for _ in 0..SECONDS {
        std::thread::sleep(Duration::from_secs(1));
        let elapsed = started.elapsed().as_secs_f64();
        let frames = shared.frames_played();
        let expected = elapsed * rate as f64;
        println!(
            "  {:>4.1}s  frames {:>9}  expected {:>9.0}  drift {:>+6.2}%  underruns {}",
            elapsed,
            frames,
            expected,
            if expected > 0.0 { (frames as f64 - expected) / expected * 100.0 } else { 0.0 },
            shared.underruns(),
        );
    }
    stream.pause()?;
    stop.store(true, Ordering::Relaxed);

    println!("\nunderruns       {}", shared.underruns());
    if direct {
        println!("note            direct mode does not use the ring, so frames stay at 0");
    }
    Ok(())
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else {
        text.chars().take(width - 1).collect::<String>() + "…"
    }
}

fn summarise_rates(rates: &[u32]) -> String {
    if rates.is_empty() {
        return "rates unknown".into();
    }
    let shown: Vec<String> = rates.iter().take(6).map(|r| format!("{}", r / 1000)).collect();
    format!("{} kHz", shown.join("/"))
}
