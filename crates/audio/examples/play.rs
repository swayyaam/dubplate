//! Drive the whole engine from the terminal: queue, transport, seek.
//!
//!     cargo run --release -p dubplate-audio --example play -- file.flac [more...]
//!
//! Exercises the path a user would: play, let it run, seek, pause, resume,
//! skip. The numbers to watch are position tracking wall clock and underruns
//! staying at zero.

use std::time::{Duration, Instant};

use dubplate_audio::engine::{queue_item, Command, Engine};

fn main() -> anyhow::Result<()> {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: play <file> [file...]");
        std::process::exit(2);
    }

    let engine = Engine::spawn();
    let items = paths
        .iter()
        .enumerate()
        .map(|(index, path)| queue_item(index as i64, path))
        .collect();

    engine.send(Command::SetQueue { items, start: 0 });
    engine.send(Command::SetVolume(0.35));

    settle(&engine);
    let state = engine.snapshot();
    println!("device      {}", state.device.as_deref().unwrap_or("—"));
    if let Some(source) = &state.source {
        println!(
            "source      {} {} Hz {} ch, depth {}",
            source.codec,
            source.sample_rate,
            source.channels,
            source
                .bits_per_sample
                .map(|b| b.to_string())
                .unwrap_or_else(|| "—".into())
        );
    }
    println!("duration    {:.1}s\n", state.duration_ms as f64 / 1000.0);

    let started = Instant::now();
    watch(&engine, 2, "playing", started);

    println!("\n-- seek to 60s --");
    engine.send(Command::Seek { ms: 60_000 });
    watch(&engine, 2, "after seek", started);

    println!("\n-- pause --");
    engine.send(Command::Pause);
    std::thread::sleep(Duration::from_millis(300));
    let paused_at = engine.snapshot().position_ms;
    std::thread::sleep(Duration::from_millis(700));
    let still = engine.snapshot().position_ms;
    println!(
        "  position held at {:.2}s (drift {} ms while paused)",
        still as f64 / 1000.0,
        still as i64 - paused_at as i64
    );

    println!("\n-- resume --");
    engine.send(Command::Play);
    watch(&engine, 2, "resumed", started);

    if paths.len() > 1 {
        println!("\n-- next track --");
        engine.send(Command::Next);
        settle(&engine);
        let state = engine.snapshot();
        println!(
            "  now on queue index {} (track {:?})",
            state.queue_index, state.track_id
        );
        watch(&engine, 2, "next track", started);
    }

    let final_state = engine.snapshot();
    println!("\nunderruns   {}", final_state.underruns);
    if let Some(error) = final_state.error {
        println!("error       {error}");
    }
    engine.send(Command::Stop);
    std::thread::sleep(Duration::from_millis(100));
    Ok(())
}

/// Give the control thread a moment to open the file and the device.
fn settle(engine: &Engine) {
    for _ in 0..100 {
        if engine.snapshot().source.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(150));
}

fn watch(engine: &Engine, seconds: u64, label: &str, started: Instant) {
    for _ in 0..seconds {
        std::thread::sleep(Duration::from_secs(1));
        let state = engine.snapshot();
        println!(
            "  {:<11} {:>7.2}s / {:>7.2}s   playing {:<5}  underruns {}  wall {:>5.1}s",
            label,
            state.position_ms as f64 / 1000.0,
            state.duration_ms as f64 / 1000.0,
            state.playing,
            state.underruns,
            started.elapsed().as_secs_f64(),
        );
    }
}
