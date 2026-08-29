import { invoke } from "@tauri-apps/api/core";

/**
 * Peaks are a full decode pass -- about a tenth of a second per track -- so they
 * are fetched once and kept. Bounded, because a long session touches a lot of
 * tracks and each set is a thousand floats.
 */
const CACHE_LIMIT = 40;
const cache = new Map<number, number[]>();
const inFlight = new Map<number, Promise<number[] | null>>();

export async function loadWaveform(trackId: number): Promise<number[] | null> {
  const cached = cache.get(trackId);
  if (cached) return cached;

  const existing = inFlight.get(trackId);
  if (existing) return existing;

  const request = invoke<number[]>("waveform", { trackId })
    .then((peaks) => {
      if (cache.size >= CACHE_LIMIT) {
        // Oldest first; insertion order is good enough for a display cache.
        const oldest = cache.keys().next().value;
        if (oldest !== undefined) cache.delete(oldest);
      }
      cache.set(trackId, peaks);
      return peaks;
    })
    .catch(() => null)
    .finally(() => inFlight.delete(trackId));

  inFlight.set(trackId, request);
  return request;
}

export function peekWaveform(trackId: number): number[] | null {
  return cache.get(trackId) ?? null;
}
