import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AnalysisStatus, BatchReport, Bucket, CollectionHealth, TrackRow } from "../types";
import { formatBytes, formatKhz, formatTotalTime, trackArtist, trackTitle } from "../lib/format";

type Filter = "suspected" | "padded" | "lossless" | "lossy" | "unknown" | "unanalysed";

const FILTER_LABELS: Record<Filter, string> = {
  suspected: "Suspected transcodes",
  padded: "Padded containers",
  lossless: "Lossless",
  lossy: "Lossy",
  unknown: "Unresolved codec",
  unanalysed: "Not yet analysed",
};

/**
 * What the collection actually is.
 *
 * For a library assembled from many sources over years this is a more
 * interesting first screen than a genre list. Everything here filters and
 * counts; nothing hides or deletes.
 */
export function HealthView({ onPlay }: { onPlay: (tracks: TrackRow[], index: number) => void }) {
  const [health, setHealth] = useState<CollectionHealth | null>(null);
  const [status, setStatus] = useState<AnalysisStatus | null>(null);
  const [filter, setFilter] = useState<Filter | null>(null);
  const [rows, setRows] = useState<TrackRow[]>([]);

  const refresh = useCallback(async () => {
    setHealth(await invoke<CollectionHealth>("collection_health").catch(() => null));
    setStatus(await invoke<AnalysisStatus>("analysis_status").catch(() => null));
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // The pass reports after every batch, so the numbers fill in as it works.
  useEffect(() => {
    const progress = listen<BatchReport>("analysis:progress", (event) => {
      setStatus((current) =>
        current ? { ...current, remaining: event.payload.remaining, running: true } : current,
      );
    });
    const done = listen("analysis:done", () => void refresh());
    return () => {
      void progress.then((un) => un());
      void done.then((un) => un());
    };
  }, [refresh]);

  useEffect(() => {
    if (!filter) {
      setRows([]);
      return;
    }
    let live = true;
    void invoke<TrackRow[]>("health_tracks", { filter, limit: 500 })
      .then((result) => live && setRows(result))
      .catch(() => live && setRows([]));
    return () => {
      live = false;
    };
  }, [filter, health]);

  const analysed = health ? health.analysed / Math.max(1, health.total) : 0;

  return (
    <div className="health">
      {health && (
        <header className="health__head">
          <div>
            <h1 className="health__title">{health.total.toLocaleString()} tracks</h1>
            <p className="health__sub">
              {formatTotalTime(health.totalDurationMs)} · {formatBytes(health.totalBytes)}
            </p>
          </div>
          <AnalysisControl status={status} analysed={analysed} onRefresh={refresh} />
        </header>
      )}

      {health && (
        <div className="cards">
          <Card
            label="Lossless"
            value={`${Math.round((health.lossless / Math.max(1, health.total)) * 100)}%`}
            note={`${health.lossless.toLocaleString()} of ${health.total.toLocaleString()} files`}
            onClick={() => setFilter("lossless")}
            active={filter === "lossless"}
          />
          <Card
            label="Padded containers"
            value={health.padded.toLocaleString()}
            note="More bits declared than the audio uses"
            onClick={() => setFilter("padded")}
            active={filter === "padded"}
            tone={health.padded > 0 ? "warn" : undefined}
          />
          <Card
            label="Suspected transcodes"
            value={health.suspected.toLocaleString()}
            note="Lossless containers with a lossy-looking spectrum"
            onClick={() => setFilter("suspected")}
            active={filter === "suspected"}
            tone={health.suspected > 0 ? "warn" : undefined}
          />
          <Card
            label="Not yet analysed"
            value={(health.total - health.analysed).toLocaleString()}
            note="Nothing is known about these yet"
            onClick={() => setFilter("unanalysed")}
            active={filter === "unanalysed"}
          />
        </div>
      )}

      {health && (
        <div className="dists">
          <Distribution title="Format" buckets={health.codecs} total={health.total} />
          <Distribution
            title="Sample rate"
            buckets={health.sampleRates}
            total={health.total}
            format={(label) => (label === "unknown" ? label : `${formatKhz(Number(label))} kHz`)}
          />
          <Distribution
            title="Bit depth"
            buckets={health.bitDepths}
            total={health.total}
            // "none" is the honest answer for a lossy codec, not a gap.
            format={(label) => (label === "none" ? "none (lossy)" : `${label} bit`)}
          />
        </div>
      )}

      {filter && (
        <section className="health__list">
          <h2 className="block__title">
            {FILTER_LABELS[filter]} — {rows.length}
            {rows.length === 500 ? "+" : ""}
          </h2>
          {filter === "suspected" && (
            <p className="block__note">
              A suspicion, not a verdict. The cutoff and score that produced it are
              shown so you can judge: quiet recordings, older masters and genuinely
              dark mixes all have low high-frequency content without ever having been
              near an encoder.
            </p>
          )}
          <ol className="evidence">
            {rows.map((track, index) => (
              <li key={track.id}>
                <button className="evidence__row" onDoubleClick={() => onPlay(rows, index)}>
                  <span className="evidence__title">{trackTitle(track)}</span>
                  <span className="evidence__by">{trackArtist(track)}</span>
                  <span className="evidence__fact">{evidence(track, filter)}</span>
                </button>
              </li>
            ))}
            {rows.length === 0 && <li className="block__note">Nothing here. Good.</li>}
          </ol>
        </section>
      )}
    </div>
  );
}

/** The number that produced the judgement, never just the judgement. */
function evidence(track: TrackRow, filter: Filter): string {
  if (filter === "suspected") {
    const score = track.transcodeScore ?? 0;
    const cutoff = track.spectralCutoff;
    return `${score.toFixed(2)} · cutoff ${cutoff ? `${(cutoff / 1000).toFixed(1)} kHz` : "—"}`;
  }
  if (filter === "padded") {
    return `${track.bitDepth ?? "—"} bit container · ${track.effectiveBits ?? "—"} bit content`;
  }
  const parts = [track.codec.toUpperCase()];
  if (track.sampleRate) parts.push(`${formatKhz(track.sampleRate)} kHz`);
  if (track.bitDepth) parts.push(`${track.bitDepth} bit`);
  return parts.join(" · ");
}

function AnalysisControl({
  status,
  analysed,
  onRefresh,
}: {
  status: AnalysisStatus | null;
  analysed: number;
  onRefresh: () => void;
}) {
  if (!status) return null;
  const done = status.remaining === 0;

  return (
    <div className="analysis">
      {done ? (
        <span className="analysis__note">Every track analysed.</span>
      ) : (
        <>
          <div className="analysis__bar">
            <div className="analysis__fill" style={{ width: `${analysed * 100}%` }} />
          </div>
          <span className="analysis__note">
            {status.remaining.toLocaleString()} left
            {status.running ? " · running" : ""}
          </span>
        </>
      )}
      {!status.running && !done && (
        <button
          className="button button--primary"
          onClick={() => {
            void invoke("start_analysis");
            // The pass reports as it goes, but reflect the start immediately.
            window.setTimeout(onRefresh, 300);
          }}
        >
          Analyse library
        </button>
      )}
    </div>
  );
}

function Card({
  label,
  value,
  note,
  onClick,
  active,
  tone,
}: {
  label: string;
  value: string;
  note: string;
  onClick: () => void;
  active: boolean;
  tone?: "warn";
}) {
  return (
    <button
      className={`card${active ? " card--on" : ""}${tone === "warn" ? " card--warn" : ""}`}
      onClick={onClick}
    >
      <span className="card__value">{value}</span>
      <span className="card__label">{label}</span>
      <span className="card__note">{note}</span>
    </button>
  );
}

function Distribution({
  title,
  buckets,
  total,
  format,
}: {
  title: string;
  buckets: Bucket[];
  total: number;
  format?: (label: string) => string;
}) {
  const shown = useMemo(() => buckets.slice(0, 7), [buckets]);
  return (
    <section className="dist">
      <h2 className="block__title">{title}</h2>
      {shown.map((bucket) => (
        <div key={bucket.label} className="dist__row">
          <span className="dist__label">
            {format ? format(bucket.label) : bucket.label.toUpperCase()}
          </span>
          <span className="dist__bar">
            <span
              className="dist__fill"
              style={{ width: `${(bucket.count / Math.max(1, total)) * 100}%` }}
            />
          </span>
          <span className="dist__count">{bucket.count.toLocaleString()}</span>
        </div>
      ))}
    </section>
  );
}
