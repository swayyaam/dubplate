//! Waveform peaks on disk, keyed by content.
//!
//! Keyed by the track's content key rather than its id, so a file that moves
//! keeps its waveform for the same reason it keeps its play count.

use std::path::{Path, PathBuf};

pub struct PeaksCache {
    root: PathBuf,
}

impl PeaksCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn path_for(&self, content_key: &str) -> PathBuf {
        let safe: String = content_key
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        self.root
            .join(&safe[..2.min(safe.len())])
            .join(format!("{safe}.pk"))
    }

    pub fn read(&self, content_key: &str) -> Option<Vec<f32>> {
        let bytes = std::fs::read(self.path_for(content_key)).ok()?;
        if bytes.len() % 4 != 0 || bytes.is_empty() {
            return None;
        }
        Some(
            bytes
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect(),
        )
    }

    pub fn write(&self, content_key: &str, peaks: &[f32]) -> std::io::Result<()> {
        let path = self.path_for(content_key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut bytes = Vec::with_capacity(peaks.len() * 4);
        for peak in peaks {
            bytes.extend_from_slice(&peak.to_le_bytes());
        }
        // Write and rename, so a crash cannot leave a half-written waveform
        // that later runs would trust.
        let temp = path.with_extension("pk.tmp");
        std::fs::write(&temp, &bytes)?;
        std::fs::rename(&temp, &path)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}
