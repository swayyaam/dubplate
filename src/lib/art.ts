import { convertFileSrc } from "@tauri-apps/api/core";

/** Widths the artwork cache holds. Asking for anything else gets a 404. */
export const ART_SIZES = [64, 300, 1000] as const;
export type ArtSize = (typeof ART_SIZES)[number];

/**
 * Covers come from a custom protocol rather than IPC: an art grid asks for
 * hundreds at once, and the cache already holds each one at the size being
 * drawn. The webview caches them like any other image.
 */
export function artUrl(hash: string | null | undefined, size: ArtSize): string | null {
  if (!hash) return null;
  return `art://localhost/${hash}/${size}`;
}

// Referenced so the Tauri helper stays imported if a future path needs it.
export const _convertFileSrc = convertFileSrc;
