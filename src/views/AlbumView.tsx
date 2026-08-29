import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AlbumRow, TrackRow } from "../types";
import { Cover } from "../components/Cover";
import { FormatBadge } from "../components/FormatBadge";
import { formatDuration, formatTotalTime, trackArtist, trackTitle } from "../lib/format";
import { usePlayerValue } from "../lib/playerStore";

interface Props {
  album: AlbumRow;
  onPlay: (tracks: TrackRow[], index: number) => void;
  onBack: () => void;
}

/** One album: art, tracklist, total time, and the format it actually is. */
export function AlbumView({ album, onPlay, onBack }: Props) {
  const [tracks, setTracks] = useState<TrackRow[]>([]);
  const playingId = usePlayerValue((state) => state?.trackId ?? null);

  useEffect(() => {
    let live = true;
    void invoke<TrackRow[]>("album_tracks", { albumId: album.id })
      .then((rows) => {
        if (live) setTracks(rows);
      })
      .catch(() => setTracks([]));
    return () => {
      live = false;
    };
  }, [album.id]);

  const total = useMemo(
    () => tracks.reduce((sum, track) => sum + track.durationMs, 0),
    [tracks],
  );

  return (
    <div className="album">
      <header className="album__head">
        <Cover hash={album.artHash} size={1000} alt={album.title} className="cover--hero" />
        <div className="album__facts">
          <button className="linkish" onClick={onBack}>
            ← Library
          </button>
          <h1 className="album__title">{album.title}</h1>
          <p className="album__artist">{album.artist ?? "Unknown artist"}</p>
          <p className="album__meta">
            {album.year ? `${album.year} · ` : ""}
            {album.trackCount} {album.trackCount === 1 ? "track" : "tracks"} ·{" "}
            {formatTotalTime(total)}
          </p>
          {tracks.length > 0 && (
            <div className="album__badge">
              <FormatBadge track={tracks[0]} />
            </div>
          )}
          <button className="button button--primary" onClick={() => onPlay(tracks, 0)}>
            Play album
          </button>
        </div>
      </header>

      <ol className="album__tracks">
        {tracks.map((track, index) => (
          <li key={track.id}>
            <button
              className={`album__track${track.id === playingId ? " album__track--current" : ""}`}
              onDoubleClick={() => onPlay(tracks, index)}
              onClick={() => onPlay(tracks, index)}
            >
              <span className="album__no">{track.trackNo ?? index + 1}</span>
              <span className="album__name">{trackTitle(track)}</span>
              <span className="album__by">{trackArtist(track)}</span>
              <span className="album__time">{formatDuration(track.durationMs)}</span>
            </button>
          </li>
        ))}
      </ol>
    </div>
  );
}
