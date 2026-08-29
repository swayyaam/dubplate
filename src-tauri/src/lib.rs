use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use dubplate_audio::engine::{Command, Engine, PlayEvent, PlayerState, QueueItem, RepeatMode};
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, PlatformConfig};
use dubplate_library::artwork::{self, ArtworkCache, ArtworkReport};
use dubplate_library::{
    history, index, query, watch, AlbumRow, Library, LibraryWatcher, Listen, SyncReport, TrackRow,
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
/// Buckets in a waveform. Enough detail for a full-width seek bar, small enough
/// to send over IPC without thinking about it.
const WAVEFORM_BUCKETS: usize = 1000;
/// How often playback state is written back. Often enough that a crash loses
/// seconds, rarely enough that it is not writing during every frame.
const SAVE_INTERVAL: Duration = Duration::from_secs(3);
/// The housekeeping tick: drain finished listens, refresh the macOS Now Playing
/// panel, and every few ticks write playback state back.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_secs(1);

struct AppState {
    library: Mutex<Library>,
    artwork: ArtworkCache,
    watcher: Mutex<Option<LibraryWatcher>>,
    engine: Engine,
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

/// Peaks for the seek bar. One decode pass, roughly a tenth of a second.
#[tauri::command]
async fn waveform(state: State<'_, Arc<AppState>>, track_id: i64) -> Fallible<Vec<f32>> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let path: String = {
            let library = state.library.lock().map_err(to_error)?;
            library
                .connection()
                .query_row("SELECT path FROM tracks WHERE id = ?1", [track_id], |row| {
                    row.get(0)
                })
                .map_err(to_error)?
        };
        dubplate_audio::peaks::compute(std::path::Path::new(&path), WAVEFORM_BUCKETS)
            .map_err(to_error)
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
    let mut stmt = library
        .connection()
        .prepare("SELECT path FROM tracks WHERE id = ?1")
        .map_err(to_error)?;
    for id in track_ids {
        if let Ok(path) = stmt.query_row([id], |row| row.get::<_, String>(0)) {
            items.push(QueueItem {
                track_id: *id,
                path,
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
                watcher: Mutex::new(None),
                engine: Engine::spawn(),
            });

            // Resume watching the folder from the last session, so changes made
            // while the app was closed are picked up without the user asking.
            if let Ok(Some(root)) = current_root(&state) {
                if root.is_dir() {
                    start_watching(app.handle().clone(), Arc::clone(&state), root)?;
                }
            }

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
            waveform,
            get_ui_state,
            set_ui_state
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
