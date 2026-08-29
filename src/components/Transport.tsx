import { memo, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { PlayerState, RepeatMode, TrackRow } from "../types";
import { formatDuration, formatKhz, trackArtist, trackTitle } from "../lib/format";
import { usePlayer } from "../lib/playerStore";
import { loadWaveform, peekWaveform } from "../lib/waveforms";
import { Cover } from "./Cover";
import { Waveform } from "./Waveform";

interface Props {
  trackById: Map<number, TrackRow>;
  onOpenNowPlaying: () => void;
  onOpenSignal: () => void;
}

/**
 * Always-visible transport. Reads position from the shared store, which polls
 * the audio callback's frame counter at 30fps -- never the decoder, which runs
 * ahead by whatever the ring holds.
 */
export const Transport = memo(function Transport({
  trackById,
  onOpenNowPlaying,
  onOpenSignal,
}: Props) {
  const state = usePlayer();
  const trackId = state?.trackId ?? null;
  const [peaks, setPeaks] = useState<number[] | null>(null);
  const [scrub, setScrub] = useState<number | null>(null);

  useEffect(() => {
    if (trackId === null) {
      setPeaks(null);
      return;
    }
    setPeaks(peekWaveform(trackId));
    let live = true;
    void loadWaveform(trackId).then((result) => {
      if (live) setPeaks(result);
    });
    return () => {
      live = false;
    };
  }, [trackId]);

  if (!state || trackId === null) return null;

  const track = trackById.get(trackId);
  const duration = state.durationMs || 1;
  const position = scrub !== null ? scrub * duration : state.positionMs;
  const progress = Math.min(1, position / duration);

  return (
    <div className="transport">
      <button className="transport__now" onClick={onOpenNowPlaying} title="Now playing">
        <Cover hash={track?.artHash ?? null} size={64} alt="" className="cover--chip" />
        <span className="transport__text">
          <span className="transport__title">{track ? trackTitle(track) : "—"}</span>
          <span className="transport__artist">{track ? trackArtist(track) : ""}</span>
        </span>
      </button>

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
        <div className="transport__wave">
          <Waveform
            peaks={peaks}
            progress={progress}
            height={30}
            onScrub={setScrub}
            onSeek={(fraction) =>
              void invoke("seek", { ms: Math.round(fraction * duration) })
            }
          />
        </div>
        <span className="transport__time">{formatDuration(state.durationMs)}</span>
      </div>

      <div className="transport__meta">
        {/* The doc's rule for an interface being unplugged: pause, and say so.
            Silently doing nothing is how a player looks broken. */}
        {state.error && (
          <span className="chip chip--error" title={state.error}>
            {state.error}
          </span>
        )}
        {state.underruns > 0 && !state.error && (
          <span
            className="chip chip--warn"
            title={`The decoder fell behind and the device ran dry ${state.underruns} times`}
          >
            {state.underruns} dropouts
          </span>
        )}
        {state.source && <SignalChip state={state} onOpen={onOpenSignal} />}
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
/**
 * Codec and rate, plus the verdict as a colour.
 *
 * Green means the hardware is genuinely running at the file's rate with nothing
 * in between; amber means something altered the audio. The device format behind
 * that judgement is read back from CoreAudio, not assumed from what we asked
 * for.
 */
function SignalChip({ state, onOpen }: { state: PlayerState; onOpen: () => void }) {
  const source = state.source!;
  const signal = state.signal;
  const spec = source.bitsPerSample
    ? `${source.bitsPerSample}/${formatKhz(source.sampleRate)}`
    : formatKhz(source.sampleRate);

  const detail = [
    `${source.codec.toUpperCase()} ${source.sampleRate} Hz`,
    source.bitsPerSample ? `${source.bitsPerSample} bit` : "no bit depth (lossy codec)",
    signal?.deviceName ?? state.device ?? "unknown device",
    signal?.deviceFormat
      ? `device at ${signal.deviceFormat.sampleRate} Hz ${signal.deviceFormat.sampleFormat}`
      : "device format unknown",
    signal?.exclusive ? "exclusive" : "shared",
    signal ? (signal.bitPerfect ? "bit-perfect" : (signal.reason ?? "altered")) : "",
  ]
    .filter(Boolean)
    .join(" · ");

  const tone = signal ? (signal.bitPerfect ? " chip--perfect" : " chip--converted") : "";

  return (
    <button className={`chip chip--button${tone}`} title={detail} onClick={onOpen}>
      {source.codec.toUpperCase()} <span className="chip__spec">{spec}</span>
    </button>
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
