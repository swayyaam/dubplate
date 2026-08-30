import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { FlowStep, TrackRow } from "../types";
import { Cover } from "../components/Cover";
import { Waveform } from "../components/Waveform";
import { formatDuration, formatKhz, trackArtist, trackTitle } from "../lib/format";
import { usePlayer } from "../lib/playerStore";
import { loadWaveform, peekWaveform } from "../lib/waveforms";
import type { WaveformData } from "../lib/waveforms";

interface Props {
  trackById: Map<number, TrackRow>;
  /** The verdict badge is the way into the signal path, as the doc specifies. */
  onOpenSignal: () => void;
  onOpenQueue: () => void;
}

/**
 * Large art, the track, a waveform seek bar, and what the output is actually
 * doing. Everything else is chrome and stays out.
 */
export function NowPlayingView({ trackById, onOpenSignal, onOpenQueue }: Props) {
  const state = usePlayer();
  const trackId = state?.trackId ?? null;
  const [peaks, setPeaks] = useState<WaveformData | null>(null);
  const [scrub, setScrub] = useState<number | null>(null);
  const [building, setBuilding] = useState(false);
  const [palette, setPalette] = useState<string[]>([]);

  // Colours from the sleeve, for the backdrop. Separate from the single accent
  // the waveform uses: a wash needs more than one colour or it has no depth.
  const artHash = trackId !== null ? (trackById.get(trackId)?.artHash ?? null) : null;
  useEffect(() => {
    if (!artHash) {
      setPalette([]);
      return;
    }
    let live = true;
    void invoke<string[]>("accent_palette", { hash: artHash })
      .then((colours) => live && setPalette(colours))
      .catch(() => live && setPalette([]));
    return () => {
      live = false;
    };
  }, [artHash]);

  // Held as a style object so the aura is not rebuilt on every position tick.
  const aura = useMemo(
    () =>
      Object.fromEntries(
        palette.map((colour, index) => [`--aura-${index + 1}`, colour]),
      ) as React.CSSProperties,
    [palette],
  );

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

  if (!state || trackId === null) {
    return (
      <div className="empty">
        <p className="empty__body">Nothing playing. Pick something from the library.</p>
      </div>
    );
  }

  const track = trackById.get(trackId);
  const duration = state.durationMs || 1;
  const position = scrub !== null ? scrub * duration : state.positionMs;
  const progress = Math.min(1, position / duration);

  return (
    <div className={`playing${palette.length > 0 ? " playing--aura" : ""}`} style={aura}>
      {/* Three slow washes of colour taken from the sleeve, drifting at
          different speeds so they never quite repeat. Transform and opacity
          only, so the compositor animates them without repainting -- this
          screen is on-screen while audio is playing and must not compete with
          the callback for CPU. */}
      {palette.length > 0 && (
        <div className="aura" aria-hidden="true">
          <span className="aura__blob aura__blob--1" />
          <span className="aura__blob aura__blob--2" />
          <span className="aura__blob aura__blob--3" />
        </div>
      )}

      <div className="playing__scroll">
      <Cover
        hash={track?.artHash ?? null}
        size={1000}
        alt={track ? trackTitle(track) : "Album art"}
        className="cover--hero cover--playing"
      />

      <div className="playing__text glass">
        <h1 className="playing__title">{track ? trackTitle(track) : "—"}</h1>
        <p className="playing__artist">{track ? trackArtist(track) : ""}</p>
        {track?.album && <p className="playing__album">{track.album}</p>}
      </div>

      <div className="playing__seek glass">
        <Waveform
          data={peaks}
          progress={progress}
          height={72}
          onScrub={setScrub}
          onSeek={(fraction) =>
            void invoke("seek", { ms: Math.round(fraction * duration) })
          }
        />
        <div className="playing__times">
          <span>{formatDuration(position)}</span>
          <span>{formatDuration(state.durationMs)}</span>
        </div>
      </div>

      {/* Tempo, key and loudness were all measured by the analysis pass, so a
          set that mixes can be built out of them. */}
      <button
        className="button glass"
        disabled={building}
        onClick={() => {
          setBuilding(true);
          void invoke<FlowStep[]>("build_set", { trackId, length: 20 })
            .then((steps) => {
              if (steps.length < 2) return;
              return invoke("play_tracks", {
                trackIds: steps.map((step) => step.track.id),
                start: 0,
              }).then(onOpenQueue);
            })
            .finally(() => setBuilding(false));
        }}
        title="Queue tracks that mix with this one: close in tempo, one step around the Camelot wheel"
      >
        {building ? "Building…" : "Build a set from here"}
      </button>

      {/* The verdict, and the way into the full four-block readout. Green only
          when nothing altered the audio and the hardware really is at the
          file's rate -- read back from the device, not assumed. */}
      {state.signal && (
        <button
          className={`verdict verdict--button ${
            state.signal.bitPerfect ? "verdict--perfect" : "verdict--altered"
          }`}
          onClick={onOpenSignal}
          title="Show the full signal path"
        >
          <span className="verdict__badge">
            {state.signal.bitPerfect
              ? "bit-perfect"
              : `altered, ${state.signal.alteredStages} stage${
                  state.signal.alteredStages === 1 ? "" : "s"
                }`}
          </span>
          <span className="verdict__reason">
            {state.signal.deviceName ?? "—"}
            {state.signal.deviceFormat
              ? ` · ${formatKhz(state.signal.deviceFormat.sampleRate)} kHz · ${
                  state.signal.deviceFormat.sampleFormat
                }`
              : " · format unknown"}
            {state.signal.exclusive ? " · exclusive" : " · shared"}
          </span>
        </button>
      )}
    </div>
    </div>
  );
}
