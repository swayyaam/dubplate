import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { TrackRow, View } from "../types";
import { trackArtist, trackTitle } from "../lib/format";

export interface Action {
  id: string;
  label: string;
  hint?: string;
  run: () => void;
}

interface Props {
  open: boolean;
  onClose: () => void;
  actions: Action[];
  onPlayTrack: (track: TrackRow) => void;
}

/** Matches typed at this rate feel instant; FTS5 answers in well under 1ms. */
const SEARCH_LIMIT = 8;

/**
 * Search everything, run anything, without touching the mouse.
 *
 * Track results come from the same FTS index the library search uses, so what
 * matches here matches there.
 */
export function CommandPalette({ open, onClose, actions, onPlayTrack }: Props) {
  const [query, setQuery] = useState("");
  const [tracks, setTracks] = useState<TrackRow[]>([]);
  const [cursor, setCursor] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const generation = useRef(0);

  useEffect(() => {
    if (open) {
      setQuery("");
      setTracks([]);
      setCursor(0);
      // Focus after the element exists, not before.
      requestAnimationFrame(() => inputRef.current?.focus());
    }
  }, [open]);

  useEffect(() => {
    const trimmed = query.trim();
    if (!trimmed) {
      setTracks([]);
      return;
    }
    const mine = ++generation.current;
    void invoke<TrackRow[]>("search_tracks", { queryText: trimmed, limit: SEARCH_LIMIT })
      .then((rows) => {
        // Only the newest query is allowed to write: responses can land out of
        // order, and a stale one would fight the user's typing.
        if (mine === generation.current) setTracks(rows);
      })
      .catch(() => {
        if (mine === generation.current) setTracks([]);
      });
  }, [query]);

  const matchedActions = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return actions;
    return actions.filter((action) => action.label.toLowerCase().includes(needle));
  }, [actions, query]);

  const rows = useMemo(
    () => [
      ...matchedActions.map((action) => ({ kind: "action" as const, action })),
      ...tracks.map((track) => ({ kind: "track" as const, track })),
    ],
    [matchedActions, tracks],
  );

  const run = useCallback(
    (index: number) => {
      const row = rows[index];
      if (!row) return;
      if (row.kind === "action") row.action.run();
      else onPlayTrack(row.track);
      onClose();
    },
    [rows, onClose, onPlayTrack],
  );

  useEffect(() => {
    setCursor((current) => Math.min(current, Math.max(0, rows.length - 1)));
  }, [rows.length]);

  if (!open) return null;

  return (
    <div className="palette__scrim" onPointerDown={onClose}>
      <div
        className="palette"
        role="dialog"
        aria-label="Command palette"
        onPointerDown={(event) => event.stopPropagation()}
      >
        <input
          ref={inputRef}
          // Both: autoFocus covers the mount, and the effect above covers
          // reopening, when the element is reused rather than remounted.
          autoFocus
          className="palette__input"
          value={query}
          placeholder="Search tracks, or run a command"
          spellCheck={false}
          onChange={(event) => {
            setQuery(event.target.value);
            setCursor(0);
          }}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown") {
              event.preventDefault();
              setCursor((c) => Math.min(rows.length - 1, c + 1));
            } else if (event.key === "ArrowUp") {
              event.preventDefault();
              setCursor((c) => Math.max(0, c - 1));
            } else if (event.key === "Enter") {
              event.preventDefault();
              run(cursor);
            } else if (event.key === "Escape") {
              event.preventDefault();
              onClose();
            }
          }}
        />

        <ul className="palette__list">
          {rows.length === 0 && <li className="palette__none">No matches</li>}
          {rows.map((row, index) => (
            <li
              key={row.kind === "action" ? row.action.id : `t${row.track.id}`}
              className={`palette__row${index === cursor ? " palette__row--on" : ""}`}
              onPointerEnter={() => setCursor(index)}
              onPointerUp={() => run(index)}
            >
              {row.kind === "action" ? (
                <>
                  <span className="palette__label">{row.action.label}</span>
                  {row.action.hint && <span className="palette__hint">{row.action.hint}</span>}
                </>
              ) : (
                <>
                  <span className="palette__label">{trackTitle(row.track)}</span>
                  <span className="palette__hint">{trackArtist(row.track)}</span>
                </>
              )}
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}

export type { View };
