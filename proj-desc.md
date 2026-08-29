# dubplate

A minimal, art-forward desktop music player for a personal lossless collection. macOS first. Rust core, Tauri shell. Built for people who own their music rather than rent it.

Named after the one-off acetate cut for a soundsystem: the exclusive original, before any copy degrades it. Which is the whole point of the player.

## The premise

Streaming apps feel good and sound mediocre. Local players sound good and feel like 2009. This is the third option: playback that is technically correct (bit-perfect, gapless, device-aware) inside an interface that is actually designed.

Target library: 1,000 to 10,000 tracks, mixed FLAC / WAV / MP3, inconsistent tags, listened to through whatever output happens to be plugged in.

## Non-goals

Being explicit about this because solo projects die from scope.

- No streaming service integration, no online library
- No mobile app in v1 (the SQLite index makes it possible later, don't design for it now)
- No tag editing in v1 (read-only library, fix tags in MusicBrainz Picard)
- No plugin system, no themes, no visualizers
- No social features, no scrobbling in v1 (Last.fm is a small add later)
- Not a DJ tool. Analysis features exist to make the library smarter, not to perform with.

## Principles

1. **The audio callback is sacred.** No allocation, no locks, no I/O. Every player that clicks and pops broke this rule.
2. **Never show a spinner during playback.** Press play, sound in under 100ms, always.
3. **Filesystem is the source of truth, database is a cache.** Delete the DB, rescan, lose nothing except play counts.
4. **Keyboard first.** Everything reachable without the mouse, command palette for the rest.
5. **Show art, hide chrome.** The album cover is the interface.

## Architecture

Three long-lived threads plus the UI process.

```
Tauri / UI (webview)
        |  commands + events over IPC
        v
Control thread  ------ owns queue, playback state, DB handle
        |  bounded channel (commands)
        v
Decode thread   ------ owns Symphonia decoders (current + next)
        |  SPSC ring buffer of interleaved f32
        v
CoreAudio callback -- reads ring, applies gain, writes to device
```

- **Control thread** handles Tauri commands, mutates queue state, talks to SQLite, emits events to the UI.
- **Decode thread** holds two decoders: the playing track and a pre-rolled next track. Writes samples into the ring buffer, never blocks on anything but the buffer being full.
- **Output callback** drains the ring, applies a smoothed gain ramp (volume x ReplayGain), writes to the device. Updates an atomic sample counter and nothing else.

**Seeking** works with a generation counter. Control thread bumps the generation and signals the decoder. Decoder seeks, clears the ring, writes samples tagged with the new generation. The callback drops anything stale. This avoids a lock on the buffer.

**Position reporting** comes from the atomic sample counter, polled by the UI at 30fps. Never from the decoder, which runs ahead by whatever the buffer holds.

## Audio engine

### Output

- CoreAudio via `cpal` for the normal path, direct `coreaudio-rs` where cpal falls short (hog mode, nominal sample rate changes)
- Buffer target: 100 to 200ms of ring capacity, device buffer as small as it will reliably take
- Gain applied in f32 before the device, ramped over ~10ms on change to avoid zipper noise

### Device switching

macOS changes the default output device out from under you constantly. Headphones out, interface in, Bluetooth connects.

- Register a listener on `kAudioHardwarePropertyDefaultOutputDevice`
- On change: tear down the output stream, rebuild against the new device, resume from the atomic sample position
- Ring buffer and decoder state survive the swap, so the gap is a few tens of milliseconds
- Also handle device disappearing entirely (unplugged interface): pause, surface it in the UI, resume on reconnect

Build this in phase 3, not later. Retrofitting it into a player that assumes one fixed device is a rewrite.

### Exclusive mode (hog mode)

- Per-device opt-in, stored in settings. Interface gets exclusive, laptop speakers never do.
- On engage: take hog mode, set the device's nominal sample rate to the track's rate, disable software volume
- UI must make the state obvious, because system sounds and other apps go silent
- Auto-release hog mode on pause after N seconds, and on app background, so you don't wonder why YouTube has no audio

### Sample rate handling

Three modes in settings:

1. **Follow file** (default with exclusive): switch device rate per track. Best fidelity, small gap on rate change.
2. **Fixed rate**: pick one rate, resample everything with a high-quality resampler (`rubato`). Gapless always works.
3. **Follow album**: switch on album boundaries only. Practical compromise, since tracks in an album share a rate.

Be honest in the UI: gapless and per-track rate switching are mutually exclusive across a rate boundary.

### Gapless

- Decoder pre-rolls the next track when the current one has under ~5 seconds left
- Both tracks feed the same ring buffer, with a marker at the boundary sample index so the UI flips "now playing" at the right moment, not early
- MP3 needs LAME/Xing header parsing to trim encoder delay and padding. Verify Symphonia's handling here and trim manually if it does not.
- FLAC and WAV need no trimming

### Crossfade

Optional, off by default. Requires two decoders active simultaneously with independent gain envelopes. Do it after gapless works, they share most of the machinery.

## Library and index

### Scanning

- Parallel directory walk with `jwalk`
- Tags via `lofty`
- Incremental: skip files whose `(mtime, size)` matches the DB
- Move detection: cheap content key from `(size, first 64KB hash)` so a moved or renamed file keeps its play count
- Live updates via `notify`, debounced 500ms, since editors and sync clients fire bursts
- Full scan of 10k files should land in single-digit seconds

### Schema

```sql
CREATE TABLE artists (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL,
  sort_name   TEXT,
  UNIQUE(name)
);

CREATE TABLE albums (
  id              INTEGER PRIMARY KEY,
  title           TEXT NOT NULL,
  album_artist_id INTEGER REFERENCES artists(id),
  year            INTEGER,
  art_hash        TEXT,          -- key into the artwork cache
  disc_count      INTEGER,
  UNIQUE(title, album_artist_id)
);

CREATE TABLE tracks (
  id            INTEGER PRIMARY KEY,
  path          TEXT NOT NULL UNIQUE,
  content_key   TEXT NOT NULL,   -- size + partial hash, for move detection
  mtime         INTEGER NOT NULL,
  size          INTEGER NOT NULL,

  title         TEXT,
  artist_id     INTEGER REFERENCES artists(id),
  album_id      INTEGER REFERENCES albums(id),
  track_no      INTEGER,
  disc_no       INTEGER,
  year          INTEGER,
  genre         TEXT,

  duration_ms   INTEGER,
  codec         TEXT,            -- flac, wav, mp3, alac, aac
  is_lossy      INTEGER NOT NULL DEFAULT 0,
  sample_rate   INTEGER,
  bit_depth     INTEGER,         -- null for lossy codecs, they have none
  sample_format TEXT,            -- s16, s24, s32, f32
  channels      INTEGER,
  bitrate       INTEGER,
  bitrate_mode  TEXT,            -- cbr, vbr, abr, lossless

  rg_track_gain REAL,            -- ReplayGain 2.0 / EBU R128
  rg_track_peak REAL,
  bpm           REAL,
  music_key     TEXT,            -- Camelot notation

  effective_bits   INTEGER,      -- real depth after padding check
  spectral_cutoff  INTEGER,      -- Hz, highest band with real energy
  transcode_score  REAL,         -- 0..1 confidence this came from lossy
  analyzed_at      INTEGER,

  added_at      INTEGER NOT NULL,
  last_played   INTEGER,
  play_count    INTEGER NOT NULL DEFAULT 0,
  skip_count    INTEGER NOT NULL DEFAULT 0,
  loved         INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_tracks_album  ON tracks(album_id, disc_no, track_no);
CREATE INDEX idx_tracks_artist ON tracks(artist_id);
CREATE INDEX idx_tracks_added  ON tracks(added_at DESC);

CREATE VIRTUAL TABLE tracks_fts USING fts5(
  title, artist, album, album_artist,
  content='',
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TABLE playlists (
  id         INTEGER PRIMARY KEY,
  name       TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  is_smart   INTEGER NOT NULL DEFAULT 0,
  rules_json TEXT              -- null unless smart
);

CREATE TABLE playlist_tracks (
  playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
  track_id    INTEGER NOT NULL REFERENCES tracks(id)    ON DELETE CASCADE,
  position    REAL NOT NULL,   -- fractional, so reordering is one UPDATE
  PRIMARY KEY (playlist_id, track_id)
);

CREATE TABLE play_history (
  id         INTEGER PRIMARY KEY,
  track_id   INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  played_at  INTEGER NOT NULL,
  ms_played  INTEGER NOT NULL,
  completed  INTEGER NOT NULL   -- crossed the 50% mark
);

CREATE TABLE app_state (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL          -- queue, position, volume, last view
);
```

Notes:

- `position REAL` in playlists means dragging a track between two others is a single write, no renumbering
- FTS is `content=''` (contentless) and rebuilt on scan, keeping it small and simple
- Play counts live in the DB, not in tags, so nothing writes to your files

### Artwork cache

Never decode embedded art during scroll. On scan, extract embedded art or find a `cover.*` sibling file, hash it, and write pre-resized WebP variants (64px, 300px, 1000px) into an on-disk cache keyed by hash. Duplicate covers across an album collapse to one file.

### Search

FTS5 prefix queries with diacritic folding, run on every keystroke against a 10k-row index, which is well under a millisecond. Rank by a blend of FTS score, play count, and recency. Search should feel like it has no latency because it genuinely doesn't.

## Analysis pipeline

Background, resumable, low priority. Runs over tracks where `analyzed_at IS NULL`.

| Output | Library | Cost |
|---|---|---|
| EBU R128 loudness + true peak | `ebur128` | full decode |
| Waveform peaks (1000 buckets) | own code | same pass |
| BPM | onset detection + tempo estimation | same pass |
| Musical key | chroma / Krumhansl profiles | same pass |
| Effective bit depth | low-bit check | same pass |
| Spectral cutoff + transcode score | `rustfft` | same pass |

One decode pass per track feeds all six. Budget 30 to 60 minutes for 10k tracks on a laptop. Throttle to a couple of threads so the machine stays usable, and pause automatically while music is playing if it causes any dropout.

ReplayGain becomes real volume levelling across the whole library, which is the single biggest "sounds professional" win. Waveform peaks go in the seek bar.

## Signal path and format verification

The centrepiece nerd feature. A panel that shows exactly what the file is, everything that touched the audio on the way out, and what the device is actually running at. Roon and Audirvana have versions of this. Most free players do not, and the ones that do usually get the details wrong.

### The panel

Four blocks, always in the same order.

**Source.** Codec, sample rate, bit depth, channel count, bitrate, file size. This is what is on disk.

**Decoder.** What came out of Symphonia. Rate and sample format.

**Processing.** Every stage that could alter samples, each with its own line: resampling, volume, ReplayGain, crossfade. Each is either inactive (grey) or shows what it did. Three lines saying "none" is the point of the feature.

**Output.** Device name, the rate and depth the device reports, and whether the stream is exclusive or shared.

Then one verdict badge. Green `bit-perfect` when nothing in the processing block fired and the device rate matches the file rate. Amber `altered, N stages` otherwise, where N is clickable and expands into what changed.

### Rules for getting it right

**Read the stream, never the tags.** `CodecParameters` from Symphonia gives `sample_rate`, `bits_per_sample`, `channels` and `sample_format` from the container and frame headers. Tags in a mixed-source library are wrong often enough to be useless for this.

**Lossy codecs have no bit depth.** MP3, AAC and Vorbis store frequency coefficients, not samples. They decode straight to float. Displaying "16 bit" for an MP3 is meaningless, and plenty of players do it anyway. Show codec, rate, bitrate and bitrate mode, then note that the decoder emits 32-bit float. `bit_depth` stays null for these.

**WAV needs two labels.** 32-bit integer and 32-bit float are both common and are different things. `sample_format` distinguishes them.

**Verify the device, don't report your own request.** After setting the rate, read `kAudioStreamPropertyPhysicalFormat` back and display what the device says. Claiming bit-perfect because you asked politely is how players lie without intending to.

### Verification checks

Both run once per track during the analysis pass and are stored, not recomputed.

**Padded bit depth.** A lot of "hi-res" downloads are 16-bit content sitting inside a 24-bit container. Check whether the low 8 bits are zero across every sample. If they are, store `effective_bits = 16` and display `24 bit (16 bit content)`.

**Transcode detection.** Files sourced from mixed places are often MP3 re-encoded to FLAC, which is lossless packaging of already-destroyed audio. It shows as a brick wall in the spectrum: a hard cutoff near 20 kHz for 320 kbps, around 19 kHz for 192, near 16 kHz for 128. Average the FFT magnitude over the whole track, find the highest bin holding meaningful energy, and score how abrupt the rolloff is. Store `spectral_cutoff` and `transcode_score`.

Be careful with false positives here. Quiet or sparse recordings, older masters, and some genuinely dark mixes have naturally low high-frequency content. Treat the score as a suspicion, show the number that produced it, and never delete or hide anything automatically.

### Collection health

Once every track carries this, a library-wide view falls out for free: how much of the collection is truly lossless, what is padded, what is probably a transcode, the distribution of sample rates. Filterable and sortable, so "show me every fake FLAC" is one click. For a collection assembled from many sources over years, this is a more interesting first screen than a genre list.

## Interface

Dark by default, near-black rather than pure black. One accent colour, sampled from the current album art. Type-driven layout, generous whitespace, no borders where spacing will do.

**Views**

- **Now playing**: large art, track and artist, waveform seek bar, transport, output device indicator
- **Library**: albums as an art grid, artists and tracks as virtualized lists
- **Album**: art, tracklist, total time, format badge (FLAC 24/96)
- **Queue**: current, up next, drag to reorder
- **Signal path**: the format and processing readout, reachable from the badge in Now playing
- **Collection health**: format breakdown, padded and suspected-transcode filters
- **Command palette** (`Cmd+K`): search everything, run any action

**Details that matter**

- Virtualized lists everywhere, so 10k rows scroll at 60fps
- Format and rate badge visible while playing, so bit-perfect is verifiable at a glance
- Media keys and the macOS Now Playing panel via `souvlaki`
- Restores exact state on launch: queue, track, playback position, volume, last view
- Every list has a keyboard path, arrows plus enter, no mouse required

## Roadmap

**Phase 0. Skeleton**
Tauri app, pick a folder, walk it, list tracks in a table. Proves the toolchain.

**Phase 1. Index**
SQLite schema, incremental scan, move detection, FTS search, filesystem watcher, artwork cache. No audio yet.

**Phase 2. Playback**
Symphonia decode, ring buffer, CoreAudio output, transport controls, queue, shuffle and repeat, persisted state. This is the hard one.

**Phase 3. Feels real**
Gapless, device switching, media keys, resume on launch, play counts and history.

**Phase 4. Looks right**
The actual UI pass. Art grid, now playing, command palette, waveform seek bar, keyboard nav.

**Phase 5. Correctness**
Exclusive / hog mode, sample rate switching, ReplayGain applied, and the signal path panel wired to real device readback.

**Phase 6. Smart**
Analysis pipeline, padded-depth and transcode detection, collection health view, smart playlists, similarity search over audio features, "build a set that flows from this track".

Phases 0 to 3 are a usable player. Everything after is the reason to keep going.

## Stack

| Concern | Choice |
|---|---|
| Core | Rust |
| Shell | Tauri v2 |
| UI | TypeScript, choose one of React / Svelte / Solid |
| Decode | `symphonia` |
| Output | `cpal`, plus `coreaudio-rs` for hog mode |
| Resampling | `rubato` |
| Tags | `lofty` |
| Scan | `jwalk`, `notify` |
| DB | `rusqlite` with FTS5 |
| Loudness | `ebur128` |
| Spectral analysis | `rustfft` |
| Ring buffer | `rtrb` |
| Media keys | `souvlaki` |

## Open questions

- **Name.** Settled: `dubplate`. Check crates.io, the App Store and the domain before the first public commit.
- **UI framework.** Svelte is the lightest fit for this and pairs well with Tauri, but pick what you'll actually ship in.
- **Cross-platform.** Design the audio layer behind a trait now (`OutputDevice`, `enumerate`, `open`, `set_rate`, `set_exclusive`) so WASAPI and ALOSA backends drop in later without touching the decoder.
- **Tag writing.** Read-only in v1, but a "fix this album" flow using MusicBrainz lookup is the obvious v2.
- **Library sync.** If the phone version ever happens, the index is already portable. Don't build for it, just don't preclude it.

## Risks

- **Rust plus real-time audio plus DSP is a lot at once.** Phase 2 is where the project either works or stalls. Build the ring buffer and callback in isolation first, with a sine generator, before wiring a decoder to it.
- **Hog mode has sharp edges.** Test unplugging the interface mid-track, sleeping the laptop, and switching devices while hogging. All three break naive implementations.
- **Analysis quality.** BPM and key detection are easy to get 80% right and hard to get 95% right. Ship the 80%, show confidence, let users override.
- **Transcode false positives.** Spectral cutoff is suggestive, not proof. Dark masters and sparse recordings will trip it. Present it as a suspicion with the underlying number visible, never as a verdict, and never act on it automatically.
- **Scope.** Everything in phase 6 is optional. Phase 0 to 3 is the actual product.