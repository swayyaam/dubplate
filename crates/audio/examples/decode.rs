//! Decode real files and check the numbers add up.
//!
//!     cargo run --release -p dubplate-audio --example decode -- file.flac [more...]
//!
//! Reports what the stream says it is, decodes the whole thing, and compares
//! the frames produced against the frame count the container declared. A
//! mismatch means either the container is lying or we are dropping audio.

use std::path::PathBuf;
use std::time::Instant;

use dubplate_audio::decode::TrackDecoder;

fn main() -> anyhow::Result<()> {
    let paths: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("usage: decode <file> [file...]");
        std::process::exit(2);
    }

    for path in paths {
        println!("── {}", path.file_name().unwrap_or_default().to_string_lossy());
        match inspect(&path) {
            Ok(()) => {}
            Err(err) => println!("   FAILED: {err}"),
        }
        println!();
    }
    Ok(())
}

fn inspect(path: &std::path::Path) -> anyhow::Result<()> {
    let mut decoder = TrackDecoder::open(path)?;
    let format = decoder.format().clone();

    println!(
        "   codec {}   {} Hz   {} ch   depth {}   format {}",
        format.codec,
        format.sample_rate,
        format.channels,
        format
            .bits_per_sample
            .map(|b| b.to_string())
            // Lossy codecs genuinely have none; this is not a missing value.
            // Absent is meaningful for a lossy codec and merely unpopulated
            // for PCM, so do not label it as either here.
            .unwrap_or_else(|| "—".into()),
        format.sample_format.as_deref().unwrap_or("—"),
    );

    // Seek before draining the file, so "seek is broken" and "seek after EOF is
    // broken" are distinguishable failures.
    if let Some(declared) = format.total_frames {
        let target = declared / 3;
        match decoder.seek(target) {
            Ok(landed) => {
                let after = decoder.next_block().map(|b| b.map(|b| b.len()).unwrap_or(0));
                println!(
                    "   early seek to {target} landed at {landed}, next block {}",
                    match after {
                        Ok(len) => format!("{len} samples"),
                        Err(err) => format!("FAILED: {err}"),
                    }
                );
            }
            Err(err) => println!("   early seek FAILED: {err}"),
        }
        decoder.seek(0)?;
    }

    let started = Instant::now();
    let mut frames = 0u64;
    let mut peak = 0.0f32;
    let mut samples = 0u64;
    while let Some(block) = decoder.next_block()? {
        frames += (block.len() / format.channels.max(1) as usize) as u64;
        samples += block.len() as u64;
        for sample in block {
            peak = peak.max(sample.abs());
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    let seconds = frames as f64 / format.sample_rate as f64;

    println!(
        "   decoded {frames} frames ({seconds:.2}s) in {:.0}ms — {:.0}x realtime",
        elapsed * 1000.0,
        if elapsed > 0.0 { seconds / elapsed } else { 0.0 }
    );

    match format.total_frames {
        Some(declared) => {
            let delta = frames as i64 - declared as i64;
            println!(
                "   container declared {declared} frames, delta {delta:+} ({:.4}%)",
                delta as f64 / declared.max(1) as f64 * 100.0
            );
        }
        None => println!("   container declared no frame count"),
    }
    println!("   peak {peak:.4}   samples {samples}");
    if peak > 1.0 {
        println!("   note: peak above full scale, which is legal in float but will clip a device");
    }

    // Seek to the midpoint and confirm the reader lands near where we asked.
    if frames > 0 {
        let target = frames / 2;
        let landed = decoder.seek(target)?;
        let drift = landed as i64 - target as i64;
        println!(
            "   seek to {target} landed at {landed} (drift {drift:+} frames, {:.1}ms)",
            drift as f64 / format.sample_rate as f64 * 1000.0
        );
        let after = decoder.next_block()?.map(|b| b.len()).unwrap_or(0);
        println!("   first block after seek: {after} samples");
    }
    Ok(())
}
