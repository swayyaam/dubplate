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

**Phase 5 — correctness.** Exclusive access, sample rate switching, ReplayGain
applied, and a signal path panel wired to what the hardware actually reports.

Measured on a real 1,358 track library and a real output device:

| | |
|---|---|
| Cold index | 122 ms |
| Rescan, nothing changed | 4 ms |
| Search, per keystroke | 0.10 – 0.65 ms |
| Decode speed | 1500x – 10700x realtime |
| Underruns across a gapless transition | +0 |
| Underruns across an output device swap | +0 |
| Verdict, 44.1kHz FLAC played exclusively | bit-perfect |
| Verdict, same file at 35% volume | altered, 1 stage |

The audio callback is sacred: no allocation, no locks, no I/O. It drains the
ring, applies a 10ms gain ramp, updates one atomic frame counter, and does
nothing else.

The signal path panel is built on what CoreAudio says, not on what dubplate
asked for. That distinction is the whole point of it, and it caught a real lie
the first time it ran: the engine had been reporting its *requested* rate as the
device rate, while the hardware sat at 44.1kHz claiming 48.

Working today:

- Parallel walk, incremental sync, move detection, FTS5 search, filesystem watcher
- Artwork cache: WebP at 64/300/1000px, keyed by image content
- Symphonia decode of FLAC, WAV, AIFF, MP3, AAC, ALAC and Vorbis
- Lock-free SPSC ring, CoreAudio output, gapless, device switching, media keys
- Exclusive (hog) mode, per device, released on pause, on exit, and by the
  stream's own Drop
- Three rate modes: follow file, follow album, and a fixed rate with a
  high-quality FFT resampler
- ReplayGain applied as volume times gain, held back by the stored peak so a
  positive gain cannot clip
- Signal path panel: source, decoder, processing, output, and a verdict
- Album grid, album view, now playing, queue, waveform seek bar, command palette
- One accent colour, sampled from the current cover

Deliberately correct already:

- **Lossy codecs report no bit depth.** They decode to 32-bit float and have
  none to report; showing "16 bit" for an MP3 is meaningless.
- **32-bit integer and 32-bit float are reported separately.** Both are common
  and they are different things.
- **The device format is read back, never echoed.** An unreadable format is
  reported as unknown, not silently assumed to be what we asked for.
- **Format comes from the stream, never the tags.** ReplayGain is the exception,
  because it is metadata by definition.
- **A positive ReplayGain cannot clip**, because the stored peak limits it.
- **MP4 lossiness is unknown, not guessed.**
- **macOS packages are not walked into.** DAW sample libraries are not tracks.
- **Gain is ramped, never stepped.**
- **MP3 encoder delay and padding are trimmed**, so gapless is actually gapless.
- **The device is put back as it was found** — rate restored, exclusive access
  released — even if the app is quit mid-track.
- **Nothing is ever written to your audio files.**

Known gaps:

- ReplayGain has nothing to apply yet on a library whose files carry no tags:
  none of the 1,358 tracks in the test library have any. Phase 5 owns applying
  it; phase 6 computes the values.
- Crossfade is listed in the processing block and always reads "none". It shares
  most of gapless's machinery and lands after it.
- Waveform peaks are computed on demand for whatever is playing. Phase 6 folds
  them into the analysis pass.
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
| 6 | Analysis pipeline, transcode detection, collection health, smart playlists | next |

Phases 0 to 3 are a usable player, and they are done. Everything after is the
reason to keep going.

## Layout

```
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
