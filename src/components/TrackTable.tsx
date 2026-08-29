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
  /** Double-click or Enter: start playing from this row. */
  onActivate: (index: number) => void;
  /** The track the engine is on, so the row can show it. */
  playingId: number | null;
  isPlaying: boolean;
  /**
   * Track ids marked for a bulk action, if the view supports one.
   *
   * Separate from `selected`, which is the keyboard cursor. Editing twelve
   * tracks at once needs a set; playing one needs a cursor; conflating them
   * would mean arrowing through a list silently changed what an edit applies
   * to.
   */
  marked?: ReadonlySet<number>;
  onMarkedChange?: (ids: ReadonlySet<number>) => void;
}

/**
 * Virtualized library list. Only the visible rows exist in the DOM, which is
 * what keeps a 10,000 track library scrolling at 60fps.
 *
 * Arrow keys move the selection and pull it into view, so the list is fully
 * usable without the mouse.
 */
export function TrackTable({
  tracks,
  selected,
  onSelect,
  onActivate,
  playingId,
  isPlaying,
  marked,
  onMarkedChange,
}: Props) {
  const scrollRef = useRef<HTMLDivElement>(null);
  // Where a shift-click range starts. Set by every plain or toggling click.
  const anchorRef = useRef<number>(0);

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

  /**
   * Click behaviour follows the platform: plain replaces, Cmd toggles one,
   * Shift takes the range from the anchor.
   */
  const onRowClick = useCallback(
    (index: number, event: React.MouseEvent) => {
      onSelect(index);
      if (!onMarkedChange) return;

      if (event.shiftKey) {
        const from = Math.min(anchorRef.current, index);
        const to = Math.max(anchorRef.current, index);
        const next = new Set<number>();
        for (let i = from; i <= to; i += 1) next.add(tracks[i].id);
        onMarkedChange(next);
        return;
      }

      anchorRef.current = index;
      const id = tracks[index].id;
      if (event.metaKey || event.ctrlKey) {
        const next = new Set(marked ?? []);
        if (next.has(id)) next.delete(id);
        else next.add(id);
        onMarkedChange(next);
        return;
      }
      // A plain click on an existing multi-selection clears it, which is what
      // every list on this platform does.
      onMarkedChange(new Set([id]));
    },
    [tracks, marked, onMarkedChange, onSelect],
  );

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
        case "Enter":
          event.preventDefault();
          onActivate(selected);
          break;
        case "a":
          if (event.metaKey || event.ctrlKey) {
            event.preventDefault();
            onMarkedChange?.(new Set(tracks.map((track) => track.id)));
          }
          break;
        case "Escape":
          if (marked && marked.size > 0) {
            event.preventDefault();
            onMarkedChange?.(new Set());
          }
          break;
      }
    },
    [tracks, selected, onSelect, onActivate, marked, onMarkedChange],
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
              isMarked={marked?.has(tracks[item.index].id) ?? false}
              isCurrent={tracks[item.index].id === playingId}
              isPlaying={isPlaying}
              onRowClick={onRowClick}
              onActivate={onActivate}
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
  isMarked: boolean;
  isCurrent: boolean;
  isPlaying: boolean;
  onRowClick: (index: number, event: React.MouseEvent) => void;
  onActivate: (index: number) => void;
}

function RowImpl({
  track,
  index,
  offset,
  isSelected,
  isMarked,
  isCurrent,
  isPlaying,
  onRowClick,
  onActivate,
}: RowProps) {
  return (
    <div
      className={`row${isSelected ? " row--selected" : ""}${isMarked ? " row--marked" : ""}${isCurrent ? " row--current" : ""}`}
      style={{ transform: `translateY(${offset}px)`, height: ROW_HEIGHT }}
      role="option"
      aria-selected={isSelected}
      onClick={(event) => onRowClick(index, event)}
      onDoubleClick={() => onActivate(index)}
      title={track.path}
    >
      <span className="col col--num">
        {isCurrent ? (isPlaying ? "▶" : "⏸") : (track.trackNo ?? "")}
      </span>
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
