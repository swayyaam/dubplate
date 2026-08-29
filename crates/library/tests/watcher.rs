mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use common::write_wav_seeded;

#[test]
fn a_burst_of_changes_fires_one_callback() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::channel();

    let counter = Arc::clone(&calls);
    let _watcher = dubplate_library::watch::watch(
        dir.path(),
        Duration::from_millis(250),
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = tx.send(());
        },
    )
    .unwrap();

    // Ten files written back to back, the way a copy or a sync client would.
    for n in 0..10 {
        write_wav_seeded(
            &dir.path().join(format!("{n}.wav")),
            44_100,
            16,
            2,
            4_410,
            n as u8 + 1,
        );
    }

    rx.recv_timeout(Duration::from_secs(10))
        .expect("the watcher should report the burst");
    // Give any stray extra callbacks a chance to land before counting.
    std::thread::sleep(Duration::from_millis(600));

    let fired = calls.load(Ordering::SeqCst);
    assert!(
        (1..=2).contains(&fired),
        "a burst should debounce to about one callback, got {fired}"
    );
}

#[test]
fn irrelevant_files_do_not_wake_the_watcher() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::channel();

    let _watcher = dubplate_library::watch::watch(
        dir.path(),
        Duration::from_millis(200),
        move || {
            let _ = tx.send(());
        },
    )
    .unwrap();

    // The kind of debris macOS leaves in a music folder.
    std::fs::write(dir.path().join(".DS_Store"), b"junk").unwrap();
    std::fs::write(dir.path().join("notes.txt"), b"junk").unwrap();
    std::fs::write(dir.path().join("cover.jpg"), b"junk").unwrap();

    assert!(
        rx.recv_timeout(Duration::from_millis(1200)).is_err(),
        "non-audio writes must not trigger a resync"
    );
}
