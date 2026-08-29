import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { AlbumRow } from "../types";
import { Cover } from "../components/Cover";
import { formatKhz } from "../lib/format";

const MIN_TILE = 168;
const GAP = 22;
/** Cover plus two lines of type. */
const LABEL_HEIGHT = 46;

interface Props {
  albums: AlbumRow[];
  onOpen: (album: AlbumRow) => void;
}

/**
 * The library as covers. "Show art, hide chrome" means this is the front door,
 * so the grid gets the space and everything else gets out of the way.
 *
 * Virtualized by row: a collection of a few thousand albums must scroll at
 * 60fps, and only the visible rows exist in the DOM.
 */
export function AlbumsView({ albums, onOpen }: Props) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [columns, setColumns] = useState(4);
  const [tile, setTile] = useState(MIN_TILE);

  // Columns come from the width, so the grid reflows with the window rather
  // than locking to a breakpoint.
  useEffect(() => {
    const element = scrollRef.current;
    if (!element) return;
    const measure = () => {
      const width = element.clientWidth - GAP;
      const count = Math.max(2, Math.floor(width / (MIN_TILE + GAP)));
      setColumns(count);
      setTile(Math.floor((width - GAP * (count - 1)) / count));
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  const rows = Math.ceil(albums.length / columns);
  const rowHeight = tile + LABEL_HEIGHT + GAP;

  const virtualizer = useVirtualizer({
    count: rows,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => rowHeight,
    overscan: 3,
  });

  const cellsFor = useCallback(
    (rowIndex: number) => albums.slice(rowIndex * columns, rowIndex * columns + columns),
    [albums, columns],
  );

  return (
    <div ref={scrollRef} className="grid" tabIndex={0}>
      <div className="grid__canvas" style={{ height: virtualizer.getTotalSize() }}>
        {virtualizer.getVirtualItems().map((row) => (
          <div
            key={row.key}
            className="grid__row"
            style={{
              transform: `translateY(${row.start}px)`,
              gridTemplateColumns: `repeat(${columns}, ${tile}px)`,
              gap: GAP,
            }}
          >
            {cellsFor(row.index).map((album) => (
              <AlbumTile key={album.id} album={album} width={tile} onOpen={onOpen} />
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}

function AlbumTile({
  album,
  width,
  onOpen,
}: {
  album: AlbumRow;
  width: number;
  onOpen: (album: AlbumRow) => void;
}) {
  const badge = useMemo(() => albumBadge(album), [album]);
  return (
    <button
      className="tile"
      style={{ width }}
      onClick={() => onOpen(album)}
      title={`${album.title}${album.artist ? ` — ${album.artist}` : ""}`}
    >
      <Cover hash={album.artHash} size={300} alt={album.title} />
      <span className="tile__title">{album.title}</span>
      <span className="tile__meta">
        <span className="tile__artist">{album.artist ?? "Unknown artist"}</span>
        {badge && <span className="tile__badge">{badge}</span>}
      </span>
    </button>
  );
}

/**
 * One badge only when the whole album agrees. A mixed album gets nothing, which
 * is more honest than picking whichever format happened to sort first.
 */
function albumBadge(album: AlbumRow): string | null {
  if (!album.codec) return null;
  const codec = album.codec.toUpperCase();
  if (album.bitDepth && album.sampleRate) {
    return `${codec} ${album.bitDepth}/${formatKhz(album.sampleRate)}`;
  }
  if (album.sampleRate) return `${codec} ${formatKhz(album.sampleRate)}`;
  return codec;
}
