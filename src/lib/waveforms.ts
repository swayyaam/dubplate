import { invoke } from "@tauri-apps/api/core";

/**
 * Five lanes of one byte per column, as sent by the `waveform` command.
 *
 * `peak` and `rms` are levels on a decibel scale: 0 is `FLOOR_DB` down or
 * silent, 255 is full scale. `low`/`mid`/`high` are the column's share of
 * energy in each band and sum to about 255, so they say what is playing
 * without saying how loud it is -- that is what `rms` is for.
 */
export interface WaveformData {
  buckets: number;
  peak: Uint8Array;
  rms: Uint8Array;
  low: Uint8Array;
  mid: Uint8Array;
  high: Uint8Array;
}

/** Decibels covered by the stored byte. Must match `waveform::FLOOR_DB`. */
export const FLOOR_DB = 60;

const MAGIC = 0x46_57_50_44; // "DPWF" little-endian
const VERSION = 2;
const LANES = 5;
const HEADER = 12;

/**
 * Waveforms are a full decode pass when they are not already cached, so they
 * are fetched once and kept. Bounded, because a long session touches a lot of
 * tracks and each one is five kilobytes.
 */
const CACHE_LIMIT = 40;
const cache = new Map<number, WaveformData>();
const inFlight = new Map<number, Promise<WaveformData | null>>();

export async function loadWaveform(trackId: number): Promise<WaveformData | null> {
  const cached = cache.get(trackId);
  if (cached) return cached;

  const existing = inFlight.get(trackId);
  if (existing) return existing;

  const request = invoke<ArrayBuffer>("waveform", { trackId })
    .then((raw) => {
      const parsed = parseWaveform(raw);
      if (!parsed) return null;
      if (cache.size >= CACHE_LIMIT) {
        // Oldest first; insertion order is good enough for a display cache.
        const oldest = cache.keys().next().value;
        if (oldest !== undefined) cache.delete(oldest);
      }
      cache.set(trackId, parsed);
      return parsed;
    })
    .catch(() => null)
    .finally(() => inFlight.delete(trackId));

  inFlight.set(trackId, request);
  return request;
}

export function peekWaveform(trackId: number): WaveformData | null {
  return cache.get(trackId) ?? null;
}

/**
 * A raw IPC body arrives as an ArrayBuffer, but the bridge that produces it is
 * injected at runtime rather than imported, so the exact shape is not something
 * the type system here can hold us to. Accepting the three things it could
 * reasonably be costs three lines and turns a possible blank seek bar into a
 * drawn one.
 */
function parseWaveform(raw: unknown): WaveformData | null {
  let bytes: Uint8Array;
  if (raw instanceof ArrayBuffer) bytes = new Uint8Array(raw);
  else if (raw instanceof Uint8Array) bytes = raw;
  else if (Array.isArray(raw)) bytes = Uint8Array.from(raw);
  else return null;

  if (bytes.byteLength < HEADER) return null;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (view.getUint32(0, true) !== MAGIC) return null;
  if (bytes[4] !== VERSION || bytes[5] !== LANES) return null;

  const buckets = view.getUint32(8, true);
  if (buckets === 0 || bytes.byteLength < HEADER + buckets * LANES) return null;

  // Lane-major on the wire, so each lane is a subarray rather than a copy.
  const lane = (index: number) =>
    bytes.subarray(HEADER + index * buckets, HEADER + (index + 1) * buckets);

  return {
    buckets,
    peak: lane(0),
    rms: lane(1),
    low: lane(2),
    mid: lane(3),
    high: lane(4),
  };
}
