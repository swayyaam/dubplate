import type { TrackRow } from "../types";

/** 3:42, or 1:02:15 once it passes an hour. */
export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "--:--";
  const total = Math.round(ms / 1000);
  const seconds = total % 60;
  const minutes = Math.floor(total / 60) % 60;
  const hours = Math.floor(total / 3600);
  const pad = (n: number) => String(n).padStart(2, "0");
  return hours > 0 ? `${hours}:${pad(minutes)}:${pad(seconds)}` : `${minutes}:${pad(seconds)}`;
}

/** Total runtime of a whole library, read as "4 days, 7 hours". */
export function formatTotalTime(ms: number): string {
  const hours = ms / 3_600_000;
  if (hours < 1) return `${Math.round(ms / 60_000)} min`;
  if (hours < 48) return `${hours.toFixed(1)} hours`;
  return `${Math.floor(hours / 24)} days, ${Math.round(hours % 24)} hours`;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

/** 44100 -> "44.1", 48000 -> "48". Trailing ".0" is noise. */
export function formatKhz(sampleRate: number): string {
  const khz = sampleRate / 1000;
  return Number.isInteger(khz) ? String(khz) : khz.toFixed(1);
}

/**
 * The spec line for a track, e.g. "24/96" for lossless or "320k" for lossy.
 *
 * Lossy codecs deliberately have no depth to show -- they store frequency
 * coefficients, not samples -- so they get a bitrate instead.
 */
export function formatSpec(track: TrackRow): string | null {
  const { bitDepth, sampleRate, bitrate, lossiness } = track;
  if (lossiness === "lossy") return bitrate ? `${bitrate}k` : null;
  if (bitDepth && sampleRate) return `${bitDepth}/${formatKhz(sampleRate)}`;
  if (sampleRate) return formatKhz(sampleRate);
  return null;
}

export function trackTitle(track: TrackRow): string {
  const title = track.title?.trim();
  if (title) return title;
  return track.fileName.replace(/\.[^.]+$/, "");
}

export function trackArtist(track: TrackRow): string {
  return track.artist?.trim() || track.albumArtist?.trim() || "Unknown artist";
}

export function trackAlbum(track: TrackRow): string {
  return track.album?.trim() || "";
}
