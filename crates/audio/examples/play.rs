//! Drive the whole engine from the terminal: queue, transport, seek.
//!
//!     cargo run --release -p dubplate-audio --example play -- file.flac [more...]
//!
//! Exercises the path a user would: play, let it run, seek, pause, resume,
//! skip. The numbers to watch are position tracking wall clock and underruns
//! staying at zero.

use std::time::{Duration, Instant};

use dubplate_audio::engine::{queue_item, Command, Engine, OutputSettings, RateMode, SignalPath};

fn main() -> anyhow::Result<()> {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: play <file> [file...]");
        std::process::exit(2);
    }

    let gapless = paths.iter().any(|p| p == "--gapless");
    let exclusive = paths.iter().any(|p| p == "--exclusive");
    let paths: Vec<String> = paths
        .into_iter()
        .filter(|p| p != "--gapless" && p != "--exclusive")
        .collect();

    let engine = Engine::spawn();
    let items = paths
        .iter()
        .enumerate()
        .map(|(index, path)| queue_item(index as i64, path))
        .collect();

    engine.send(Command::SetQueue { items, start: 0 });
    engine.send(Command::SetVolume(if exclusive { 1.0 } else { 0.35 }));
    if exclusive {
        // Exclusive plus follow-file is the combination the design document
        // calls the best-fidelity path: the device runs at the file's rate and
        // nothing else on the machine can move it.
        engine.send(Command::SetOutputSettings(OutputSettings {
            exclusive: true,
            rate_mode: RateMode::FollowFile,
            replay_gain: true,
        }));
    }

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
    println!("duration    {:.1}s", state.duration_ms as f64 / 1000.0);
    // Let the device settle after any rate change before reading it back.
    std::thread::sleep(Duration::from_millis(600));
    let snap = engine.snapshot();
    if let Some(err) = &snap.error {
        println!("error       {err}");
    }
    println!(
        "settings    exclusive={} rate_mode={:?}",
        snap.settings.exclusive, snap.settings.rate_mode
    );
    if let Some(signal) = snap.signal {
        print_signal(&signal);
    }

    if gapless {
        return watch_gapless(&engine);
    }

    let started = Instant::now();
    watch(&engine, 2, "playing", started);

    println!("\n-- seek to 60s --");
    engine.send(Command::Seek { ms: 60_000 });
    watch(&engine, 2, "after seek", started);

    println!("\n-- reopen output (the device-switch path) --");
    let before = engine.snapshot();
    engine.send(Command::ReopenOutput);
    std::thread::sleep(Duration::from_millis(400));
    let after = engine.snapshot();
    println!(
        "  position {:.2}s -> {:.2}s across the swap, playing {}, underruns {} -> {}",
        before.position_ms as f64 / 1000.0,
        after.position_ms as f64 / 1000.0,
        after.playing,
        before.underruns,
        after.underruns
    );
    watch(&engine, 1, "after swap", started);

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

/// Park just before the end of the first track and watch the join.
///
/// The thing to see is the track id flipping while position restarts at zero,
/// with the underrun count unchanged: audio ran straight through.
fn watch_gapless(engine: &Engine) -> anyhow::Result<()> {
    let state = engine.snapshot();
    let lead_in = 6_000;
    let target = state.duration_ms.saturating_sub(lead_in);
    println!("-- seeking to {:.1}s, {:.0}s before the end --", target as f64 / 1000.0, lead_in as f64 / 1000.0);
    engine.send(Command::Seek { ms: target });
    std::thread::sleep(Duration::from_millis(300));

    let before = engine.snapshot();
    let mut last_track = before.track_id;
    let underruns_before = before.underruns;
    println!(
        "   on track {:?}, {:.2}s / {:.2}s, underruns {}\n",
        last_track,
        before.position_ms as f64 / 1000.0,
        before.duration_ms as f64 / 1000.0,
        underruns_before
    );

    let deadline = Instant::now() + Duration::from_secs(12);
    let mut flipped_at: Option<(f64, Option<i64>)> = None;
    let start = Instant::now();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        let now = engine.snapshot();
        if now.track_id != last_track {
            flipped_at = Some((start.elapsed().as_secs_f64(), now.track_id));
            println!(
                "   boundary crossed at wall {:.1}s -> track {:?}, position {:.2}s, underruns {}",
                start.elapsed().as_secs_f64(),
                now.track_id,
                now.position_ms as f64 / 1000.0,
                now.underruns
            );
            last_track = now.track_id;
            break;
        }
    }

    std::thread::sleep(Duration::from_secs(2));
    let after = engine.snapshot();
    println!(
        "\n   two seconds later: track {:?}, position {:.2}s",
        after.track_id,
        after.position_ms as f64 / 1000.0
    );
    println!(
        "   underruns {} -> {} ({:+})",
        underruns_before,
        after.underruns,
        after.underruns as i64 - underruns_before as i64
    );
    match flipped_at {
        Some(_) => println!("   gapless transition: OK"),
        None => println!("   gapless transition: NEVER HAPPENED"),
    }
    engine.send(Command::Stop);
    std::thread::sleep(Duration::from_millis(100));
    Ok(())
}

/// The four blocks the design document specifies, and the verdict.
fn print_signal(signal: &SignalPath) {
    println!("\n── signal path ──");
    println!(
        "  SOURCE      {} · {} Hz · {} · {} ch",
        signal.source.codec.to_uppercase(),
        signal.source.sample_rate,
        signal
            .source
            .bits_per_sample
            .map(|b| format!("{b} bit"))
            // Not a missing value: lossy codecs have no bit depth at all.
            .unwrap_or_else(|| "no bit depth (lossy)".into()),
        signal.source.channels,
    );
    println!(
        "  DECODER     {} Hz · {}",
        signal.decoder_sample_rate, signal.decoder_format
    );
    println!("  PROCESSING");
    for stage in &signal.processing {
        match (&stage.active, &stage.detail) {
            (true, Some(detail)) => println!("    {:<12} {}", stage.name, detail),
            (true, None) => println!("    {:<12} active", stage.name),
            (false, _) => println!("    {:<12} —", stage.name),
        }
    }
    match &signal.device_format {
        Some(format) => println!(
            "  OUTPUT      {} · {} Hz · {} · {} ch · {}",
            signal.device_name.as_deref().unwrap_or("—"),
            format.sample_rate,
            format.sample_format,
            format.channels,
            if signal.exclusive { "exclusive" } else { "shared" },
        ),
        None => println!(
            "  OUTPUT      {} · format unknown",
            signal.device_name.as_deref().unwrap_or("—")
        ),
    }
    if signal.bit_perfect {
        println!("\n  VERDICT     bit-perfect");
    } else {
        println!(
            "\n  VERDICT     altered, {} stage{} — {}",
            signal.altered_stages,
            if signal.altered_stages == 1 { "" } else { "s" },
            signal.reason.as_deref().unwrap_or("")
        );
    }
}
