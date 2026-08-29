import { useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { TrackRow } from "../types";
import { Cover } from "../components/Cover";
import { formatDuration, trackArtist, trackTitle } from "../lib/format";
import { usePlayer } from "../lib/playerStore";

interface Props {
  trackById: Map<number, TrackRow>;
}

/** What is playing, and what is behind it. Drag a row to move it. */
export function QueueView({ trackById }: Props) {
  const state = usePlayer();
  const [dragging, setDragging] = useState<number | null>(null);
  const [order, setOrder] = useState<number[] | null>(null);

  const queue = useMemo(() => order ?? state?.queue ?? [], [order, state?.queue]);
  const currentIndex = state?.queueIndex ?? 0;

  if (!state || queue.length === 0) {
    return (
      <div className="empty">
        <p className="empty__body">The queue is empty.</p>
      </div>
    );
  }

  const commit = (next: number[]) => {
    setOrder(next);
    const currentId = queue[currentIndex];
    const start = Math.max(0, next.indexOf(currentId));
    // Reordering re-queues from the track still playing, so moving something
    // further down the list does not restart what is in your ears.
    void invoke("play_tracks", { trackIds: next, start }).catch(() => setOrder(null));
  };

  return (
    <div className="queue">
      <ol className="queue__list">
        {queue.map((id, index) => {
          const track = trackById.get(id);
          const isCurrent = index === currentIndex;
          return (
            <li
              key={`${id}-${index}`}
              className={`queue__row${isCurrent ? " queue__row--current" : ""}`}
              draggable
              onDragStart={() => setDragging(index)}
              onDragOver={(event) => event.preventDefault()}
              onDrop={() => {
                if (dragging === null || dragging === index) return;
                const next = [...queue];
                const [moved] = next.splice(dragging, 1);
                next.splice(index, 0, moved);
                setDragging(null);
                commit(next);
              }}
              onDoubleClick={() => void invoke("play_tracks", { trackIds: queue, start: index })}
            >
              <Cover hash={track?.artHash ?? null} size={64} alt="" className="cover--chip" />
              <span className="queue__title">{track ? trackTitle(track) : `Track ${id}`}</span>
              <span className="queue__artist">{track ? trackArtist(track) : ""}</span>
              <span className="queue__time">
                {track ? formatDuration(track.durationMs) : ""}
              </span>
            </li>
          );
        })}
      </ol>
    </div>
  );
}
