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
- Analysis pipeline: one decode pass gives loudness, the waveform, effective
  bit depth, spectral cutoff, transcode score, tempo and key
- Collection health: what is lossless, what is padded, what looks like a
  transcode, and the distribution of formats, rates and depths
- Tag editor: one track or many, writing to the files themselves
- Tags from filenames, in bulk, previewed before anything is written
- Undo, for the last twenty-five tag writes, covers included
- Smart playlists whose rules are stored rather than their contents
- "Build a set that flows from this track", using tempo, key and loudness
- Album grid, album view, now playing, queue, command palette
- Waveform seek bar showing RMS, peak, and the low/mid/high mix as colour
- One accent colour, sampled from the current cover

Deliberately correct, because each is easy to get wrong:

- **Editing many tracks must not flatten them.** A field the selection
  disagrees on shows "multiple" and is not written; only fields actually typed
  into are sent. Loading twelve tracks and pressing save would otherwise stamp
  one title across all of them.
- **Rewriting a tag must not cost the track its analysis.** A tag write changes
  the file's first 64KB, which is what the content key hashes, so a plain
  rescan would decide the audio had been replaced and throw away tempo, key,
  bit depth and spectral figures. The write records the new `(mtime, size,
  content_key)` itself, so the next scan sees a file it already knows.
- **Writes go through a copy and a rename**, which is atomic within a
  directory. An interrupted write leaves the old file or the new one, never
  half of either.
- **A write is not written until it can be read back.** A tag save can report
  success and leave the previous values in the file. Every write re-reads the
  file and confirms the edit is in it, and reports a failure if it is not --
  the editor must never show a change that did not happen, and undo must never
  record a state that never existed. There is a known case where this fires:
  see Limitations.
- **Undo is the write path pointed backwards**, not a second implementation of
  it: every write records what the fields held before, and undoing replays
  those values through the same code. Only the newest operation can be undone,
  because reversing an older edit while a newer one still stands would produce
  a state the files were never in.
- **Tag conventions have to be written in the right order.** Saving the native
  chunk after ID3v2 rewrites the container's chunk list, and the ID3v2 chunk
  written moments earlier is silently gone -- no error, it simply is not there
  when the file is read back. Native first, ID3v2 second.
- **WAV and AIFF are tagged twice.** Both containers support a native text
  chunk and an embedded ID3v2 chunk, and software disagrees about which to
  read: Rekordbox and Serato prefer ID3v2, older tools read the native chunk.
  Writing one and not the other means half the software that opens the file
  sees nothing.

- **A peak-only waveform of a modern master is a rectangle.** Honestly so: the
  loudest sample in any second of a brickwalled club track is the limiter
  ceiling, every second. The seek bar draws RMS as the body, peak as an
  outline, and the low/mid/high mix as colour, on a decibel scale with a fixed
  floor. Fixed rather than per-track, so two tracks are comparable and a
  squashed master is allowed to look squashed.
- **Band energy has to be compared logarithmically.** Spectral density in music
  falls roughly 30dB from the bass to the top octave, so a linear comparison
  makes every track ever recorded read as pure bass; summing raw magnitudes
  instead makes them all read as treble, because the top band spans a hundred
  times as many FFT bins. Each band is scored against the loudest of the three
  over a fixed window.

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
crates/analysis/  loudness, waveform, tempo, key, and format verification
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

## Limitations

- **Repeated tag edits can fail on some files.** A second write to a file that
  another tool tagged in a particular way reports success without changing it.
  Rather than trust the writer, every write is verified by reading the file
  back, so this surfaces as an error and the file is left untouched -- but the
  edit does not go through. Every file in the collection this was built against
  survives repeated edit-and-undo cycles; it reproduces on a synthetic fixture,
  and four tests are marked `#[ignore]` recording it. Run them with
  `cargo test -- --ignored`.
- **Smart playlists ship as ready-made rule sets**, not a visual rule editor.
  The engine takes arbitrary rules and stores them as JSON; only the editor is
  missing.
- **Crossfade** appears in the signal path and always reads "none".
- Tempo and key detection are the design document's honest 80%.
