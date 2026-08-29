use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use anyhow::Result;
use notify::event::{CreateKind, EventKind, ModifyKind, RemoveKind};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::scan::AUDIO_EXTENSIONS;

/// How long the library must be quiet before a change is acted on. Editors,
/// sync clients and tag editors all write in bursts; syncing per event would
/// mean a dozen scans for one album.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(500);

/// A live watch on the library folder. Dropping it stops the watch.
pub struct LibraryWatcher {
    _watcher: RecommendedWatcher,
}

/// Call `on_change` once the library folder has been quiet for `debounce`.
///
/// The callback says only that something changed, not what: the incremental
/// sync is cheap enough that working out which file moved is not worth doing
/// twice. It runs on a background thread, so it must not block for long.
pub fn watch<F>(root: &Path, debounce: Duration, on_change: F) -> Result<LibraryWatcher>
where
    F: Fn() + Send + 'static,
{
    let (tx, rx) = mpsc::channel();

    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            if is_interesting(&event) {
                // A closed receiver just means the watcher is going away.
                let _ = tx.send(());
            }
        }
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;

    std::thread::spawn(move || loop {
        // Wait for the first event, then swallow the rest of the burst.
        if rx.recv().is_err() {
            return;
        }
        loop {
            match rx.recv_timeout(debounce) {
                Ok(()) => continue,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        on_change();
    });

    Ok(LibraryWatcher { _watcher: watcher })
}

/// Ignore the noise: access times, metadata touches, and the pile of dotfiles
/// macOS scatters through a music folder. A false positive only costs one cheap
/// walk, but `.DS_Store` is rewritten every time a Finder window opens, and
/// resyncing on that is how a watcher becomes a background CPU hog.
fn is_interesting(event: &notify::Event) -> bool {
    // A folder appearing or vanishing can bring a whole album with it, and
    // notify reports the distinction rather than leaving us to guess.
    if matches!(
        event.kind,
        EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder)
    ) {
        return true;
    }

    let relevant_kind = matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Data(_))
            | EventKind::Modify(ModifyKind::Name(_))
            | EventKind::Modify(ModifyKind::Any)
    );
    relevant_kind && event.paths.iter().any(|path| path_matters(path))
}

fn path_matters(path: &Path) -> bool {
    if let Some(extension) = path.extension().and_then(|ext| ext.to_str()) {
        return AUDIO_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str());
    }

    // No extension means either a directory or a dotfile -- Rust reports no
    // extension for ".DS_Store". Only directories are worth waking for, and a
    // renamed one can still be stat'd at its new path.
    let hidden = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'));
    !hidden && path.is_dir()
}
