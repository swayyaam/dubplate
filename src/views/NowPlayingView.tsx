import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { TrackRow } from "../types";
import { Cover } from "../components/Cover";
import { Waveform } from "../components/Waveform";
import { formatDuration, formatKhz, trackArtist, trackTitle } from "../lib/format";
import { usePlayer } from "../lib/playerStore";
import { loadWaveform, peekWaveform } from "../lib/waveforms";

interface Props {
  trackById: Map<number, TrackRow>;
}

/**
 * Large art, the track, a waveform seek bar, and what the output is actually
 * doing. Everything else is chrome and stays out.
 */
export function NowPlayingView({ trackById }: Props) {
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
    <div className="playing">
      <Cover
        hash={track?.artHash ?? null}
        size={1000}
        alt={track ? trackTitle(track) : "Album art"}
        className="cover--hero cover--playing"
      />

      <div className="playing__text">
        <h1 className="playing__title">{track ? trackTitle(track) : "—"}</h1>
        <p className="playing__artist">{track ? trackArtist(track) : ""}</p>
        {track?.album && <p className="playing__album">{track.album}</p>}
      </div>

      <div className="playing__seek">
        <Waveform
          peaks={peaks}
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

      {/* A first sketch of the signal path readout. Phase 5 turns this into the
          full four-block panel with the device format read back rather than
          assumed. */}
      <dl className="signal">
        <div>
          <dt>Source</dt>
          <dd>
            {state.source
              ? `${state.source.codec.toUpperCase()} · ${formatKhz(state.source.sampleRate)} kHz · ${
                  state.source.bitsPerSample
                    ? `${state.source.bitsPerSample} bit`
                    : "no bit depth"
                } · ${state.source.channels} ch`
              : "—"}
          </dd>
        </div>
        <div>
          <dt>Output</dt>
          <dd>
            {state.device ?? "—"}
            {state.deviceSampleRate ? ` · ${formatKhz(state.deviceSampleRate)} kHz` : ""}
            {" · shared"}
          </dd>
        </div>
        <div>
          <dt>Dropouts</dt>
          <dd className={state.underruns > 0 ? "signal--warn" : undefined}>
            {state.underruns}
          </dd>
        </div>
      </dl>
    </div>
  );
}
