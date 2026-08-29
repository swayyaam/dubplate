import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type { AlbumRow, LibraryStatus, SyncOutcome, TrackRow, View, UndoBatch } from "./types";
import { TrackTable } from "./components/TrackTable";
import { TagEditor } from "./components/TagEditor";
import { FilenameTags } from "./components/FilenameTags";
import { Transport } from "./components/Transport";
import { CommandPalette, type Action } from "./components/CommandPalette";
import { AlbumsView } from "./views/AlbumsView";
import { AlbumView } from "./views/AlbumView";
import { NowPlayingView } from "./views/NowPlayingView";
import { QueueView } from "./views/QueueView";
import { SignalPathView } from "./views/SignalPathView";
import { HealthView } from "./views/HealthView";
import { PlaylistsView } from "./views/PlaylistsView";
import { formatBytes, formatTotalTime } from "./lib/format";
import { usePlayerValue } from "./lib/playerStore";

const SEARCH_LIMIT = 2000;
const DEFAULT_ACCENT = "#e8a33d";

const TABS: { view: View; label: string }[] = [
  { view: "albums", label: "Albums" },
  { view: "tracks", label: "Tracks" },
  { view: "playing", label: "Playing" },
  { view: "queue", label: "Queue" },
  { view: "playlists", label: "Playlists" },
  { view: "health", label: "Health" },
];

export default function App() {
  const [root, setRoot] = useState<string | null>(null);
  const [view, setView] = useState<View>("albums");
  const [album, setAlbum] = useState<AlbumRow | null>(null);
  const [tracks, setTracks] = useState<TrackRow[]>([]);
  const [albums, setAlbums] = useState<AlbumRow[]>([]);
  const [outcome, setOutcome] = useState<SyncOutcome | null>(null);
  const [busy, setBusy] = useState(false);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const [paletteOpen, setPaletteOpen] = useState(false);
  /** Track ids marked in the table for a bulk action. */
  const [marked, setMarked] = useState<ReadonlySet<number>>(new Set());
  const [editing, setEditing] = useState(false);
  const [renaming, setRenaming] = useState(false);
  /** The most recent tag write, if it can still be taken back. */
  const [undoable, setUndoable] = useState<UndoBatch | null>(null);

  const refreshUndo = useCallback(() => {
    void invoke<UndoBatch[]>("undo_history")
      .then((batches) => setUndoable(batches[0] ?? null))
      .catch(() => setUndoable(null));
  }, []);
  const searchRef = useRef<HTMLInputElement>(null);
  const queryGeneration = useRef(0);

  const playingId = usePlayerValue((state) => state?.trackId ?? null);
  const isPlaying = usePlayerValue((state) => state?.playing ?? false);

  const trackById = useMemo(() => new Map(tracks.map((t) => [t.id, t])), [tracks]);

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

  const loadAlbums = useCallback(async () => {
    setAlbums(await invoke<AlbumRow[]>("list_albums").catch(() => []));
  }, []);

  const goTo = useCallback((next: View) => {
    setView(next);
    if (next !== "album") setAlbum(null);
    void invoke("set_ui_state", { key: "view", value: next }).catch(() => {});
  }, []);

  // Resume the folder and the view from the last session.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const status = await invoke<LibraryStatus>("library_status");
        if (cancelled) return;
        setRoot(status.root);
        if (status.root) {
          await Promise.all([loadTracks(""), loadAlbums()]);
          // A write from a previous session can still be taken back.
          refreshUndo();
          const saved = await invoke<string | null>("get_ui_state", { key: "view" });
          if (!cancelled && saved && TABS.some((tab) => tab.view === saved)) {
            setView(saved as View);
          }
        }
      } catch (err) {
        if (!cancelled) setError(String(err));
      } finally {
        if (!cancelled) setReady(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [loadTracks, loadAlbums, refreshUndo]);

  // The watcher re-indexes after a burst of filesystem changes settles.
  useEffect(() => {
    const synced = listen<SyncOutcome>("library:synced", (event) => {
      setOutcome(event.payload);
      void loadTracks(searchRef.current?.value ?? "");
      void loadAlbums();
    });
    const failed = listen<string>("library:error", (event) => setError(event.payload));
    return () => {
      void synced.then((un) => un());
      void failed.then((un) => un());
    };
  }, [loadTracks, loadAlbums]);

  // One accent colour, taken from whatever is playing. The whole palette turns
  // with the record rather than staying a fixed brand colour.
  useEffect(() => {
    const hash = playingId !== null ? trackById.get(playingId)?.artHash : null;
    if (!hash) {
      document.documentElement.style.setProperty("--accent", DEFAULT_ACCENT);
      return;
    }
    let live = true;
    void invoke<string | null>("accent_color", { hash })
      .then((colour) => {
        if (live) {
          document.documentElement.style.setProperty("--accent", colour ?? DEFAULT_ACCENT);
        }
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [playingId, trackById]);

  const chooseFolder = useCallback(async () => {
    const picked = await open({ directory: true, multiple: false, title: "Choose your music folder" });
    if (typeof picked !== "string") return;
    setBusy(true);
    setError(null);
    try {
      setOutcome(await invoke<SyncOutcome>("open_library", { path: picked }));
      setRoot(picked);
      setQuery("");
      await Promise.all([loadTracks(""), loadAlbums()]);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }, [loadTracks, loadAlbums]);

  const rescan = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      setOutcome(await invoke<SyncOutcome>("rescan"));
      await Promise.all([loadTracks(query), loadAlbums()]);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }, [loadTracks, loadAlbums, query]);

  const playFrom = useCallback((rows: TrackRow[], index: number) => {
    const ids = rows.map((track) => track.id);
    if (ids.length === 0) return;
    void invoke("play_tracks", { trackIds: ids, start: index }).catch((err) =>
      setError(String(err)),
    );
  }, []);

  const actions = useMemo<Action[]>(
    () => [
      { id: "play", label: isPlaying ? "Pause" : "Play", hint: "Space", run: () => void invoke("toggle_play") },
      { id: "next", label: "Next track", run: () => void invoke("next_track") },
      { id: "prev", label: "Previous track", run: () => void invoke("previous_track") },
      { id: "shuffle", label: "Toggle shuffle", run: () => void invoke("set_shuffle", { shuffle: true }) },
      ...TABS.map((tab) => ({
        id: `go-${tab.view}`,
        label: `Go to ${tab.label}`,
        run: () => goTo(tab.view),
      })),
      { id: "signal", label: "Show signal path", run: () => goTo("signal") },
      { id: "analyse", label: "Analyse library", run: () => void invoke("start_analysis") },
      {
        id: "flow",
        label: "Build a set from the playing track",
        hint: "tempo and key",
        run: () => {
          if (playingId === null) return;
          void invoke<{ track: TrackRow }[]>("build_set", { trackId: playingId, length: 20 })
            .then((steps) =>
              steps.length > 1
                ? invoke("play_tracks", {
                    trackIds: steps.map((step) => step.track.id),
                    start: 0,
                  }).then(() => goTo("queue"))
                : undefined,
            )
            .catch((err) => setError(String(err)));
        },
      },
      { id: "rescan", label: "Rescan library", run: () => void rescan() },
      { id: "folder", label: "Change music folder", run: () => void chooseFolder() },
    ],
    [isPlaying, goTo, rescan, chooseFolder, playingId],
  );

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const typing =
        document.activeElement?.tagName === "INPUT" ||
        document.activeElement?.tagName === "TEXTAREA";

      if (event.key === "k" && event.metaKey) {
        event.preventDefault();
        setPaletteOpen((open) => !open);
        return;
      }
      if (paletteOpen) return;
      if ((event.key === "/" && !typing) || (event.key === "f" && event.metaKey)) {
        event.preventDefault();
        goTo("tracks");
        requestAnimationFrame(() => {
          searchRef.current?.focus();
          searchRef.current?.select();
        });
      }
      if (event.key === "Escape" && typing) (document.activeElement as HTMLElement).blur();
      if (event.key === " " && !typing) {
        event.preventDefault();
        void invoke("toggle_play");
      }
      // Cmd+1..4 for the views, the way every other Mac app does it.
      if (event.metaKey && /^[1-6]$/.test(event.key)) {
        event.preventDefault();
        goTo(TABS[Number(event.key) - 1].view);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [paletteOpen, goTo]);

  const stats = useMemo(() => summarise(tracks), [tracks]);
  const hasLibrary = root !== null;

  return (
    <div className="app">
      <header className="titlebar" data-tauri-drag-region>
        <span className="wordmark" data-tauri-drag-region>dubplate</span>
        {editing && (
        <TagEditor
          tracks={tracks.filter((track) => marked.has(track.id))}
          onClose={() => setEditing(false)}
          onWritten={() => {
            refreshUndo();
            void loadTracks(query);
            void loadAlbums();
          }}
        />
      )}

      {renaming && (
        <FilenameTags
          onClose={() => setRenaming(false)}
          onWritten={() => {
            refreshUndo();
            void loadTracks(query);
            void loadAlbums();
          }}
        />
      )}

      {hasLibrary && (
          <nav className="tabs">
            {TABS.map((tab) => (
              <button
                key={tab.view}
                className={`tab${
                  view === tab.view ||
                  (view === "album" && tab.view === "albums") ||
                  (view === "signal" && tab.view === "playing")
                    ? " tab--on"
                    : ""
                }`}
                onClick={() => goTo(tab.view)}
              >
                {tab.label}
              </button>
            ))}
          </nav>
        )}
        <span className="titlebar__spacer" data-tauri-drag-region />
        {hasLibrary && (
          <button className="hintkey" onClick={() => setPaletteOpen(true)} title="Command palette">
            ⌘K
          </button>
        )}
      </header>

      {hasLibrary && view === "tracks" && (
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

        {hasLibrary && view === "albums" && (
          albums.length > 0 ? (
            <AlbumsView
              albums={albums}
              onOpen={(picked) => {
                setAlbum(picked);
                setView("album");
              }}
            />
          ) : (
            <div className="empty">
              <p className="empty__body">
                No albums yet. Untagged files still show under Tracks.
              </p>
            </div>
          )
        )}

        {hasLibrary && view === "album" && album && (
          <AlbumView album={album} onPlay={playFrom} onBack={() => goTo("albums")} />
        )}

        {hasLibrary && view === "tracks" && tracks.length > 0 && (
          <>
            <div className="tablebar">
              <span className="tablebar__count">
                {marked.size > 0
                  ? `${marked.size} selected`
                  : `${tracks.length} track${tracks.length === 1 ? "" : "s"}`}
              </span>
              <button
                type="button"
                className="chip"
                disabled={marked.size === 0}
                onClick={() => setEditing(true)}
              >
                Edit tags
              </button>
              <button type="button" className="chip" onClick={() => setRenaming(true)}>
                Tags from filenames…
              </button>
              {undoable && (
                <button
                  type="button"
                  className="chip chip--quiet"
                  title={`${undoable.description} · ${undoable.tracks} track${undoable.tracks === 1 ? "" : "s"}`}
                  onClick={() => {
                    void invoke("undo_last").then(() => {
                      refreshUndo();
                      void loadTracks(query);
                      void loadAlbums();
                    });
                  }}
                >
                  Undo {undoable.description.toLowerCase()} ({undoable.tracks})
                </button>
              )}
            </div>
            <TrackTable
              tracks={tracks}
              selected={selected}
              onSelect={setSelected}
              onActivate={(index) => playFrom(tracks, index)}
              playingId={playingId}
              isPlaying={isPlaying}
              marked={marked}
              onMarkedChange={setMarked}
            />
          </>
        )}

        {hasLibrary && view === "tracks" && tracks.length === 0 && (
          <div className="empty">
            <p className="empty__body">
              {query ? `Nothing matches “${query}”.` : "No audio files in that folder."}
            </p>
          </div>
        )}

        {hasLibrary && view === "playing" && (
          <NowPlayingView
            trackById={trackById}
            onOpenSignal={() => goTo("signal")}
            onOpenQueue={() => goTo("queue")}
          />
        )}
        {hasLibrary && view === "signal" && <SignalPathView onBack={() => goTo("playing")} />}
        {hasLibrary && view === "playlists" && <PlaylistsView onPlay={playFrom} />}
        {hasLibrary && view === "health" && <HealthView onPlay={playFrom} />}
        {hasLibrary && view === "queue" && <QueueView trackById={trackById} />}
      </main>

      {hasLibrary && (
        <Transport
          trackById={trackById}
          onOpenNowPlaying={() => goTo("playing")}
          onOpenSignal={() => goTo("signal")}
        />
      )}

      {hasLibrary && view === "tracks" && (
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

      <CommandPalette
        open={paletteOpen}
        onClose={() => setPaletteOpen(false)}
        actions={actions}
        onPlayTrack={(track) => playFrom([track], 0)}
      />
    </div>
  );
}

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
