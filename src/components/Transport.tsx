import { memo, useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { PlayerState, RepeatMode, TrackRow } from "../types";
import { formatDuration, trackArtist, trackTitle } from "../lib/format";

/**
 * Position is polled rather than pushed, at 30fps, straight from the audio
 * callback's frame counter. Never from the decoder: it runs ahead by whatever
 * the ring buffer holds, so asking it where playback is would be wrong by about
 * 150ms.
 *
 * This component is deliberately the only thing that re-renders on a tick. The
 * track list must not: it holds ten thousand rows.
 */
const POLL_MS = 1000 / 30;

interface Props {
  /** Looked up so the bar can name the track without another round trip. */
  trackById: Map<number, TrackRow>;
  onNowPlayingChange: (trackId: number | null, playing: boolean) => void;
}

export const Transport = memo(function Transport({ trackById, onNowPlayingChange }: Props) {
  const [state, setState] = useState<PlayerState | null>(null);
  const [scrubMs, setScrubMs] = useState<number | null>(null);
  const lastNowPlaying = useRef<string>("");

  useEffect(() => {
    let live = true;
    let timer: number;
    const tick = async () => {
      try {
        const next = await invoke<PlayerState>("player_state");
        if (!live) return;
        setState(next);
        // Only wake the rest of the app when the answer actually changed.
        const key = `${next.trackId}:${next.playing}`;
        if (key !== lastNowPlaying.current) {
          lastNowPlaying.current = key;
          onNowPlayingChange(next.trackId, next.playing);
        }
      } catch {
        // The engine is not up yet; the next tick will find it.
      }
      if (live) timer = window.setTimeout(tick, POLL_MS);
    };
    void tick();
    return () => {
      live = false;
      window.clearTimeout(timer);
    };
  }, [onNowPlayingChange]);

  const seekTo = useCallback((ms: number) => {
    void invoke("seek", { ms: Math.max(0, Math.round(ms)) });
  }, []);

  if (!state || state.trackId === null) return null;

  const track = trackById.get(state.trackId);
  const position = scrubMs ?? state.positionMs;
  const duration = state.durationMs || 1;
  const progress = Math.min(1, position / duration);

  return (
    <div className="transport">
      <div className="transport__now">
        <span className="transport__title">{track ? trackTitle(track) : "—"}</span>
        <span className="transport__artist">{track ? trackArtist(track) : ""}</span>
      </div>

      <div className="transport__controls">
        <button className="icon" title="Previous" onClick={() => void invoke("previous_track")}>
          <Glyph path={GLYPH.previous} filled />
        </button>
        <button
          className="icon icon--play"
          title={state.playing ? "Pause" : "Play"}
          onClick={() => void invoke("toggle_play")}
        >
          <Glyph path={state.playing ? GLYPH.pause : GLYPH.play} filled={!state.playing} />
        </button>
        <button className="icon" title="Next" onClick={() => void invoke("next_track")}>
          <Glyph path={GLYPH.next} filled />
        </button>
      </div>

      <div className="transport__seek">
        <span className="transport__time">{formatDuration(position)}</span>
        <Scrubber
          progress={progress}
          onScrub={(fraction) => setScrubMs(fraction * duration)}
          onCommit={(fraction) => {
            seekTo(fraction * duration);
            setScrubMs(null);
          }}
        />
        <span className="transport__time">{formatDuration(state.durationMs)}</span>
      </div>

      <div className="transport__meta">
        {state.source && <SignalChip state={state} />}
        <button
          className={`icon${state.shuffle ? " icon--on" : ""}`}
          title="Shuffle"
          onClick={() => void invoke("set_shuffle", { shuffle: !state.shuffle })}
        >
          <Glyph path={GLYPH.shuffle} />
        </button>
        <button
          className={`icon${state.repeat !== "off" ? " icon--on" : ""}`}
          title={`Repeat: ${state.repeat}`}
          onClick={() => void invoke("set_repeat", { mode: nextRepeat(state.repeat) })}
        >
          <Glyph path={GLYPH.repeat} />
          {state.repeat === "one" && <span className="icon__badge">1</span>}
        </button>
        <input
          className="volume"
          type="range"
          min={0}
          max={1}
          step={0.01}
          value={state.volume}
          title={`Volume ${Math.round(state.volume * 100)}%`}
          onChange={(event) => void invoke("set_volume", { volume: Number(event.target.value) })}
        />
      </div>
    </div>
  );
});

/**
 * Codec, rate and what the device is actually running at. A first sketch of the
 * signal path readout: when the device rate differs from the file's, something
 * resampled, and phase 5 will say so properly.
 */
function SignalChip({ state }: { state: PlayerState }) {
  const source = state.source!;
  const converted =
    state.deviceSampleRate !== null && state.deviceSampleRate !== source.sampleRate;
  const spec = source.bitsPerSample
    ? `${source.bitsPerSample}/${source.sampleRate / 1000}`
    : `${source.sampleRate / 1000}`;
  const detail = [
    `${source.codec.toUpperCase()} ${source.sampleRate} Hz`,
    source.bitsPerSample ? `${source.bitsPerSample} bit` : "no bit depth (lossy codec)",
    state.device ?? "unknown device",
    state.deviceSampleRate ? `device at ${state.deviceSampleRate} Hz` : "",
    state.underruns > 0 ? `${state.underruns} underruns` : "",
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <span className={`chip${converted ? " chip--converted" : ""}`} title={detail}>
      {source.codec.toUpperCase()} <span className="chip__spec">{spec}</span>
    </span>
  );
}

function Scrubber({
  progress,
  onScrub,
  onCommit,
}: {
  progress: number;
  onScrub: (fraction: number) => void;
  onCommit: (fraction: number) => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const fractionAt = (clientX: number) => {
    const box = ref.current?.getBoundingClientRect();
    if (!box || box.width === 0) return 0;
    return Math.min(1, Math.max(0, (clientX - box.left) / box.width));
  };

  return (
    <div
      ref={ref}
      className="scrubber"
      onPointerDown={(event) => {
        event.currentTarget.setPointerCapture(event.pointerId);
        onScrub(fractionAt(event.clientX));
      }}
      onPointerMove={(event) => {
        if (event.buttons === 1) onScrub(fractionAt(event.clientX));
      }}
      onPointerUp={(event) => onCommit(fractionAt(event.clientX))}
    >
      <div className="scrubber__fill" style={{ width: `${progress * 100}%` }} />
      <div className="scrubber__head" style={{ left: `${progress * 100}%` }} />
    </div>
  );
}

/**
 * Inline SVG rather than emoji or a font. Emoji render in full colour on macOS,
 * which fights a one-accent palette, and their metrics differ per glyph so a row
 * of them never lines up.
 */
function Glyph({ path, filled = false }: { path: string; filled?: boolean }) {
  return (
    <svg viewBox="0 0 24 24" width="15" height="15" aria-hidden="true">
      <path
        d={path}
        fill={filled ? "currentColor" : "none"}
        stroke={filled ? "none" : "currentColor"}
        strokeWidth="1.9"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

const GLYPH = {
  previous: "M18 5v14L8 12zM6 5v14",
  next: "M6 5v14l10-7zM18 5v14",
  play: "M8 5.5v13l11-6.5z",
  pause: "M9 5v14M15 5v14",
  shuffle: "M16 4h4v4M20 4l-6 6M4 20l16-16M16 20h4v-4M20 20l-6-6M4 4l4 4",
  repeat: "M4 10V8a3 3 0 0 1 3-3h10l-3-3m3 12v2a3 3 0 0 1-3 3H4l3 3",
} as const;

function nextRepeat(mode: RepeatMode): RepeatMode {
  return mode === "off" ? "all" : mode === "all" ? "one" : "off";
}
