use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use dubplate_library::artwork::{self, ArtworkCache, ArtworkReport};
use dubplate_library::{index, query, watch, Library, LibraryWatcher, SyncReport, TrackRow};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

/// Key in the `app_state` table holding the folder the user chose.
const ROOT_KEY: &str = "library_root";

struct AppState {
    library: Mutex<Library>,
    artwork: ArtworkCache,
    watcher: Mutex<Option<LibraryWatcher>>,
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
            });

            // Resume watching the folder from the last session, so changes made
            // while the app was closed are picked up without the user asking.
            if let Ok(Some(root)) = current_root(&state) {
                if root.is_dir() {
                    start_watching(app.handle().clone(), Arc::clone(&state), root)?;
                }
            }

            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            library_status,
            open_library,
            rescan,
            list_tracks,
            search_tracks,
            refresh_artwork
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
