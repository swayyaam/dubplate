import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { PlaylistRow, TrackRow } from "../types";
import { formatDuration, formatKhz, trackArtist, trackTitle } from "../lib/format";

/**
 * Smart playlists store their rules rather than their members, so they answer
 * from the library as it is now: play something and it leaves "Never played"
 * without anything being edited.
 */
export function PlaylistsView({ onPlay }: { onPlay: (tracks: TrackRow[], index: number) => void }) {
  const [playlists, setPlaylists] = useState<PlaylistRow[]>([]);
  const [open, setOpen] = useState<PlaylistRow | null>(null);
  const [tracks, setTracks] = useState<TrackRow[]>([]);

  const refresh = useCallback(async () => {
    setPlaylists(await invoke<PlaylistRow[]>("list_playlists").catch(() => []));
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (!open) {
      setTracks([]);
      return;
    }
    let live = true;
    void invoke<TrackRow[]>("playlist_tracks", { playlistId: open.id })
      .then((rows) => live && setTracks(rows))
      .catch(() => live && setTracks([]));
    return () => {
      live = false;
    };
  }, [open]);

  return (
    <div className="playlists">
      <header className="playlists__head">
        <div>
          <h1 className="health__title">Playlists</h1>
          <p className="health__sub">
            Smart playlists keep their rules, not their contents, and re-answer
            every time you look.
          </p>
        </div>
        <button
          className="button"
          onClick={() => void invoke("add_preset_playlists").then(refresh)}
        >
          Add the ready-made ones
        </button>
      </header>

      {playlists.length === 0 && (
        <p className="block__note">
          None yet. The ready-made set covers hi-res, suspected transcodes,
          padded containers, never played, most played, loved and recently added.
        </p>
      )}

      <ul className="playlists__list">
        {playlists.map((playlist) => (
          <li key={playlist.id}>
            <button
              className={`playlist${open?.id === playlist.id ? " playlist--on" : ""}`}
              onClick={() => setOpen(open?.id === playlist.id ? null : playlist)}
            >
              <span className="playlist__name">{playlist.name}</span>
              <span className="playlist__count">
                {playlist.trackCount.toLocaleString()}
              </span>
              {playlist.isSmart && <span className="playlist__kind">smart</span>}
            </button>
          </li>
        ))}
      </ul>

      {open && (
        <section className="health__list">
          <h2 className="block__title">
            {open.name} — {tracks.length}
          </h2>
          <div className="playlists__actions">
            <button
              className="button button--primary"
              disabled={tracks.length === 0}
              onClick={() => onPlay(tracks, 0)}
            >
              Play
            </button>
            <button
              className="button"
              onClick={() =>
                void invoke("delete_playlist", { playlistId: open.id }).then(() => {
                  setOpen(null);
                  void refresh();
                })
              }
            >
              Delete
            </button>
          </div>
          <ol className="evidence">
            {tracks.map((track, index) => (
              <li key={track.id}>
                <button className="evidence__row" onDoubleClick={() => onPlay(tracks, index)}>
                  <span className="evidence__title">{trackTitle(track)}</span>
                  <span className="evidence__by">{trackArtist(track)}</span>
                  <span className="playlist__fact">
                    {track.codec.toUpperCase()}
                    {track.sampleRate ? ` ${formatKhz(track.sampleRate)}` : ""}
                    {" · "}
                    {formatDuration(track.durationMs)}
                  </span>
                </button>
              </li>
            ))}
            {tracks.length === 0 && (
              <li className="block__note">Nothing matches these rules right now.</li>
            )}
          </ol>
        </section>
      )}
    </div>
  );
}
