mod common;

use std::fs;

use common::write_wav;
use dubplate_library::{scan_folder, Lossiness};

#[test]
fn reads_stream_properties_from_a_wav() {
    let dir = tempfile::tempdir().unwrap();
    // 2 seconds of 24-bit / 96 kHz stereo.
    write_wav(&dir.path().join("hi-res.wav"), 96_000, 24, 2, 192_000);

    let report = scan_folder(dir.path());

    assert_eq!(report.files_seen, 1);
    assert!(report.errors.is_empty(), "unexpected errors: {:?}", report.errors);
    assert_eq!(report.tracks.len(), 1);

    let track = &report.tracks[0];
    assert_eq!(track.codec, "wav");
    assert_eq!(track.lossiness, Lossiness::Lossless);
    assert_eq!(track.sample_rate, Some(96_000));
    assert_eq!(track.bit_depth, Some(24));
    assert_eq!(track.channels, Some(2));
    assert_eq!(track.duration_ms, 2_000);
    // Untagged file, so the title falls back to the filename without extension.
    assert_eq!(track.display_title(), "hi-res");
}

#[test]
fn skips_non_audio_and_hidden_files() {
    let dir = tempfile::tempdir().unwrap();
    write_wav(&dir.path().join("real.wav"), 44_100, 16, 2, 44_100);
    fs::write(dir.path().join("cover.jpg"), b"not audio").unwrap();
    fs::write(dir.path().join("notes.txt"), b"not audio").unwrap();
    fs::write(dir.path().join(".hidden.wav"), b"not audio").unwrap();

    let report = scan_folder(dir.path());

    assert_eq!(report.files_seen, 1);
    assert_eq!(report.tracks.len(), 1);
    assert_eq!(report.tracks[0].file_name, "real.wav");
}

#[test]
fn a_broken_file_is_reported_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    write_wav(&dir.path().join("good.wav"), 44_100, 16, 2, 44_100);
    // Right extension, garbage inside.
    fs::write(dir.path().join("broken.wav"), b"RIFFnope").unwrap();

    let report = scan_folder(dir.path());

    assert_eq!(report.files_seen, 2);
    assert_eq!(report.tracks.len(), 1, "the good file must still come through");
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].path.ends_with("broken.wav"));
}

#[test]
fn recurses_into_subdirectories() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("Artist").join("Album");
    fs::create_dir_all(&nested).unwrap();
    write_wav(&nested.join("01.wav"), 44_100, 16, 2, 44_100);
    write_wav(&nested.join("02.wav"), 44_100, 16, 2, 44_100);

    let report = scan_folder(dir.path());

    assert_eq!(report.tracks.len(), 2);
}

#[test]
fn does_not_descend_into_macos_packages() {
    let dir = tempfile::tempdir().unwrap();
    write_wav(&dir.path().join("keeper.wav"), 44_100, 16, 2, 44_100);

    // A DAW sample library is a package full of one-note WAVs. None of it is a
    // track, and walking in is how half a library turns into orchestral stabs.
    let samples = dir.path().join("Logic Pro Library.bundle").join("Samples");
    fs::create_dir_all(&samples).unwrap();
    write_wav(&samples.join("FL1_stac_C4.wav"), 44_100, 16, 2, 4_410);
    write_wav(&samples.join("FL1_stac_C5.wav"), 44_100, 16, 2, 4_410);

    let report = scan_folder(dir.path());

    assert_eq!(report.files_seen, 1, "package contents must not be stat'd");
    assert_eq!(report.tracks.len(), 1);
    assert_eq!(report.tracks[0].file_name, "keeper.wav");
}
