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

  /** Bits the audio actually uses; below `bitDepth` means a padded container. */
  effectiveBits: number | null;
  /** Highest frequency carrying real energy, in Hz. */
  spectralCutoff: number | null;
  /** 0-1 suspicion that a lossless container holds lossy audio. Never a verdict. */
  transcodeScore: number | null;
  bpm: number | null;
  /** Camelot notation, e.g. "8A". */
  musicKey: string | null;
  analyzedAt: number | null;

  /** ReplayGain in dB, from tags or from the analysis pass. */
  replayGainDb: number | null;
  replayGainPeak: number | null;

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

/** One thing that could have altered the audio on its way out. */
export interface Stage {
  name: string;
  /** False renders grey. Three inactive lines is the point of the block. */
  active: boolean;
  detail: string | null;
}

export interface DeviceFormatView {
  sampleRate: number;
  bitsPerChannel: number;
  channels: number;
  /** s16, s24, s32, f32 — 32-bit int and 32-bit float are different things. */
  sampleFormat: string;
}

export interface SignalPath {
  source: SourceSnapshot;
  /** What came out of Symphonia, before any conversion. */
  decoderSampleRate: number;
  decoderFormat: string;
  processing: Stage[];
  deviceName: string | null;
  /** Read back from the hardware. Null means it would not say. */
  deviceFormat: DeviceFormatView | null;
  exclusive: boolean;
  bitPerfect: boolean;
  alteredStages: number;
  reason: string | null;
}

export type RateMode =
  | { mode: "followFile" }
  | { mode: "fixed"; rate: number }
  | { mode: "followAlbum" };

export interface OutputSettings {
  exclusive: boolean;
  rateMode: RateMode;
  replayGain: boolean;
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
  settings: OutputSettings;
  signal: SignalPath | null;
  /** Rates the current device will accept. */
  deviceRates: number[];
}

export interface AlbumRow {
  id: number;
  title: string;
  artist: string | null;
  year: number | null;
  /** Key into the artwork cache, or null when the album has no cover. */
  artHash: string | null;
  trackCount: number;
  durationMs: number;
  /** Null when the album is a mix of formats, which is worth seeing. */
  codec: string | null;
  sampleRate: number | null;
  bitDepth: number | null;
  lossless: boolean;
}

export interface Bucket {
  label: string;
  count: number;
  bytes: number;
}

export interface CollectionHealth {
  total: number;
  analysed: number;
  lossless: number;
  lossy: number;
  /** MP4 containers whose codec has not been resolved yet. */
  unknown: number;
  /** A bigger container than the audio needs. */
  padded: number;
  /** Lossless containers whose spectrum suggests a lossy origin. */
  suspected: number;
  totalBytes: number;
  totalDurationMs: number;
  codecs: Bucket[];
  sampleRates: Bucket[];
  bitDepths: Bucket[];
}

export interface PlaylistRow {
  id: number;
  name: string;
  isSmart: boolean;
  createdAt: number;
  /** Resolved live for smart playlists, counted for plain ones. */
  trackCount: number;
}

/** One track in a generated set, and why it follows the last. */
export interface FlowStep {
  track: TrackRow;
  reason: string;
}

export interface AnalysisStatus {
  remaining: number;
  total: number;
  running: boolean;
}

export interface BatchReport {
  analysed: number;
  failed: number;
  remaining: number;
  elapsedMs: number;
}

export type View =
  | "albums"
  | "tracks"
  | "album"
  | "playing"
  | "queue"
  | "signal"
  | "health"
  | "playlists";
