/** Mirrors `dubplate_library::track`. Keep in sync with crates/library/src/track.rs. */

export type Lossiness = "lossless" | "lossy" | "unknown";

export interface ScannedTrack {
  path: string;
  fileName: string;
  size: number;
  mtime: number;

  title: string | null;
  artist: string | null;
  album: string | null;
  albumArtist: string | null;
  trackNo: number | null;
  discNo: number | null;
  year: number | null;
  genre: string | null;

  durationMs: number;
  codec: string;
  lossiness: Lossiness;
  sampleRate: number | null;
  /** Always null for lossy codecs -- they have no bit depth. */
  bitDepth: number | null;
  channels: number | null;
  bitrate: number | null;
}

export interface ScanError {
  path: string;
  message: string;
}

export interface ScanReport {
  root: string;
  tracks: ScannedTrack[];
  errors: ScanError[];
  filesSeen: number;
  elapsedMs: number;
}
