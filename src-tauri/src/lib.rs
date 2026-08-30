use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use dubplate_audio::engine::{
    Command, Engine, OutputSettings, PlayEvent, PlayerState, QueueItem, RepeatMode,
};
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, PlatformConfig};
use dubplate_library::artwork::{self, ArtworkCache, ArtworkReport};
use dubplate_analysis::{pipeline, WaveformCache};
use dubplate_library::tags;
use dubplate_library::undo::{self, UndoStore};
use dubplate_library::{
    flow, health, history, index, playlists, query, watch, AlbumRow, CollectionHealth, FlowStep,
    Library, LibraryWatcher, Listen, PlaylistRow, SyncReport, TrackRow,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

/// Key in the `app_state` table holding the folder the user chose.
const ROOT_KEY: &str = "library_root";
/// Queue, current track, position and volume, so a relaunch resumes where the
/// last session stopped.
const PLAYBACK_KEY: &str = "playback";
/// The view the user was last on, so relaunching lands where they left off.
const VIEW_KEY: &str = "last_view";
/// Exclusive mode, rate handling and ReplayGain. Per-machine rather than
/// per-library, but the index is where this app keeps its state.
const OUTPUT_KEY: &str = "output_settings";
/// Tracks per analysis batch. Small enough that stopping is responsive, large
/// enough that the thread pool is worth building.
const ANALYSIS_BATCH: usize = 16;
/// Threads while music is playing. The audio callback matters more than
/// finishing sooner, and this leaves the machine usable besides.
const ANALYSIS_THREADS_PLAYING: usize = 2;
const ANALYSIS_THREADS_IDLE: usize = 6;
/// How often playback state is written back. Often enough that a crash loses
/// seconds, rarely enough that it is not writing during every frame.
const SAVE_INTERVAL: Duration = Duration::from_secs(3);
/// The housekeeping tick: drain finished listens, refresh the macOS Now Playing
/// panel, and every few ticks write playback state back.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_secs(1);

struct AppState {
    library: Mutex<Library>,
    artwork: ArtworkCache,
    waveforms: WaveformCache,
    /// Previous tag values, so a write can be taken back.
    undo: UndoStore,
    watcher: Mutex<Option<LibraryWatcher>>,
    engine: Engine,
    /// Guards against two analysis passes running at once.
    analysing: AtomicBool,
}

/// The slice of playback worth surviving a restart.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SavedPlayback {
    queue: Vec<i64>,
    queue_index: usize,
    position_ms: u64,
    volume: f32,
    repeat: String,
    shuffle: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncOutcome {
    sync: SyncReport,
    artwork: ArtworkReport,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryStatus {
    /// The folder from the last session, if there was one.
    root: Option<String>,
    track_count: i64,
}

type Fallible<T> = Result<T, String>;

fn to_error(err: impl std::fmt::Display) -> String {
    err.to_string()
}

/// What the UI needs on launch to decide between the empty state and the table.
#[tauri::command]
fn library_status(state: State<'_, Arc<AppState>>) -> Fallible<LibraryStatus> {
    let library = state.library.lock().map_err(to_error)?;
    let root = library.get_state(ROOT_KEY).map_err(to_error)?;
    let track_count = library
        .connection()
        .query_row("SELECT count(*) FROM tracks", [], |row| row.get(0))
        .map_err(to_error)?;
    Ok(LibraryStatus { root, track_count })
}

/// Point the library at a folder, index it, and start watching it.
#[tauri::command]
async fn open_library(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    path: String,
) -> Fallible<SyncOutcome> {
    let state = Arc::clone(&state);
    let root = PathBuf::from(&path);
    if !root.is_dir() {
        return Err(format!("Not a folder: {path}"));
    }

    let outcome = run_sync(Arc::clone(&state), root.clone()).await?;
    {
        let library = state.library.lock().map_err(to_error)?;
        library.set_state(ROOT_KEY, &path).map_err(to_error)?;
    }
    start_watching(app, state, root)?;
    Ok(outcome)
}

/// Re-index the folder already open. Cheap when nothing changed.
#[tauri::command]
async fn rescan(state: State<'_, Arc<AppState>>) -> Fallible<SyncOutcome> {
    let state = Arc::clone(&state);
    let root = current_root(&state)?.ok_or("No library folder is open")?;
    run_sync(state, root).await
}

#[tauri::command]
async fn list_tracks(state: State<'_, Arc<AppState>>) -> Fallible<Vec<TrackRow>> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let library = state.library.lock().map_err(to_error)?;
        query::list_tracks(&library).map_err(to_error)
    })
    .await
    .map_err(to_error)?
}

/// Full-text search. Runs on every keystroke, so it must stay well under a
/// frame; FTS5 over a library this size is a fraction of a millisecond.
#[tauri::command]
async fn search_tracks(
    state: State<'_, Arc<AppState>>,
    query_text: String,
    limit: usize,
) -> Fallible<Vec<TrackRow>> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let library = state.library.lock().map_err(to_error)?;
        query::search(&library, &query_text, limit).map_err(to_error)
    })
    .await
    .map_err(to_error)?
}

/// Look again at albums previously found to have no cover.
#[tauri::command]
async fn refresh_artwork(state: State<'_, Arc<AppState>>) -> Fallible<ArtworkReport> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let mut library = state.library.lock().map_err(to_error)?;
        artwork::refresh_missing(&library).map_err(to_error)?;
        artwork::build_cache(&mut library, &state.artwork).map_err(to_error)
    })
    .await
    .map_err(to_error)?
}

/// Start playing a set of tracks. Paths come from the index, not the UI, so a
/// renamed file plays from wherever the last sync found it.
#[tauri::command]
fn play_tracks(state: State<'_, Arc<AppState>>, track_ids: Vec<i64>, start: usize) -> Fallible<()> {
    let items = resolve_queue(&state, &track_ids)?;
    if items.is_empty() {
        return Err("None of those tracks are still in the library".into());
    }
    state.engine.send(Command::SetQueue {
        items,
        start: start.min(track_ids.len().saturating_sub(1)),
    });
    Ok(())
}

#[tauri::command]
fn player_state(state: State<'_, Arc<AppState>>) -> PlayerState {
    state.engine.snapshot()
}

#[tauri::command]
fn toggle_play(state: State<'_, Arc<AppState>>) {
    state.engine.send(Command::TogglePlay);
}

#[tauri::command]
fn next_track(state: State<'_, Arc<AppState>>) {
    state.engine.send(Command::Next);
}

#[tauri::command]
fn previous_track(state: State<'_, Arc<AppState>>) {
    state.engine.send(Command::Previous);
}

#[tauri::command]
fn seek(state: State<'_, Arc<AppState>>, ms: u64) {
    state.engine.send(Command::Seek { ms });
}

#[tauri::command]
fn set_volume(state: State<'_, Arc<AppState>>, volume: f32) {
    state.engine.send(Command::SetVolume(volume));
}

#[tauri::command]
fn set_repeat(state: State<'_, Arc<AppState>>, mode: String) {
    let mode = match mode.as_str() {
        "all" => RepeatMode::All,
        "one" => RepeatMode::One,
        _ => RepeatMode::Off,
    };
    state.engine.send(Command::SetRepeat(mode));
}

#[tauri::command]
fn set_shuffle(state: State<'_, Arc<AppState>>, shuffle: bool) {
    state.engine.send(Command::SetShuffle(shuffle));
}

/// Exclusive access, rate handling and ReplayGain.
///
/// Saved as well as applied: exclusive mode is a decision about a particular
/// device, not something to re-make every launch.
#[tauri::command]
fn set_output_settings(
    state: State<'_, Arc<AppState>>,
    settings: OutputSettings,
) -> Fallible<()> {
    state
        .engine
        .send(Command::SetOutputSettings(settings.clone()));
    if let (Ok(library), Ok(json)) = (state.library.lock(), serde_json::to_string(&settings)) {
        let _ = library.set_state(OUTPUT_KEY, &json);
    }
    Ok(())
}

/// Rebuild the output stream against the current default device.
///
/// Also the honest answer to "audio has got stuck".
#[tauri::command]
fn reopen_output(state: State<'_, Arc<AppState>>) {
    state.engine.send(Command::ReopenOutput);
}

#[tauri::command]
async fn list_albums(state: State<'_, Arc<AppState>>) -> Fallible<Vec<AlbumRow>> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let library = state.library.lock().map_err(to_error)?;
        query::list_albums(&library).map_err(to_error)
    })
    .await
    .map_err(to_error)?
}

#[tauri::command]
async fn album_tracks(state: State<'_, Arc<AppState>>, album_id: i64) -> Fallible<Vec<TrackRow>> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let library = state.library.lock().map_err(to_error)?;
        query::album_tracks(&library, album_id).map_err(to_error)
    })
    .await
    .map_err(to_error)?
}

/// The accent colour for a cover, so the interface can take its one colour from
/// whatever is playing.
#[tauri::command]
async fn accent_color(state: State<'_, Arc<AppState>>, hash: String) -> Fallible<Option<String>> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        Ok(dubplate_library::artwork::accent(&state.artwork, &hash))
    })
    .await
    .map_err(to_error)?
}

/// Up to three colours from a sleeve, for the now playing backdrop.
#[tauri::command]
async fn accent_palette(state: State<'_, Arc<AppState>>, hash: String) -> Fallible<Vec<String>> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        Ok(dubplate_library::artwork::palette(&state.artwork, &hash))
    })
    .await
    .map_err(to_error)?
}

/// The waveform for the seek bar, as raw bytes.
///
/// Bytes rather than JSON: five lanes of a thousand values would be a 20KB
/// number-by-number array on the wire, and the canvas wants typed arrays at the
/// other end anyway.
///
/// A miss runs the full analysis for that one track rather than a waveform-only
/// decode. It costs the same -- decoding is the expensive part and this is the
/// same single pass -- and it means playing an unanalysed track fills in its
/// loudness, tempo and key too, instead of throwing that work away.
#[tauri::command]
async fn waveform(state: State<'_, Arc<AppState>>, track_id: i64) -> Fallible<tauri::ipc::Response> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let (path, content_key): (String, String) = {
            let library = state.library.lock().map_err(to_error)?;
            library
                .connection()
                .query_row(
                    "SELECT path, content_key FROM tracks WHERE id = ?1",
                    [track_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(to_error)?
        };
        if let Some(bytes) = state.waveforms.read_bytes(&content_key) {
            return Ok(tauri::ipc::Response::new(bytes));
        }

        // Analysed outside the lock, then stored under it, for the same reason
        // the batch runner does: a decode must never hold up a query.
        let analysis = dubplate_analysis::analyse(std::path::Path::new(&path)).map_err(to_error)?;
        let bytes = analysis.waveform.to_bytes();
        let result = pipeline::AnalysedTrack {
            id: track_id,
            content_key,
            analysis: Some(analysis),
        };
        {
            let mut library = state.library.lock().map_err(to_error)?;
            pipeline::store_all(&mut library, &state.waveforms, std::slice::from_ref(&result))
                .map_err(to_error)?;
        }
        Ok(tauri::ipc::Response::new(bytes))
    })
    .await
    .map_err(to_error)?
}

/// The tag values shared by a selection, for the editor to show.
#[tauri::command]
async fn track_tags(
    state: State<'_, Arc<AppState>>,
    ids: Vec<i64>,
) -> Fallible<Vec<tags::FieldValue>> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let library = state.library.lock().map_err(to_error)?;
        tags::read_fields(&library, &ids).map_err(to_error)
    })
    .await
    .map_err(to_error)?
}

/// Write an edit to the selected files.
///
/// Blocking, because the caller has to know whether their files changed before
/// it can show them anything, and because a bulk edit that reported success
/// before finishing would be lying.
#[tauri::command]
async fn write_track_tags(
    state: State<'_, Arc<AppState>>,
    ids: Vec<i64>,
    edit: tags::TagEdit,
) -> Fallible<tags::WriteReport> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let touched_artwork = edit.artwork.is_some();
        let report = {
            let mut library = state.library.lock().map_err(to_error)?;
            let report = tags::write(&mut library, &state.undo, &ids, &edit).map_err(to_error)?;
            if touched_artwork {
                tags::invalidate_album_art(&library, &ids).map_err(to_error)?;
            }
            report
        };
        if touched_artwork {
            // Outside the write so a slow image decode does not hold the lock.
            let mut library = state.library.lock().map_err(to_error)?;
            let _ = dubplate_library::artwork::build_cache(&mut library, &state.artwork);
        }
        Ok(report)
    })
    .await
    .map_err(to_error)?
}

/// Operations that can still be taken back, newest first.
#[tauri::command]
async fn undo_history(state: State<'_, Arc<AppState>>) -> Fallible<Vec<undo::Batch>> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let library = state.library.lock().map_err(to_error)?;
        undo::history(&library).map_err(to_error)
    })
    .await
    .map_err(to_error)?
}

/// Put the most recent tag write back.
///
/// Only the most recent: reversing an older edit while a newer one still
/// stands would leave the files in a state they were never in.
#[tauri::command]
async fn undo_last(state: State<'_, Arc<AppState>>) -> Fallible<Option<tags::WriteReport>> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let mut library = state.library.lock().map_err(to_error)?;
        let Some(batch) = undo::newest(&library).map_err(to_error)? else {
            return Ok(None);
        };
        let report = tags::undo_batch(&mut library, &state.undo, batch).map_err(to_error)?;
        // A restored cover has to reach the artwork cache too, or the old one
        // stays on screen.
        let ids: Vec<i64> = report.outcomes.iter().map(|outcome| outcome.id).collect();
        let _ = tags::invalidate_album_art(&library, &ids);
        let _ = dubplate_library::artwork::build_cache(&mut library, &state.artwork);
        Ok(Some(report))
    })
    .await
    .map_err(to_error)?
}

/// What the filenames say, without writing anything.
#[tauri::command]
async fn filename_preview(
    state: State<'_, Arc<AppState>>,
    ids: Option<Vec<i64>>,
    only_missing: bool,
) -> Fallible<Vec<tags::NamePreview>> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let library = state.library.lock().map_err(to_error)?;
        tags::preview_names(&library, ids.as_deref(), only_missing).map_err(to_error)
    })
    .await
    .map_err(to_error)?
}

/// Write filename-derived tags to the given tracks.
#[tauri::command]
async fn apply_filename_tags(
    state: State<'_, Arc<AppState>>,
    ids: Vec<i64>,
    fields: tags::NameFields,
) -> Fallible<tags::WriteReport> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let mut library = state.library.lock().map_err(to_error)?;
        tags::apply_names(&mut library, &state.undo, &ids, fields).map_err(to_error)
    })
    .await
    .map_err(to_error)?
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AnalysisStatus {
    remaining: i64,
    total: i64,
    running: bool,
}

#[tauri::command]
fn analysis_status(state: State<'_, Arc<AppState>>) -> Fallible<AnalysisStatus> {
    let library = state.library.lock().map_err(to_error)?;
    let total = library
        .connection()
        .query_row("SELECT count(*) FROM tracks", [], |row| row.get(0))
        .map_err(to_error)?;
    Ok(AnalysisStatus {
        remaining: pipeline::remaining(&library).map_err(to_error)?,
        total,
        running: state.analysing.load(Ordering::Relaxed),
    })
}

/// Start the background analysis pass.
///
/// Resumable, so calling it again after a restart picks up where it stopped,
/// and throttled, so it does not cost the thing it exists to improve.
#[tauri::command]
fn start_analysis(app: AppHandle, state: State<'_, Arc<AppState>>) {
    if state.analysing.swap(true, Ordering::AcqRel) {
        return;
    }
    let state = Arc::clone(&state);
    std::thread::Builder::new()
        .name("dubplate-analysis".into())
        .spawn(move || {
            run_analysis(&app, &state);
            state.analysing.store(false, Ordering::Release);
            let _ = app.emit("analysis:done", ());
        })
        .ok();
}

fn run_analysis(app: &AppHandle, state: &Arc<AppState>) {
    loop {
        let playing = state.engine.snapshot().playing;
        let threads = if playing {
            ANALYSIS_THREADS_PLAYING
        } else {
            ANALYSIS_THREADS_IDLE
        };

        let pending = {
            let Ok(library) = state.library.lock() else {
                return;
            };
            match pipeline::take_pending(&library, ANALYSIS_BATCH) {
                Ok(pending) => pending,
                Err(_) => return,
            }
        };
        if pending.is_empty() {
            return;
        }

        // The expensive part, with no lock held: the rest of the app keeps
        // querying while this runs.
        let underruns_before = state.engine.snapshot().underruns;
        let results = pipeline::analyse_all(&pending, threads);

        let report = {
            let Ok(mut library) = state.library.lock() else {
                return;
            };
            match pipeline::store_all(&mut library, &state.waveforms, &results) {
                Ok(report) => report,
                Err(_) => return,
            }
        };
        let _ = app.emit("analysis:progress", &report);
        if report.remaining == 0 {
            return;
        }

        // Back off hard if the last batch cost the listener a dropout. The
        // design document is explicit: analysis pauses rather than glitching
        // playback.
        if state.engine.snapshot().underruns > underruns_before {
            std::thread::sleep(Duration::from_secs(5));
        } else if playing {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

#[tauri::command]
async fn list_playlists(state: State<'_, Arc<AppState>>) -> Fallible<Vec<PlaylistRow>> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let library = state.library.lock().map_err(to_error)?;
        playlists::list(&library).map_err(to_error)
    })
    .await
    .map_err(to_error)?
}

#[tauri::command]
async fn playlist_tracks(
    state: State<'_, Arc<AppState>>,
    playlist_id: i64,
) -> Fallible<Vec<TrackRow>> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let library = state.library.lock().map_err(to_error)?;
        playlists::tracks(&library, playlist_id).map_err(to_error)
    })
    .await
    .map_err(to_error)?
}

/// Create the ready-made smart playlists, skipping any that already exist.
///
/// A visual rule editor is a bigger thing; these cover the questions worth
/// asking of a collection assembled over years, and each is a real rule set
/// stored as JSON rather than a hard-coded query.
#[tauri::command]
fn add_preset_playlists(state: State<'_, Arc<AppState>>) -> Fallible<usize> {
    let library = state.library.lock().map_err(to_error)?;
    let existing: Vec<String> = playlists::list(&library)
        .map_err(to_error)?
        .into_iter()
        .map(|playlist| playlist.name)
        .collect();

    let mut created = 0;
    for (name, rules) in playlists::presets() {
        if existing.iter().any(|current| current == name) {
            continue;
        }
        playlists::create_smart(&library, name, &rules).map_err(to_error)?;
        created += 1;
    }
    Ok(created)
}

#[tauri::command]
fn delete_playlist(state: State<'_, Arc<AppState>>, playlist_id: i64) -> Fallible<()> {
    let library = state.library.lock().map_err(to_error)?;
    playlists::delete(&library, playlist_id).map_err(to_error)
}

/// Build a set that flows from one track, using the tempo, key and loudness the
/// analysis pass measured.
#[tauri::command]
async fn build_set(
    state: State<'_, Arc<AppState>>,
    track_id: i64,
    length: usize,
) -> Fallible<Vec<FlowStep>> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let library = state.library.lock().map_err(to_error)?;
        flow::build_set(&library, track_id, length.clamp(2, 100)).map_err(to_error)
    })
    .await
    .map_err(to_error)?
}

#[tauri::command]
async fn collection_health(state: State<'_, Arc<AppState>>) -> Fallible<CollectionHealth> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let library = state.library.lock().map_err(to_error)?;
        health::summary(&library).map_err(to_error)
    })
    .await
    .map_err(to_error)?
}

/// The tracks behind one number in the health view.
#[tauri::command]
async fn health_tracks(
    state: State<'_, Arc<AppState>>,
    filter: String,
    limit: usize,
) -> Fallible<Vec<TrackRow>> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let library = state.library.lock().map_err(to_error)?;
        health::tracks(&library, &filter, limit).map_err(to_error)
    })
    .await
    .map_err(to_error)?
}

/// Small key/value store for interface state, so a relaunch is not a reset.
#[tauri::command]
fn get_ui_state(state: State<'_, Arc<AppState>>, key: String) -> Fallible<Option<String>> {
    let library = state.library.lock().map_err(to_error)?;
    library.get_state(&ui_key(&key)).map_err(to_error)
}

#[tauri::command]
fn set_ui_state(state: State<'_, Arc<AppState>>, key: String, value: String) -> Fallible<()> {
    let library = state.library.lock().map_err(to_error)?;
    library.set_state(&ui_key(&key), &value).map_err(to_error)
}

/// Namespaced so interface preferences cannot collide with engine state.
fn ui_key(key: &str) -> String {
    if key == "view" {
        VIEW_KEY.to_string()
    } else {
        format!("ui.{key}")
    }
}

/// Bank what was actually listened to.
///
/// Play counts live in the database rather than in tags, so nothing dubplate
/// does writes to the user's files.
fn record_plays(state: &Arc<AppState>, events: &[PlayEvent]) {
    if events.is_empty() {
        return;
    }
    let listens: Vec<Listen> = events
        .iter()
        .map(|event| Listen {
            track_id: event.track_id,
            ms_played: event.ms_played,
            completed: event.completed,
        })
        .collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    if let Ok(mut library) = state.library.lock() {
        let _ = history::record(&mut library, &listens, now);
    }
}

fn now_playing_metadata(state: &Arc<AppState>, track_id: i64) -> Option<(String, String, String)> {
    let library = state.library.lock().ok()?;
    history::summary(&library, track_id)
}

/// Turn track ids into playable queue items, keeping the caller's order and
/// silently dropping anything the index no longer knows about.
fn resolve_queue(state: &Arc<AppState>, track_ids: &[i64]) -> Fallible<Vec<QueueItem>> {
    let library = state.library.lock().map_err(to_error)?;
    let mut items = Vec::with_capacity(track_ids.len());
    // Album and ReplayGain come along with the path: the engine needs the album
    // to decide when "follow album" may change the device rate, and the gain to
    // level the track without a second round trip.
    let mut stmt = library
        .connection()
        .prepare("SELECT path, album_id, rg_track_gain, rg_track_peak FROM tracks WHERE id = ?1")
        .map_err(to_error)?;
    for id in track_ids {
        if let Ok((path, album_id, gain, peak)) = stmt.query_row([id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<f32>>(2)?,
                row.get::<_, Option<f32>>(3)?,
            ))
        }) {
            items.push(QueueItem {
                track_id: *id,
                path,
                album_id,
                replay_gain_db: gain,
                replay_gain_peak: peak,
            });
        }
    }
    Ok(items)
}

fn current_root(state: &Arc<AppState>) -> Fallible<Option<PathBuf>> {
    let library = state.library.lock().map_err(to_error)?;
    Ok(library
        .get_state(ROOT_KEY)
        .map_err(to_error)?
        .map(PathBuf::from))
}

/// The indexing pass, off the async workers because it saturates every core.
async fn run_sync(state: Arc<AppState>, root: PathBuf) -> Fallible<SyncOutcome> {
    tauri::async_runtime::spawn_blocking(move || sync_blocking(&state, &root))
        .await
        .map_err(to_error)?
}

fn sync_blocking(state: &Arc<AppState>, root: &std::path::Path) -> Fallible<SyncOutcome> {
    let mut library = state.library.lock().map_err(to_error)?;
    let sync = index::sync(&mut library, root).map_err(to_error)?;
    // Covers are a separate pass: the tag scan reads with cover art switched
    // off, because decoding every embedded image mid-walk costs more than it
    // saves.
    let artwork = artwork::build_cache(&mut library, &state.artwork).map_err(to_error)?;
    Ok(SyncOutcome { sync, artwork })
}

/// Watch the library folder and re-index after any burst of changes settles.
fn start_watching(app: AppHandle, state: Arc<AppState>, root: PathBuf) -> Fallible<()> {
    let watched_root = root.clone();
    // Weak, not Arc: the state owns the watcher, which owns this closure. An
    // owning handle back to the state would be a cycle that never drops.
    let weak: Weak<AppState> = Arc::downgrade(&state);
    let watcher = watch::watch(&root, watch::DEFAULT_DEBOUNCE, move || {
        let Some(state) = weak.upgrade() else {
            return;
        };
        match sync_blocking(&state, &watched_root) {
            Ok(outcome) => {
                // Nothing actually changed on disk, so do not make the UI redraw.
                let s = &outcome.sync;
                if s.added + s.updated + s.moved + s.removed > 0 {
                    let _ = app.emit("library:synced", &outcome);
                }
            }
            Err(err) => {
                let _ = app.emit("library:error", err);
            }
        }
    })
    .map_err(to_error)?;

    *state.watcher.lock().map_err(to_error)? = Some(watcher);
    Ok(())
}

/// Resolve `art://localhost/<hash>/<width>` to a file in the artwork cache.
///
/// The hash and width are validated rather than pasted into a path: this handler
/// reads whatever it is asked for, so it must only ever be asked for cache
/// entries.
fn serve_artwork<R: tauri::Runtime>(app: &AppHandle<R>, path: &str) -> Option<Vec<u8>> {
    let mut parts = path.trim_start_matches('/').split('/');
    let hash = parts.next()?;
    let width: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    if hash.is_empty()
        || hash.len() > 64
        || !hash.chars().all(|c| c.is_ascii_hexdigit())
        || !dubplate_library::artwork::VARIANTS.contains(&width)
    {
        return None;
    }

    let cache = dubplate_library::ArtworkCache::new(app.path().app_data_dir().ok()?.join("artwork"));
    std::fs::read(cache.variant_path(hash, width)).ok()
}

fn empty_response() -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(404)
        .body(Vec::new())
        .expect("a 404 with an empty body is always constructible")
}

fn save_playback(state: &Arc<AppState>) {
    let snapshot = state.engine.snapshot();
    // Nothing worth restoring, and writing an empty queue would throw away what
    // the last session left.
    if snapshot.queue.is_empty() {
        return;
    }
    let saved = SavedPlayback {
        queue: snapshot.queue,
        queue_index: snapshot.queue_index,
        position_ms: snapshot.position_ms,
        volume: snapshot.volume,
        repeat: match snapshot.repeat {
            RepeatMode::All => "all".into(),
            RepeatMode::One => "one".into(),
            RepeatMode::Off => "off".into(),
        },
        shuffle: snapshot.shuffle,
    };
    if let (Ok(library), Ok(json)) = (state.library.lock(), serde_json::to_string(&saved)) {
        let _ = library.set_state(PLAYBACK_KEY, &json);
    }
}

/// Put the queue back, at the track and position the last session stopped on.
/// Deliberately does not start playing: launching an app should be quiet.
fn restore_playback(state: &Arc<AppState>) {
    let json = {
        let Ok(library) = state.library.lock() else {
            return;
        };
        match library.get_state(PLAYBACK_KEY) {
            Ok(Some(json)) => json,
            _ => return,
        }
    };
    let Ok(saved) = serde_json::from_str::<SavedPlayback>(&json) else {
        return;
    };
    let Ok(items) = resolve_queue(state, &saved.queue) else {
        return;
    };
    if items.is_empty() {
        return;
    }

    state.engine.send(Command::SetVolume(saved.volume));
    state.engine.send(Command::SetShuffle(saved.shuffle));
    state.engine.send(Command::SetRepeat(match saved.repeat.as_str() {
        "all" => RepeatMode::All,
        "one" => RepeatMode::One,
        _ => RepeatMode::Off,
    }));
    state.engine.send(Command::SetQueue {
        items,
        start: saved.queue_index,
    });
    state.engine.send(Command::Seek {
        ms: saved.position_ms,
    });
    state.engine.send(Command::Pause);
}

fn restore_output_settings(state: &Arc<AppState>) {
    let json = {
        let Ok(library) = state.library.lock() else {
            return;
        };
        match library.get_state(OUTPUT_KEY) {
            Ok(Some(json)) => json,
            _ => return,
        }
    };
    if let Ok(settings) = serde_json::from_str::<OutputSettings>(&json) {
        state.engine.send(Command::SetOutputSettings(settings));
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Covers are served straight off disk rather than base64'd through IPC:
        // an art grid asks for hundreds at once, and the cache already holds
        // them at exactly the size being drawn.
        .register_uri_scheme_protocol("art", |ctx, request| {
            match serve_artwork(ctx.app_handle(), request.uri().path()) {
                Some(bytes) => tauri::http::Response::builder()
                    .header("Content-Type", "image/webp")
                    // Content-addressed, so it can never go stale.
                    .header("Cache-Control", "public, max-age=31536000, immutable")
                    .body(bytes)
                    .unwrap_or_else(|_| empty_response()),
                None => empty_response(),
            }
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let library = Library::open(data_dir.join("library.sqlite"))?;
            let artwork = ArtworkCache::new(data_dir.join("artwork"));

            let state = Arc::new(AppState {
                library: Mutex::new(library),
                artwork,
                waveforms: WaveformCache::new(data_dir.join("waveforms")),
                undo: UndoStore::new(data_dir.join("undo")),
                watcher: Mutex::new(None),
                engine: Engine::spawn(),
                analysing: AtomicBool::new(false),
            });

            // Resume watching the folder from the last session, so changes made
            // while the app was closed are picked up without the user asking.
            if let Ok(Some(root)) = current_root(&state) {
                if root.is_dir() {
                    start_watching(app.handle().clone(), Arc::clone(&state), root)?;
                }
            }

            // Settings before the queue: exclusive access and the device rate
            // are decided when a stream opens, so they have to be known first.
            restore_output_settings(&state);
            restore_playback(&state);

            // Media keys and the macOS Now Playing panel. souvlaki dispatches to
            // the main queue itself, so this needs no window handle and no
            // special thread.
            let mut media = MediaControls::new(PlatformConfig {
                display_name: "dubplate",
                dbus_name: "dubplate",
                hwnd: None,
            })
            .ok();
            if let Some(controls) = media.as_mut() {
                let engine_state = Arc::clone(&state);
                let _ = controls.attach(move |event| {
                    let engine = &engine_state.engine;
                    match event {
                        MediaControlEvent::Play => engine.send(Command::Play),
                        MediaControlEvent::Pause => engine.send(Command::Pause),
                        MediaControlEvent::Toggle => engine.send(Command::TogglePlay),
                        MediaControlEvent::Next => engine.send(Command::Next),
                        MediaControlEvent::Previous => engine.send(Command::Previous),
                        MediaControlEvent::Stop => engine.send(Command::Stop),
                        MediaControlEvent::SetPosition(position) => engine.send(Command::Seek {
                            ms: position.0.as_millis() as u64,
                        }),
                        MediaControlEvent::SetVolume(volume) => {
                            engine.send(Command::SetVolume(volume as f32))
                        }
                        // Seek by relative amounts and the rest are not wired up
                        // yet; ignoring them is better than guessing.
                        _ => {}
                    }
                });
            }

            // One housekeeping thread: bank finished listens, keep Now Playing
            // in step, and write playback state back every few ticks. Position
            // moves thirty times a second and the queue almost never does, so
            // none of this belongs on the hot path.
            let keeper = Arc::clone(&state);
            std::thread::spawn(move || {
                let mut controls = media;
                let mut last_track: Option<i64> = None;
                let mut last_playing = false;
                let mut ticks = 0u32;

                loop {
                    std::thread::sleep(HOUSEKEEPING_INTERVAL);
                    ticks += 1;

                    record_plays(&keeper, &keeper.engine.take_play_events());

                    let snapshot = keeper.engine.snapshot();
                    if let Some(controls) = controls.as_mut() {
                        if snapshot.track_id != last_track {
                            last_track = snapshot.track_id;
                            if let Some(id) = snapshot.track_id {
                                if let Some((title, artist, album)) =
                                    now_playing_metadata(&keeper, id)
                                {
                                    let _ = controls.set_metadata(MediaMetadata {
                                        title: Some(&title),
                                        artist: Some(&artist),
                                        album: Some(&album),
                                        duration: Some(Duration::from_millis(
                                            snapshot.duration_ms,
                                        )),
                                        ..Default::default()
                                    });
                                }
                            }
                        }
                        if snapshot.playing != last_playing || snapshot.track_id != last_track {
                            last_playing = snapshot.playing;
                            let _ = controls.set_playback(if snapshot.playing {
                                MediaPlayback::Playing { progress: None }
                            } else {
                                MediaPlayback::Paused { progress: None }
                            });
                        }
                    }

                    if ticks % (SAVE_INTERVAL.as_secs().max(1) as u32) == 0 {
                        save_playback(&keeper);
                    }
                }
            });

            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            library_status,
            open_library,
            rescan,
            list_tracks,
            search_tracks,
            refresh_artwork,
            play_tracks,
            player_state,
            toggle_play,
            next_track,
            previous_track,
            seek,
            set_volume,
            set_repeat,
            set_shuffle,
            list_albums,
            album_tracks,
            accent_color,
            accent_palette,
            waveform,
            get_ui_state,
            set_ui_state,
            set_output_settings,
            reopen_output,
            analysis_status,
            start_analysis,
            collection_health,
            health_tracks,
            build_set,
            list_playlists,
            playlist_tracks,
            add_preset_playlists,
            delete_playlist,
            track_tags,
            write_track_tags,
            filename_preview,
            apply_filename_tags,
            undo_history,
            undo_last
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
