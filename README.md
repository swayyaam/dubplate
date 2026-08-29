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

**Phase 0 — skeleton.** Pick a folder, walk it, list what is inside. No audio
playback yet.

Working today:

- Parallel filesystem walk with `jwalk`, tag reads with `lofty`, both across all cores
- Virtualized library table that holds 60fps at 10,000 rows
- Format badges — codec, bit depth, sample rate, bitrate
- Keyboard navigation, filter, last-folder persistence
- Unreadable files are reported, never fatal

Deliberately correct already, because they are easy to get wrong later:

- **Lossy codecs report no bit depth.** MP3, AAC and Vorbis store frequency
  coefficients rather than samples. They decode straight to 32-bit float.
  `bit_depth` stays null rather than showing a meaningless "16 bit".
- **File type comes from magic bytes, not the extension.** A collection
  assembled from many sources has mislabelled files in it.
- **MP4 lossiness is `unknown`.** The container holds AAC or ALAC and a tag-level
  read does not reliably say which. Phase 2 resolves it from Symphonia's
  `CodecParameters`. A wrong "lossless" badge is worse than an absent one.

## Roadmap

| Phase | What | State |
|---|---|---|
| 0 | Skeleton — folder picker, walk, table | done |
| 1 | SQLite index, incremental scan, move detection, FTS5, artwork cache | next |
| 2 | Symphonia decode, ring buffer, CoreAudio output, queue | |
| 3 | Gapless, device switching, media keys, resume, play counts | |
| 4 | The UI pass — art grid, now playing, command palette, waveform seek | |
| 5 | Exclusive mode, sample rate switching, ReplayGain, signal path panel | |
| 6 | Analysis pipeline, transcode detection, collection health, smart playlists | |

Phases 0 to 3 are a usable player. Everything after is the reason to keep going.

## Layout

```
crates/library/   filesystem scanning and (later) the SQLite index
src-tauri/        Tauri shell, commands, and the audio engine to come
src/              React frontend
```

## Development

Requires Rust and Node.

```bash
pnpm install
pnpm tauri:dev
```

Scan a folder from the terminal without launching the app:

```bash
cargo run --release -p dubplate-library --example scan -- ~/Music
```

Tests:

```bash
cargo test
pnpm typecheck
```

## Licence

MIT.
