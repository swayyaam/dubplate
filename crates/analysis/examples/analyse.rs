//! Analyse real files and print what came out.
//!
//!     cargo run --release -p dubplate-analysis --example analyse -- file.flac ...
//!     cargo run --release -p dubplate-analysis --example analyse -- --sweep ~/Music/DJ
//!
//! The sweep is the check that matters: transcode scoring is only trustworthy
//! if it stays quiet on a library of genuine files.

use std::path::{Path, PathBuf};
use std::time::Instant;

use dubplate_analysis::analyse;
use rayon::prelude::*;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|a| a == "--sweep").unwrap_or(false) {
        let root = args.get(1).map(|r| expand(r)).unwrap_or_default();
        return sweep(Path::new(&root));
    }
    if args.first().map(|a| a == "--pipeline").unwrap_or(false) {
        let root = args.get(1).map(|r| expand(r)).unwrap_or_default();
        return pipeline_run(Path::new(&root));
    }
    for arg in &args {
        report(Path::new(&expand(arg)));
    }
    Ok(())
}

fn report(path: &Path) {
    println!("── {}", path.file_name().unwrap_or_default().to_string_lossy());
    let started = Instant::now();
    match analyse(path) {
        Ok(result) => {
            println!(
                "   loudness   {}  gain {}  true peak {}",
                result.loudness_lufs.map(|v| format!("{v:.2} LUFS")).unwrap_or_else(|| "—".into()),
                result.replay_gain_db.map(|v| format!("{v:+.2} dB")).unwrap_or_else(|| "—".into()),
                result.true_peak.map(|v| format!("{v:.3}")).unwrap_or_else(|| "—".into()),
            );
            println!(
                "   depth      declared {}  effective {}{}",
                result.declared_bits.map(|v| v.to_string()).unwrap_or_else(|| "—".into()),
                result.effective_bits.map(|v| v.to_string()).unwrap_or_else(|| "—".into()),
                if result.is_padded() { "   PADDED" } else { "" },
            );
            println!(
                "   spectrum   cutoff {}  rolloff {:.1} dB  transcode score {:.2}",
                result.spectral_cutoff.map(|v| format!("{} Hz", v)).unwrap_or_else(|| "—".into()),
                result.rolloff_db,
                result.transcode_score,
            );
            println!(
                "   musical    {} BPM (confidence {:.2})   key {}",
                result.bpm.map(|v| format!("{v:.1}")).unwrap_or_else(|| "—".into()),
                result.bpm_confidence,
                result
                    .key
                    .as_ref()
                    .map(|k| format!("{} ({}, confidence {:.2})", k.camelot, k.name, k.confidence))
                    .unwrap_or_else(|| "—".into()),
            );
            println!(
                "   took       {:.0} ms for {:.1}s of audio",
                started.elapsed().as_secs_f64() * 1000.0,
                result.duration_ms as f64 / 1000.0
            );
        }
        Err(err) => println!("   FAILED: {err}"),
    }
    println!();
}

/// Run the whole folder and summarise, which is the collection health view in
/// terminal form.
fn sweep(root: &Path) -> anyhow::Result<()> {
    let files: Vec<PathBuf> = dubplate_library::scan::walk(root)
        .into_iter()
        .map(|entry| entry.path)
        .collect();
    println!("analysing {} files with {} threads\n", files.len(), rayon::current_num_threads());

    let started = Instant::now();
    let results: Vec<(PathBuf, dubplate_analysis::TrackAnalysis)> = files
        .par_iter()
        .filter_map(|path| analyse(path).ok().map(|result| (path.clone(), result)))
        .collect();
    let elapsed = started.elapsed();

    let mut padded = Vec::new();
    let mut suspicious = Vec::new();
    let mut lossless_cutoffs = Vec::new();
    let mut gains = Vec::new();
    let mut keyed = 0usize;
    let mut tempoed = 0usize;

    for (path, result) in &results {
        if result.is_padded() {
            padded.push((path, result));
        }
        // Only lossless files can be "suspected transcodes": an MP3 is not a
        // suspicion, it is simply lossy and says so.
        if result.declared_bits.is_some() {
            if let Some(cutoff) = result.spectral_cutoff {
                lossless_cutoffs.push(cutoff);
            }
            if result.transcode_score >= 0.5 {
                suspicious.push((path, result));
            }
        }
        if let Some(gain) = result.replay_gain_db {
            gains.push(gain);
        }
        if result.key.is_some() {
            keyed += 1;
        }
        if result.bpm_confidence > 0.25 {
            tempoed += 1;
        }
    }

    let audio_seconds: f64 = results.iter().map(|(_, r)| r.duration_ms as f64 / 1000.0).sum();
    println!("analysed        {} of {} files", results.len(), files.len());
    println!(
        "elapsed         {:.1}s for {:.1} hours of audio ({:.0}x realtime)",
        elapsed.as_secs_f64(),
        audio_seconds / 3600.0,
        audio_seconds / elapsed.as_secs_f64().max(0.001)
    );
    println!("\nreplaygain      {} tracks measured", gains.len());
    if !gains.is_empty() {
        let mean = gains.iter().sum::<f32>() / gains.len() as f32;
        let min = gains.iter().copied().fold(f32::INFINITY, f32::min);
        let max = gains.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        println!("                {min:+.1} to {max:+.1} dB, mean {mean:+.1} dB");
    }
    println!("tempo           {tempoed} with usable confidence");
    println!("key             {keyed} estimated");

    println!("\npadded depth    {} files", padded.len());
    for (path, result) in padded.iter().take(8) {
        println!(
            "   {} — {} bit container, {} bit content",
            short(path),
            result.declared_bits.unwrap_or(0),
            result.effective_bits.unwrap_or(0)
        );
    }

    println!("\nsuspected transcodes (lossless containers only)  {}", suspicious.len());
    let mut sorted = suspicious.clone();
    sorted.sort_by(|a, b| b.1.transcode_score.partial_cmp(&a.1.transcode_score).unwrap());
    for (path, result) in sorted.iter().take(12) {
        println!(
            "   {:.2}  cutoff {:>6} Hz ({:>5.1}% of nyquist)  rolloff {:>5.1} dB  {}",
            result.transcode_score,
            result.spectral_cutoff.unwrap_or(0),
            result.spectral_cutoff.unwrap_or(0) as f32 / (result.sample_rate as f32 / 2.0) * 100.0,
            result.rolloff_db,
            short(path)
        );
    }

    if !lossless_cutoffs.is_empty() {
        lossless_cutoffs.sort_unstable();
        let at = |q: f64| lossless_cutoffs[((lossless_cutoffs.len() - 1) as f64 * q) as usize];
        println!(
            "\nlossless cutoff percentiles  p10 {} Hz  p50 {} Hz  p90 {} Hz",
            at(0.1),
            at(0.5),
            at(0.9)
        );
    }
    Ok(())
}

fn short(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().chars().take(58).collect()
}

fn expand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var("HOME").map(|h| format!("{h}/{rest}")).unwrap_or_else(|_| path.into()),
        None => path.into(),
    }
}

/// Index a folder, then analyse it in batches the way the app does, and check
/// the results actually landed in the database.
fn pipeline_run(root: &Path) -> anyhow::Result<()> {
    use dubplate_analysis::{pipeline, PeaksCache};
    use dubplate_library::{index, Library};

    // Stable rather than per-process: running this twice should demonstrate
    // resumability rather than redoing everything.
    let work = std::env::temp_dir().join("dubplate-analysis-workdir");
    std::fs::create_dir_all(&work)?;
    let mut library = Library::open(work.join("library.sqlite"))?;
    let peaks = PeaksCache::new(work.join("waveforms"));

    let sync = index::sync(&mut library, root)?;
    println!("indexed         {} tracks in {} ms", sync.added, sync.elapsed_ms);
    println!("to analyse      {}\n", pipeline::remaining(&library)?);

    let started = Instant::now();
    let mut batches = 0usize;
    loop {
        let report = pipeline::run_batch(&mut library, &peaks, 32, 6)?;
        if report.analysed == 0 && report.failed == 0 {
            break;
        }
        batches += 1;
        if batches % 10 == 0 {
            println!("  {} remaining after {} batches", report.remaining, batches);
        }
        if report.remaining == 0 {
            break;
        }
    }
    println!("\nanalysed all in {:.1}s over {batches} batches", started.elapsed().as_secs_f64());

    // Resumability: with nothing left, another pass must do no work at all.
    let again = pipeline::run_batch(&mut library, &peaks, 32, 6)?;
    println!("second pass     {} analysed, {} failed (resumable)", again.analysed, again.failed);

    let conn = library.connection();
    let row: (i64, i64, i64, i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(rg_track_gain IS NOT NULL), 0),
                COALESCE(SUM(bpm IS NOT NULL), 0),
                COALESCE(SUM(music_key IS NOT NULL), 0),
                COALESCE(SUM(effective_bits IS NOT NULL), 0),
                COALESCE(SUM(analyzed_at IS NOT NULL), 0)
         FROM tracks",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    println!(
        "\nstored          replaygain {}  bpm {}  key {}  depth {}  analysed {}",
        row.0, row.1, row.2, row.3, row.4
    );

    let health = dubplate_library::health::summary(&library)?;
    println!("\n== collection health ==");
    println!("total           {}", health.total);
    println!("lossless        {}", health.lossless);
    println!("lossy           {}", health.lossy);
    println!("unknown         {}", health.unknown);
    println!("padded          {}", health.padded);
    println!("suspected       {}", health.suspected);
    println!("sample rates    {:?}", health.sample_rates.iter().map(|b| (&b.label, b.count)).collect::<Vec<_>>());
    println!("bit depths      {:?}", health.bit_depths.iter().map(|b| (&b.label, b.count)).collect::<Vec<_>>());

    let waveforms = std::fs::read_dir(peaks.root())
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| std::fs::read_dir(e.path()).ok().map(|d| d.count()))
                .sum::<usize>()
        })
        .unwrap_or(0);
    println!("\nwaveforms cached {waveforms}");
    println!("workdir          {}", work.display());

    // Optional fixture dump, so the interface can be looked at with real
    // numbers rather than invented ones.
    if let Some(out) = std::env::args().nth(3).filter(|a| !a.starts_with('-')) {
        let dir = PathBuf::from(expand(&out));
        if dir.is_dir() {
            std::fs::write(dir.join("dev-health.json"), serde_json::to_string(&health)?)?;
            let tracks = dubplate_library::query::list_tracks(&library)?;
            std::fs::write(dir.join("dev-tracks.json"), serde_json::to_string(&tracks)?)?;
            println!("wrote fixtures   {}", dir.display());
        }
    }

    // A set built from the most-played track, which is what the data is for.
    let seed: Option<i64> = library
        .connection()
        .query_row(
            "SELECT id FROM tracks WHERE bpm IS NOT NULL AND music_key IS NOT NULL
             ORDER BY duration_ms DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok();
    if let Some(seed) = seed {
        println!("\n== a set that flows from this track ==");
        for (index, step) in dubplate_library::flow::build_set(&library, seed, 10)?.iter().enumerate() {
            println!(
                "{:>2}. {:<52} {}",
                index + 1,
                step.track
                    .title
                    .clone()
                    .unwrap_or_else(|| step.track.file_name.clone())
                    .chars()
                    .take(52)
                    .collect::<String>(),
                step.reason
            );
        }
    }
    Ok(())
}
