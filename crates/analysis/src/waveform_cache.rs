//! Waveforms on disk, keyed by content.
//!
//! Keyed by the track's content key rather than its id, so a file that moves
//! keeps its waveform for the same reason it keeps its play count.

use std::path::{Path, PathBuf};

use crate::waveform::Waveform;

pub struct WaveformCache {
    root: PathBuf,
}

impl WaveformCache {
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

    /// The stored waveform, or `None` if it is absent, damaged, or from an
    /// older format. Every one of those means the same thing to the caller:
    /// compute it again.
    pub fn read(&self, content_key: &str) -> Option<Waveform> {
        Waveform::from_bytes(&std::fs::read(self.path_for(content_key)).ok()?)
    }

    /// The stored bytes, ready to go straight over the wire without being
    /// parsed and re-serialised on the way past.
    pub fn read_bytes(&self, content_key: &str) -> Option<Vec<u8>> {
        let bytes = std::fs::read(self.path_for(content_key)).ok()?;
        // Validated before it is trusted, so a stale file is regenerated
        // rather than sent to the canvas as noise.
        Waveform::from_bytes(&bytes)?;
        Some(bytes)
    }

    pub fn write(&self, content_key: &str, waveform: &Waveform) -> std::io::Result<()> {
        let path = self.path_for(content_key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write and rename, so a crash cannot leave a half-written waveform
        // that later runs would trust.
        let temp = path.with_extension("pk.tmp");
        std::fs::write(&temp, waveform.to_bytes())?;
        std::fs::rename(&temp, &path)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}
