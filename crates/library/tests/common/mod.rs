#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::Path;

/// Canonical 44-byte PCM WAV header plus audio. `seed` varies the sample data so
/// two files can be byte-different, which move detection needs to distinguish.
pub fn write_wav_seeded(
    path: &Path,
    sample_rate: u32,
    bits: u16,
    channels: u16,
    frames: u32,
    seed: u8,
) {
    let block_align = channels * bits / 8;
    let byte_rate = sample_rate * u32::from(block_align);
    let data_len = frames * u32::from(block_align);

    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());

    let start = out.len();
    out.resize(start + data_len as usize, 0);
    if seed != 0 {
        // Vary the first bytes of audio, which is what the content key hashes.
        for (offset, byte) in out[start..].iter_mut().take(4096).enumerate() {
            *byte = seed.wrapping_add(offset as u8);
        }
    }

    let mut file = fs::File::create(path).unwrap();
    file.write_all(&out).unwrap();
}

pub fn write_wav(path: &Path, sample_rate: u32, bits: u16, channels: u16, frames: u32) {
    write_wav_seeded(path, sample_rate, bits, channels, frames, 0);
}

/// Attach ID3v2 tags so artist and album grouping can be exercised.
pub fn tag(path: &Path, title: &str, artist: &str, album: &str) {
    use lofty::config::WriteOptions;
    use lofty::prelude::{Accessor, TagExt};
    use lofty::tag::{Tag, TagType};

    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title(title.to_string());
    tag.set_artist(artist.to_string());
    tag.set_album(album.to_string());
    tag.save_to_path(path, WriteOptions::default()).unwrap();
}

/// Push a file's mtime forward so an incremental sync notices it.
pub fn touch_forward(path: &Path) {
    let meta = fs::metadata(path).unwrap();
    let later = meta.modified().unwrap() + std::time::Duration::from_secs(120);
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(later).unwrap();
}
