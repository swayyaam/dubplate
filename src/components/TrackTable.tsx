import { memo, useCallback, useEffect, useRef } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { TrackRow } from "../types";
import { formatDuration, trackAlbum, trackArtist, trackTitle } from "../lib/format";
import { FormatBadge } from "./FormatBadge";

const ROW_HEIGHT = 44;

interface Props {
  tracks: TrackRow[];
  selected: number;
  onSelect: (index: number) => void;
}

/**
 * Virtualized library list. Only the visible rows exist in the DOM, which is
 * what keeps a 10,000 track library scrolling at 60fps.
 *
 * Arrow keys move the selection and pull it into view, so the list is fully
 * usable without the mouse.
 */
export function TrackTable({ tracks, selected, onSelect }: Props) {
  const scrollRef = useRef<HTMLDivElement>(null);

  const virtualizer = useVirtualizer({
    count: tracks.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 12,
  });

  // Keep the selected row on screen when the selection moves by keyboard.
  useEffect(() => {
    if (selected >= 0 && selected < tracks.length) {
      virtualizer.scrollToIndex(selected, { align: "auto" });
    }
    // virtualizer identity is stable enough; re-running on selection is the point
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selected, tracks.length]);

  const onKeyDown = useCallback(
    (event: React.KeyboardEvent) => {
      if (tracks.length === 0) return;
      const clamp = (n: number) => Math.max(0, Math.min(tracks.length - 1, n));
      const page = Math.max(1, Math.floor((scrollRef.current?.clientHeight ?? 0) / ROW_HEIGHT) - 1);

      switch (event.key) {
        case "ArrowDown":
          event.preventDefault();
          onSelect(clamp(selected + 1));
          break;
        case "ArrowUp":
          event.preventDefault();
          onSelect(clamp(selected - 1));
          break;
        case "PageDown":
          event.preventDefault();
          onSelect(clamp(selected + page));
          break;
        case "PageUp":
          event.preventDefault();
          onSelect(clamp(selected - page));
          break;
        case "Home":
          event.preventDefault();
          onSelect(0);
          break;
        case "End":
          event.preventDefault();
          onSelect(tracks.length - 1);
          break;
      }
    },
    [tracks.length, selected, onSelect],
  );

  return (
    <div className="table">
      <div className="table__head" role="row">
        <span className="col col--num">#</span>
        <span className="col">Title</span>
        <span className="col">Artist</span>
        <span className="col">Album</span>
        <span className="col col--format">Format</span>
        <span className="col col--time">Time</span>
      </div>

      <div
        ref={scrollRef}
        className="table__body"
        tabIndex={0}
        role="listbox"
        aria-label="Tracks"
        onKeyDown={onKeyDown}
      >
        <div className="table__canvas" style={{ height: virtualizer.getTotalSize() }}>
          {virtualizer.getVirtualItems().map((item) => (
            <Row
              key={tracks[item.index].id}
              track={tracks[item.index]}
              index={item.index}
              offset={item.start}
              isSelected={item.index === selected}
              onSelect={onSelect}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

interface RowProps {
  track: TrackRow;
  index: number;
  offset: number;
  isSelected: boolean;
  onSelect: (index: number) => void;
}

function RowImpl({ track, index, offset, isSelected, onSelect }: RowProps) {
  return (
    <div
      className={`row${isSelected ? " row--selected" : ""}`}
      style={{ transform: `translateY(${offset}px)`, height: ROW_HEIGHT }}
      role="option"
      aria-selected={isSelected}
      onClick={() => onSelect(index)}
      title={track.path}
    >
      <span className="col col--num">{track.trackNo ?? ""}</span>
      <span className="col col--title">{trackTitle(track)}</span>
      <span className="col col--dim">{trackArtist(track)}</span>
      <span className="col col--dim">{trackAlbum(track)}</span>
      <span className="col col--format">
        <FormatBadge track={track} />
      </span>
      <span className="col col--time">{formatDuration(track.durationMs)}</span>
    </div>
  );
}

// Rows re-render only when their own data or selection changes, not when the
// window scrolls. The 30fps position tick in phase 2 makes this matter.
const Row = memo(RowImpl);
