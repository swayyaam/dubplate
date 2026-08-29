/** Mirrors `dubplate_library::model` and the Tauri command layer. */

export type Lossiness = "lossless" | "lossy" | "unknown";

export interface TrackRow {
  id: number;
  path: string;
  fileName: string;
  size: number;

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

  /** Key into the artwork cache, or null if the album has no cover. */
  artHash: string | null;
  playCount: number;
  loved: boolean;
  addedAt: number;
  lastPlayed: number | null;
}

export interface ScanError {
  path: string;
  message: string;
}

export interface SyncReport {
  root: string;
  filesSeen: number;
  added: number;
  updated: number;
  /** Recognised at a new path by content, so play counts survived. */
  moved: number;
  removed: number;
  /** Skipped because (mtime, size) still matched the index. */
  unchanged: number;
  errors: ScanError[];
  elapsedMs: number;
}

export interface ArtworkReport {
  albumsChecked: number;
  artFound: number;
  artMissing: number;
  errors: ScanError[];
  elapsedMs: number;
}

export interface SyncOutcome {
  sync: SyncReport;
  artwork: ArtworkReport;
}

export interface LibraryStatus {
  root: string | null;
  trackCount: number;
}

export type RepeatMode = "off" | "all" | "one";

/** What the file is, straight from the stream. Never from tags. */
export interface SourceSnapshot {
  codec: string;
  sampleRate: number;
  channels: number;
  /** Null for lossy codecs, which have no bit depth at all. */
  bitsPerSample: number | null;
}

export interface PlayerState {
  playing: boolean;
  trackId: number | null;
  queue: number[];
  queueIndex: number;
  positionMs: number;
  durationMs: number;
  volume: number;
  repeat: RepeatMode;
  shuffle: boolean;
  source: SourceSnapshot | null;
  device: string | null;
  /** What the device is running at, which is not always the file's rate. */
  deviceSampleRate: number | null;
  /** Non-zero means the decoder fell behind and the device ran dry. */
  underruns: number;
  error: string | null;
}
