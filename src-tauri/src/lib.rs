use std::path::PathBuf;

use dubplate_library::ScanReport;

/// Walk a folder and return everything playable inside it.
///
/// Runs on the blocking pool: a full scan is CPU-bound across every core and
/// must never sit on an async worker, let alone the UI thread.
#[tauri::command]
async fn scan_library(path: String) -> Result<ScanReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = PathBuf::from(&path);
        if !root.is_dir() {
            return Err(format!("Not a folder: {path}"));
        }
        Ok(dubplate_library::scan_folder(&root))
    })
    .await
    .map_err(|err| format!("Scan task failed: {err}"))?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![scan_library])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
