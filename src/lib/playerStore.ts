import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { PlayerState } from "../types";

/**
 * One poll of the engine at 30fps, shared by every view.
 *
 * Deliberately not React context: context re-renders every consumer on every
 * tick, and the album grid holds hundreds of tiles. Components subscribe to the
 * single value they need instead, so a moving position updates the seek bar and
 * nothing else.
 */
const POLL_MS = 1000 / 30;

let snapshot: PlayerState | null = null;
const listeners = new Set<() => void>();
let timer: number | undefined;

function emit() {
  for (const listener of listeners) listener();
}

async function tick() {
  try {
    snapshot = await invoke<PlayerState>("player_state");
    emit();
  } catch {
    // The engine is not up yet; the next tick will find it.
  }
  timer = window.setTimeout(tick, POLL_MS);
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  if (timer === undefined) void tick();
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) {
      window.clearTimeout(timer);
      timer = undefined;
    }
  };
}

/** The whole state. Use only where every field matters, like the transport. */
export function usePlayer(): PlayerState | null {
  return useSyncExternalStore(subscribe, () => snapshot);
}

/**
 * One value from the state. The selector must return a primitive: re-rendering
 * is decided by `Object.is`, so returning a fresh object every tick would defeat
 * the point of subscribing narrowly.
 */
export function usePlayerValue<T>(select: (state: PlayerState | null) => T): T {
  return useSyncExternalStore(subscribe, () => select(snapshot));
}

export function playerSnapshot(): PlayerState | null {
  return snapshot;
}
