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

**Phase 1 — index.** The library is a SQLite index over the filesystem, kept in
sync automatically. Still no audio playback.

Measured against a real 1,419 track library (36 GB, mixed FLAC/WAV/MP3/M4A):

| | |
|---|---|
| Cold index | 122 ms |
| Rescan, nothing changed | 4 ms |
| `list_tracks`, 1,419 rows | 1 ms |
| Search, per keystroke | 0.10 – 0.65 ms |
| Index on disk | 896 KB |
| Artwork cache | 388 covers, 32 MB |

Working today:

- Parallel walk with `jwalk`, tags with `lofty`, both across all cores
- Incremental sync: files whose `(mtime, size)` still match are never reopened
- Move detection by content key, so a rename or a reorganised folder keeps play counts
- FTS5 search with prefix matching and diacritic folding, ranked by match quality, play count and recency
- Debounced filesystem watcher that re-indexes after a burst of changes settles
- Artwork cache: pre-resized WebP at 64/300/1000px, keyed by image content so duplicate covers collapse
- Virtualized table that holds 60fps at 10,000 rows

Deliberately correct already, because they are easy to get wrong later:

- **Lossy codecs report no bit depth.** MP3, AAC and Vorbis store frequency
  coefficients rather than samples. They decode straight to 32-bit float, so
  `bit_depth` stays null rather than showing a meaningless "16 bit".
- **File type comes from magic bytes, not the extension.** A collection
  assembled from many sources has mislabelled files in it.
- **MP4 lossiness is unknown, not guessed.** The container holds AAC or ALAC and
  a tag-level read cannot reliably say which, so `is_lossy` is NULL rather than
  claiming lossless. Phase 2 resolves it from Symphonia's `CodecParameters`.
- **macOS packages are not walked into.** `Logic Pro Library.bundle` and DAW
  project packages hold thousands of one-second samples that are not tracks.
- **A malformed tag never hides a valid file.** Reads retry in relaxed mode
  before a file is reported unreadable.
- **Rewriting a file discards its stored analysis.** Loudness, BPM and spectral
  figures describe bytes that are gone.

## Roadmap

| Phase | What | State |
|---|---|---|
| 0 | Skeleton — folder picker, walk, table | done |
| 1 | SQLite index, incremental scan, move detection, FTS5, watcher, artwork cache | done |
| 2 | Symphonia decode, ring buffer, CoreAudio output, queue | next |
| 3 | Gapless, device switching, media keys, resume, play counts | |
| 4 | The UI pass — art grid, now playing, command palette, waveform seek | |
| 5 | Exclusive mode, sample rate switching, ReplayGain, signal path panel | |
| 6 | Analysis pipeline, transcode detection, collection health, smart playlists | |

Phases 0 to 3 are a usable player. Everything after is the reason to keep going.

## Layout

```
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

Tests:

```bash
cargo test
pnpm typecheck
```

## Licence

MIT.
