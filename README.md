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

**Phase 2 — playback.** Symphonia decoding, a lock-free ring buffer, CoreAudio
output, queue and transport, and state that survives a relaunch.

Measured on a real 1,419 track library (36 GB, mixed FLAC/WAV/MP3/M4A):

| | |
|---|---|
| Cold index | 122 ms |
| Rescan, nothing changed | 4 ms |
| Search, per keystroke | 0.10 – 0.65 ms |
| Decode speed | 1500x – 10700x realtime |
| Underruns, full playback session | 1 |
| Index on disk | 896 KB |
| Artwork cache | 388 covers, 32 MB |

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
- Lock-free SPSC ring, CoreAudio output via `cpal`, gapless-ready generation counter
- Queue, shuffle, repeat, seek, volume, and a transport that resumes after a relaunch
- Virtualized table that holds 60fps at 10,000 rows

Deliberately correct already, because they are easy to get wrong later:

- **Lossy codecs report no bit depth.** MP3, AAC and Vorbis store frequency
  coefficients rather than samples. They decode straight to 32-bit float, so
  bit depth stays null rather than showing a meaningless "16 bit".
- **Format comes from the stream, never the tags.** `lofty` reads tags for the
  library table; Symphonia's codec parameters are the authority for what a file
  actually is.
- **MP4 lossiness is unknown, not guessed.** The container holds AAC or ALAC.
- **macOS packages are not walked into.** DAW sample libraries are not tracks.
- **A malformed tag never hides a valid file.**
- **Rewriting a file discards its stored analysis.**
- **Gain is ramped, never stepped.** A volume change applied between one sample
  and the next is a step discontinuity, which is audible as a click.

Known gaps, deliberately left for later phases:

- The output stream follows the file's sample rate. The fixed-rate and
  follow-album modes, and the resampler they need, are phase 5.
- The stream reports the configuration cpal negotiated, not what the hardware is
  running. A real bit-perfect verdict needs the device format read back, which
  is also phase 5.
- Device switching, gapless and media keys are phase 3.

## Roadmap

| Phase | What | State |
|---|---|---|
| 0 | Skeleton — folder picker, walk, table | done |
| 1 | SQLite index, incremental scan, move detection, FTS5, watcher, artwork cache | done |
| 2 | Symphonia decode, ring buffer, CoreAudio output, queue | done |
| 3 | Gapless, device switching, media keys, resume, play counts | next |
| 4 | The UI pass — art grid, now playing, command palette, waveform seek | |
| 5 | Exclusive mode, sample rate switching, ReplayGain, signal path panel | |
| 6 | Analysis pipeline, transcode detection, collection health, smart playlists | |

Phases 0 to 3 are a usable player. Everything after is the reason to keep going.

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

Drive the engine from the terminal — play, seek, pause, skip:

```bash
cargo run --release -p dubplate-audio --example play -- ~/Music/some-track.flac
```

Tests:

```bash
cargo test
pnpm typecheck
```

## Licence

MIT.
