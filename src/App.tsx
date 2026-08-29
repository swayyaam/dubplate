import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { ScanReport, ScannedTrack } from "./types";
import { TrackTable } from "./components/TrackTable";
import { formatBytes, formatTotalTime, trackAlbum, trackArtist, trackTitle } from "./lib/format";

const ROOT_KEY = "dubplate.libraryRoot";

export default function App() {
  const [report, setReport] = useState<ScanReport | null>(null);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const searchRef = useRef<HTMLInputElement>(null);

  const scan = useCallback(async (path: string) => {
    setScanning(true);
    setError(null);
    try {
      const result = await invoke<ScanReport>("scan_library", { path });
      setReport(result);
      setSelected(0);
      localStorage.setItem(ROOT_KEY, path);
    } catch (err) {
      setError(String(err));
      setReport(null);
    } finally {
      setScanning(false);
    }
  }, []);

  const chooseFolder = useCallback(async () => {
    const picked = await open({ directory: true, multiple: false, title: "Choose your music folder" });
    if (typeof picked === "string") await scan(picked);
  }, [scan]);

  // Remember the last folder and pick it back up on launch. The full "restore
  // queue, position and view" pass lands in phase 3; this is the first piece.
  const didAutoScan = useRef(false);
  useEffect(() => {
    if (didAutoScan.current) return;
    didAutoScan.current = true;
    const saved = localStorage.getItem(ROOT_KEY);
    if (saved) void scan(saved);
  }, [scan]);

  // "/" or Cmd+F jumps to the filter from anywhere.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const typingInField = document.activeElement?.tagName === "INPUT";
      if ((event.key === "/" && !typingInField) || (event.key === "f" && event.metaKey)) {
        event.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
      }
      if (event.key === "Escape" && typingInField) searchRef.current?.blur();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const tracks = report?.tracks ?? [];

  // Client-side filter, standing in for the FTS5 index that arrives in phase 1.
  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return tracks;
    return tracks.filter((track) =>
      `${trackTitle(track)} ${trackArtist(track)} ${trackAlbum(track)}`.toLowerCase().includes(needle),
    );
  }, [tracks, query]);

  const stats = useMemo(() => summarise(filtered), [filtered]);

  return (
    <div className="app">
      <header className="titlebar" data-tauri-drag-region>
        <span className="wordmark" data-tauri-drag-region>dubplate</span>
        {report && (
          <span className="titlebar__path" data-tauri-drag-region title={report.root}>
            {homeRelative(report.root)}
          </span>
        )}
      </header>

      {report && (
        <div className="toolbar">
          <input
            ref={searchRef}
            className="search"
            type="text"
            value={query}
            placeholder="Filter library"
            spellCheck={false}
            onChange={(event) => {
              setQuery(event.target.value);
              setSelected(0);
            }}
          />
          <button className="button" onClick={() => void scan(report.root)} disabled={scanning}>
            {scanning ? "Scanning" : "Rescan"}
          </button>
          <button className="button" onClick={() => void chooseFolder()} disabled={scanning}>
            Change folder
          </button>
        </div>
      )}

      <main className="main">
        {error && <div className="notice notice--error">{error}</div>}

        {/* Also shown after a failed scan: a saved folder that has moved must not
            leave the app with an error and no way to pick another one. */}
        {!report && !scanning && (
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

        {scanning && !report && <div className="empty"><p className="empty__body">Reading your library…</p></div>}

        {report && filtered.length > 0 && (
          <TrackTable tracks={filtered} selected={selected} onSelect={setSelected} />
        )}

        {report && filtered.length === 0 && (
          <div className="empty">
            <p className="empty__body">
              {tracks.length === 0 ? "No audio files in that folder." : `Nothing matches “${query}”.`}
            </p>
          </div>
        )}
      </main>

      {report && (
        <footer className="statusbar">
          <span>{stats.count.toLocaleString()} tracks</span>
          <span className="dot" />
          <span>{formatTotalTime(stats.durationMs)}</span>
          <span className="dot" />
          <span>{formatBytes(stats.bytes)}</span>
          <span className="dot" />
          <span className="statusbar__health" title="Share of the library that is a lossless codec">
            {stats.losslessPct}% lossless
          </span>
          <span className="statusbar__spacer" />
          {report.errors.length > 0 && (
            <span
              className="statusbar__warn"
              title={report.errors.slice(0, 8).map((e) => `${e.path}\n  ${e.message}`).join("\n\n")}
            >
              {report.errors.length} unreadable
            </span>
          )}
          <span className="statusbar__dim">scanned {report.filesSeen.toLocaleString()} files in {report.elapsedMs} ms</span>
        </footer>
      )}
    </div>
  );
}

/** /Users/someone/Music -> ~/Music. macOS only, which is what this targets. */
function homeRelative(path: string): string {
  return path.replace(/^\/Users\/[^/]+/, "~");
}

function summarise(tracks: ScannedTrack[]) {
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
