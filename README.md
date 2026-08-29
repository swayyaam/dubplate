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

**Phase 4 — looks right.** The interface pass: album covers as the front door, a
now-playing view built around the art, a waveform seek bar, and a command
palette. The accent colour is sampled from whatever is playing, so the whole
palette turns with the record.

Measured on a real 1,358 track library (mixed FLAC/WAV/MP3/M4A, 384 covers):

| | |
|---|---|
| Cold index | 122 ms |
| Rescan, nothing changed | 4 ms |
| Search, per keystroke | 0.10 – 0.65 ms |
| Decode speed | 1500x – 10700x realtime |
| Underruns across a gapless transition | +0 |
| Underruns across an output device swap | +0 |
| Index on disk | 896 KB |

The audio callback is sacred: no allocation, no locks, no I/O. It drains the
ring, applies a 10ms gain ramp, updates one atomic frame counter, and does
nothing else. Position is read from that counter, never from the decoder, which
runs ahead by whatever the ring holds.

Working today:

- Parallel walk with `jwalk`, tags with `lofty`, both across all cores
- Incremental sync: files whose `(mtime, size)` still match are never reopened
- Move detection by content key, so a rename keeps play counts
- FTS5 search with prefix matching and diacritic folding
- Debounced filesystem watcher that re-indexes after a burst settles
- Artwork cache: WebP at 64/300/1000px, keyed by image content
- Symphonia decode of FLAC, WAV, AIFF, MP3, AAC, ALAC and Vorbis
- Lock-free SPSC ring, CoreAudio output via `cpal`
- Gapless: the next track is pre-rolled into the same ring, and "now playing"
  flips when the listener reaches the join rather than when the decoder did
- Device switching that keeps the open file and its position
- Media keys and the macOS Now Playing panel via `souvlaki`
- Play counts, skip counts and listening history
- Album grid, album view, now playing, queue with drag to reorder
- Waveform seek bar drawn from the track's own peaks
- Command palette on `⌘K`: search every track, run every action
- One accent colour, sampled from the current cover
- Virtualized everywhere: 10,000 rows and thousands of covers at 60fps

Deliberately correct already, because they are easy to get wrong later:

- **Lossy codecs report no bit depth.** MP3, AAC and Vorbis store frequency
  coefficients rather than samples. They decode straight to 32-bit float, so
  bit depth stays null rather than showing a meaningless "16 bit".
- **Format comes from the stream, never the tags.**
- **MP4 lossiness is unknown, not guessed.** The container holds AAC or ALAC.
- **An album badge appears only when every track agrees.** A mixed album gets
  none, which beats picking whichever format sorted first.
- **macOS packages are not walked into.** DAW sample libraries are not tracks.
- **A malformed tag never hides a valid file.**
- **Gain is ramped, never stepped.**
- **MP3 encoder delay and padding are trimmed**, so gapless is actually gapless.
- **A greyscale sleeve yields no accent** rather than a muddy grey one.
- **Nothing is ever written to your audio files.** Play counts live in the index.

Known gaps, deliberately left for later phases:

- The output stream follows the file's sample rate. The fixed-rate and
  follow-album modes, and the resampler they need, are phase 5. Gapless across a
  rate change is therefore not possible, and the player falls back to an
  ordinary gapped change.
- The stream reports the configuration cpal negotiated, not what the hardware is
  running. A real bit-perfect verdict needs the device format read back, which
  is phase 5. The now-playing readout is a sketch of that panel, not the verdict.
- Waveform peaks are computed on demand for whatever is playing. Phase 6 folds
  them into the analysis pass, where one decode also feeds loudness, BPM, key
  and spectral analysis.
- Default-device *moves* are polled twice a second; a device *disappearing* is
  reported immediately through cpal's stream error callback.

## Roadmap

| Phase | What | State |
|---|---|---|
| 0 | Skeleton — folder picker, walk, table | done |
| 1 | SQLite index, incremental scan, move detection, FTS5, watcher, artwork cache | done |
| 2 | Symphonia decode, ring buffer, CoreAudio output, queue | done |
| 3 | Gapless, device switching, media keys, resume, play counts | done |
| 4 | The UI pass — art grid, now playing, command palette, waveform seek | done |
| 5 | Exclusive mode, sample rate switching, ReplayGain, signal path panel | next |
| 6 | Analysis pipeline, transcode detection, collection health, smart playlists | |

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
