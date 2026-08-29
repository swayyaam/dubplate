import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type { LibraryStatus, SyncOutcome, TrackRow } from "./types";
import { TrackTable } from "./components/TrackTable";
import { Transport } from "./components/Transport";
import { formatBytes, formatTotalTime } from "./lib/format";

/** Search returns the best matches, not every match; the list is ranked. */
const SEARCH_LIMIT = 2000;

export default function App() {
  const [root, setRoot] = useState<string | null>(null);
  const [tracks, setTracks] = useState<TrackRow[]>([]);
  const [outcome, setOutcome] = useState<SyncOutcome | null>(null);
  const [busy, setBusy] = useState(false);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const [nowPlaying, setNowPlaying] = useState<{ id: number | null; playing: boolean }>({
    id: null,
    playing: false,
  });
  const searchRef = useRef<HTMLInputElement>(null);

  // Search runs per keystroke. Responses can land out of order, so only the
  // newest request is allowed to write to state.
  const queryGeneration = useRef(0);

  const loadTracks = useCallback(async (text: string) => {
    const generation = ++queryGeneration.current;
    const trimmed = text.trim();
    const rows = trimmed
      ? await invoke<TrackRow[]>("search_tracks", { queryText: trimmed, limit: SEARCH_LIMIT })
      : await invoke<TrackRow[]>("list_tracks");
    if (generation === queryGeneration.current) {
      setTracks(rows);
      setSelected(0);
    }
  }, []);

  // Resume the folder from the last session. The index is already on disk, so
  // this is a read, not a rescan.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const status = await invoke<LibraryStatus>("library_status");
        if (cancelled) return;
        setRoot(status.root);
        if (status.root) await loadTracks("");
      } catch (err) {
        if (!cancelled) setError(String(err));
      } finally {
        if (!cancelled) setReady(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [loadTracks]);

  // The watcher re-indexes after any burst of filesystem changes settles.
  useEffect(() => {
    const synced = listen<SyncOutcome>("library:synced", (event) => {
      setOutcome(event.payload);
      void loadTracks(searchRef.current?.value ?? "");
    });
    const failed = listen<string>("library:error", (event) => setError(event.payload));
    return () => {
      void synced.then((un) => un());
      void failed.then((un) => un());
    };
  }, [loadTracks]);

  const chooseFolder = useCallback(async () => {
    const picked = await open({ directory: true, multiple: false, title: "Choose your music folder" });
    if (typeof picked !== "string") return;
    setBusy(true);
    setError(null);
    try {
      const result = await invoke<SyncOutcome>("open_library", { path: picked });
      setRoot(picked);
      setOutcome(result);
      setQuery("");
      await loadTracks("");
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }, [loadTracks]);

  const rescan = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      setOutcome(await invoke<SyncOutcome>("rescan"));
      await loadTracks(query);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }, [loadTracks, query]);

  // "/" or Cmd+F jumps to search from anywhere.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const typingInField = document.activeElement?.tagName === "INPUT";
      if ((event.key === "/" && !typingInField) || (event.key === "f" && event.metaKey)) {
        event.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
      }
      if (event.key === "Escape" && typingInField) searchRef.current?.blur();
      // Space is the universal play/pause, except while typing into a field.
      if (event.key === " " && !typingInField) {
        event.preventDefault();
        void invoke("toggle_play");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const stats = useMemo(() => summarise(tracks), [tracks]);
  const hasLibrary = root !== null;

  // The transport names the playing track without another round trip.
  const trackById = useMemo(() => new Map(tracks.map((track) => [track.id, track])), [tracks]);

  // Playing from a row queues everything currently listed, so a filtered view
  // plays as the playlist it looks like.
  const activate = useCallback(
    (index: number) => {
      const ids = tracks.map((track) => track.id);
      if (ids.length === 0) return;
      void invoke("play_tracks", { trackIds: ids, start: index }).catch((err) =>
        setError(String(err)),
      );
    },
    [tracks],
  );

  const onNowPlayingChange = useCallback((id: number | null, playing: boolean) => {
    setNowPlaying({ id, playing });
  }, []);

  return (
    <div className="app">
      <header className="titlebar" data-tauri-drag-region>
        <span className="wordmark" data-tauri-drag-region>dubplate</span>
        {root && (
          <span className="titlebar__path" data-tauri-drag-region title={root}>
            {homeRelative(root)}
          </span>
        )}
      </header>

      {hasLibrary && (
        <div className="toolbar">
          <input
            ref={searchRef}
            className="search"
            type="text"
            value={query}
            placeholder="Search library"
            spellCheck={false}
            onChange={(event) => {
              setQuery(event.target.value);
              void loadTracks(event.target.value);
            }}
          />
          <button className="button" onClick={() => void rescan()} disabled={busy}>
            {busy ? "Scanning" : "Rescan"}
          </button>
          <button className="button" onClick={() => void chooseFolder()} disabled={busy}>
            Change folder
          </button>
        </div>
      )}

      <main className="main">
        {error && <div className="notice notice--error">{error}</div>}

        {ready && !hasLibrary && (
          <div className="empty">
            <h1 className="empty__title">
              {error ? "Could not read that folder" : "Point dubplate at your music"}
            </h1>
            <p className="empty__body">
              {error
                ? "It may have moved, or be on a drive that is not mounted."
                : "Nothing is copied, moved or written. The folder on disk stays the source of truth."}
            </p>
            <button className="button button--primary" onClick={() => void chooseFolder()}>
              Choose folder
            </button>
          </div>
        )}

        {hasLibrary && tracks.length > 0 && (
          <TrackTable
            tracks={tracks}
            selected={selected}
            onSelect={setSelected}
            onActivate={activate}
            playingId={nowPlaying.id}
            isPlaying={nowPlaying.playing}
          />
        )}

        {hasLibrary && tracks.length === 0 && (
          <div className="empty">
            <p className="empty__body">
              {query ? `Nothing matches “${query}”.` : "No audio files in that folder."}
            </p>
          </div>
        )}
      </main>

      {hasLibrary && <Transport trackById={trackById} onNowPlayingChange={onNowPlayingChange} />}

      {hasLibrary && (
        <footer className="statusbar">
          <span>{stats.count.toLocaleString()} tracks</span>
          <span className="dot" />
          <span>{formatTotalTime(stats.durationMs)}</span>
          <span className="dot" />
          <span>{formatBytes(stats.bytes)}</span>
          <span className="dot" />
          <span title="Share of these tracks using a lossless codec">
            {stats.losslessPct}% lossless
          </span>
          <span className="statusbar__spacer" />
          {outcome && <SyncSummary outcome={outcome} />}
        </footer>
      )}
    </div>
  );
}

/** What the last index pass actually did, rather than just how long it took. */
function SyncSummary({ outcome }: { outcome: SyncOutcome }) {
  const { sync, artwork } = outcome;
  const parts: string[] = [];
  if (sync.added) parts.push(`+${sync.added}`);
  if (sync.updated) parts.push(`~${sync.updated}`);
  if (sync.moved) parts.push(`moved ${sync.moved}`);
  if (sync.removed) parts.push(`−${sync.removed}`);
  if (parts.length === 0) parts.push("no changes");

  return (
    <>
      {sync.errors.length > 0 && (
        <span
          className="statusbar__warn"
          title={sync.errors.slice(0, 8).map((e) => `${e.path}\n  ${e.message}`).join("\n\n")}
        >
          {sync.errors.length} unreadable
        </span>
      )}
      {artwork.artFound > 0 && (
        <span className="statusbar__dim" title="Covers cached at 64, 300 and 1000px">
          {artwork.artFound} covers
        </span>
      )}
      <span className="statusbar__dim">
        {parts.join(" ")} · {sync.unchanged.toLocaleString()} unchanged · {sync.elapsedMs} ms
      </span>
    </>
  );
}

/** /Users/someone/Music -> ~/Music. macOS only, which is what this targets. */
function homeRelative(path: string): string {
  return path.replace(/^\/Users\/[^/]+/, "~");
}

function summarise(tracks: TrackRow[]) {
  let durationMs = 0;
  let bytes = 0;
  let lossless = 0;
  for (const track of tracks) {
    durationMs += track.durationMs;
    bytes += track.size;
    if (track.lossiness === "lossless") lossless += 1;
  }
  return {
    count: tracks.length,
    durationMs,
    bytes,
    losslessPct: tracks.length ? Math.round((lossless / tracks.length) * 100) : 0,
  };
}
