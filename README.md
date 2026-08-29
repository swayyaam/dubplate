# dubplate

A minimal, art-forward desktop music player for a personal lossless collection.
macOS first. Rust core, Tauri shell. Built for people who own their music rather
than rent it.

Named after the one-off acetate cut for a soundsystem: the exclusive original,
before any copy degrades it. Which is the whole point of the player.

## The premise

Streaming apps feel good and sound mediocre. Local players sound good and feel
like 2009. This is the third option: playback that is technically correct
(bit-perfect, gapless, device-aware) inside an interface that is actually
designed.

See [proj-desc.md](proj-desc.md) for the full design document — architecture,
schema, audio engine, and the reasoning behind it.

## Status

**Phase 6 — smart.** The analysis pipeline, format verification, the collection
health view, smart playlists, and sets built from tempo and key. Every phase in
the plan is done.

Measured on a real 1,358 track library:

| | |
|---|---|
| Cold index | 122 ms |
| Rescan, nothing changed | 4 ms |
| Search, per keystroke | 0.10 – 0.65 ms |
| Full analysis, 81.4 hours of audio | 96 s (≈3,000x realtime) |
| Underruns across a gapless transition | +0 |
| Underruns across an output device swap | +0 |
| Verdict, played exclusively at the file's rate | bit-perfect |

The audio callback is sacred: no allocation, no locks, no I/O. It drains the
ring, applies a 10ms gain ramp, updates one atomic frame counter, and does
nothing else.

Working today:

- Parallel walk, incremental sync, move detection, FTS5 search, filesystem watcher
- Artwork cache: WebP at 64/300/1000px, keyed by image content
- Symphonia decode of FLAC, WAV, AIFF, MP3, AAC, ALAC and Vorbis
- Lock-free SPSC ring, CoreAudio output, gapless, device switching, media keys
- Exclusive (hog) mode, three sample-rate modes, a high-quality FFT resampler
- ReplayGain measured and applied, held back by true peak so it cannot clip
- Signal path panel built on what the hardware reports, with a verdict
- Analysis pipeline: one decode pass gives loudness, waveform peaks, effective
  bit depth, spectral cutoff, transcode score, tempo and key
- Collection health: what is lossless, what is padded, what looks like a
  transcode, and the distribution of formats, rates and depths
- Smart playlists whose rules are stored rather than their contents
- "Build a set that flows from this track", using tempo, key and loudness
- Album grid, album view, now playing, queue, waveform seek bar, command palette
- One accent colour, sampled from the current cover

Deliberately correct, because each is easy to get wrong:

- **Lossy codecs report no bit depth.** They decode to 32-bit float and have
  none; showing "16 bit" for an MP3 is meaningless.
- **32-bit integer and 32-bit float are reported separately.**
- **The device format is read back, never echoed.** Unknown is reported as
  unknown, not assumed to be what we asked for.
- **Format comes from the stream, never the tags.** ReplayGain is the exception,
  because it is metadata by definition.
- **A positive ReplayGain cannot clip**, because the measured peak limits it.
- **A transcode score is a suspicion with its evidence attached.** The cutoff and
  rolloff that produced it are stored and shown, and nothing is hidden, moved or
  deleted on the strength of one.
- **One dithered sample proves a file is not padded**, however much of it looks
  like it is.
- **A tempo the search cannot see is not invented.** Nothing is reported rather
  than a number that came from noise.
- **Smart playlist rules never reach the SQL text.** Fields and operators are
  enums and values are bound, so a rule containing a percent sign searches for a
  percent sign.
- **MP3 encoder delay and padding are trimmed**, so gapless is actually gapless.
- **The device is put back as it was found** on exit, rate and all.
- **Nothing is ever written to your audio files.**

Known gaps:

- Crossfade appears in the signal path and always reads "none". It shares most
  of gapless's machinery and was never a phase in the plan.
- Smart playlists come as seven ready-made rule sets rather than a visual rule
  editor. The engine takes arbitrary rules; only the editor is missing.
- Tempo and key are the honest 80% the design document asks for, reported with a
  confidence rather than as fact.
- Default-device *moves* are polled twice a second, using
  `kAudioHardwarePropertyDefaultOutputDevice`; a device *disappearing* is
  reported immediately through cpal's stream error callback.

## Roadmap

| Phase | What | State |
|---|---|---|
| 0 | Skeleton — folder picker, walk, table | done |
| 1 | SQLite index, incremental scan, move detection, FTS5, watcher, artwork cache | done |
| 2 | Symphonia decode, ring buffer, CoreAudio output, queue | done |
| 3 | Gapless, device switching, media keys, resume, play counts | done |
| 4 | The UI pass — art grid, now playing, command palette, waveform seek | done |
| 5 | Exclusive mode, sample rate switching, ReplayGain, signal path panel | done |
| 6 | Analysis pipeline, transcode detection, collection health, smart playlists | done |

Phases 0 to 3 were the usable player. Everything after was the reason to keep
going, and it is all here.

## Layout

```
crates/analysis/  loudness, peaks, tempo, key, and format verification
crates/audio/     ring buffer, output backends, decoding, the engine threads
crates/library/   filesystem scanning, SQLite index, search, artwork cache, watcher
src-tauri/        Tauri shell, commands, and the audio engine to come
src/              React frontend
```

## Development

Requires Rust and Node.

```bash
pnpm install
pnpm tauri:dev
```

Inspect a folder from the terminal without launching the app:

```bash
cargo run --release -p dubplate-library --example scan -- ~/Music
```

Exercise the whole index pipeline, including a warm rescan and search timings:

```bash
cargo run --release -p dubplate-library --example index -- ~/Music
```

Prove the output path with a tone, before any decoder is involved:

```bash
cargo run --release -p dubplate-audio --example sine
```

Drive the engine from the terminal — play, seek, swap output device, skip:

```bash
cargo run --release -p dubplate-audio --example play -- ~/Music/some-track.flac
```

Watch a gapless join between two tracks at the same sample rate:

```bash
cargo run --release -p dubplate-audio --example play -- first.flac second.flac --gapless
```

Ask the hardware what it is actually doing, and test exclusive access:

```bash
cargo run --release -p dubplate-audio --example device
cargo run --release -p dubplate-audio --example device -- --hog
```

Play a track exclusively, at the file's own rate, and print the signal path:

```bash
cargo run --release -p dubplate-audio --example play -- track.flac --exclusive
```

Analyse a file, or sweep a whole folder and see the distribution:

```bash
cargo run --release -p dubplate-analysis --example analyse -- track.flac
cargo run --release -p dubplate-analysis --example analyse -- --sweep ~/Music
```

See the accent colour each cached cover produces:

```bash
cargo run --release -p dubplate-library --example accents -- \
  ~/Library/Application\ Support/com.swayammishra.dubplate/artwork
```

Tests:

```bash
cargo test
pnpm typecheck
```

## Licence

MIT.
