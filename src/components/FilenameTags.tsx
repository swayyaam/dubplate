import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useEscape } from "../lib/sheet";
import type { NameFields, NamePreview, WriteReport } from "../types";

interface Props {
  onClose: () => void;
  onWritten: (report: WriteReport) => void;
}

/**
 * Fill in missing tags from filenames, in bulk, after showing exactly what
 * would happen.
 *
 * A quarter of a real DJ collection has no artist tag and its filename is the
 * only metadata it has. This is the one operation in the application that
 * rewrites hundreds of files at once, so nothing happens until the whole list
 * has been seen and rows have been deselected.
 */
export function FilenameTags({ onClose, onWritten }: Props) {
  const [rows, setRows] = useState<NamePreview[] | null>(null);
  const [chosen, setChosen] = useState<ReadonlySet<number>>(new Set());
  const [onlyMissing, setOnlyMissing] = useState(true);
  const [fields, setFields] = useState<NameFields>({
    title: true,
    artist: true,
    track: true,
    overwrite: false,
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setRows(null);
    invoke<NamePreview[]>("filename_preview", { ids: null, onlyMissing })
      .then((preview) => {
        if (!live) return;
        setRows(preview);
        // Pre-select only the rows that would actually change something.
        setChosen(new Set(preview.filter((row) => row.changes).map((row) => row.id)));
      })
      .catch((err) => live && setError(String(err)));
    return () => {
      live = false;
    };
  }, [onlyMissing]);

  const changing = useMemo(() => (rows ?? []).filter((row) => row.changes), [rows]);

  const toggle = useCallback((id: number) => {
    setChosen((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const apply = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const report = await invoke<WriteReport>("apply_filename_tags", {
        ids: [...chosen],
        fields,
      });
      if (report.failed > 0) {
        const first = report.outcomes.find((outcome) => outcome.error);
        setError(`${report.failed} failed — ${first?.error ?? "unknown"}`);
      }
      onWritten(report);
      if (report.failed === 0) onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }, [chosen, fields, onWritten, onClose]);

  useEscape(() => !busy && onClose());

  return (
    <div
      className="sheet"
      role="dialog"
      onClick={() => !busy && onClose()}
      aria-label="Tags from filenames"
    >
      <div className="sheet__panel sheet__panel--wide" onClick={(event) => event.stopPropagation()}>
        <header className="sheet__head">
          <h2 className="sheet__title">Tags from filenames</h2>
          <span className="sheet__subtitle">
            {rows === null
              ? "Reading…"
              : `${changing.length} of ${rows.length} would change · ${chosen.size} selected`}
          </span>
        </header>

        <div className="namebar">
          <button
            type="button"
            className={`chip${onlyMissing ? " chip--on" : ""}`}
            onClick={() => setOnlyMissing((value) => !value)}
          >
            Untagged only
          </button>
          <span className="namebar__gap" />
          {(["title", "artist", "track"] as const).map((key) => (
            <button
              key={key}
              type="button"
              className={`chip${fields[key] ? " chip--on" : ""}`}
              onClick={() => setFields((current) => ({ ...current, [key]: !current[key] }))}
            >
              {key === "track" ? "Track no." : key[0].toUpperCase() + key.slice(1)}
            </button>
          ))}
          <button
            type="button"
            className={`chip${fields.overwrite ? " chip--warn" : ""}`}
            onClick={() => setFields((current) => ({ ...current, overwrite: !current.overwrite }))}
            title="Replace values the files already have, rather than only filling in blanks"
          >
            Overwrite existing
          </button>
        </div>

        <div className="names">
          {(rows ?? []).map((row) => (
            <label key={row.id} className={`names__row${row.changes ? "" : " names__row--same"}`}>
              <input
                type="checkbox"
                checked={chosen.has(row.id)}
                disabled={!row.changes}
                onChange={() => toggle(row.id)}
              />
              <span className="names__file">{row.fileName}</span>
              <span className="names__arrow">→</span>
              <span className="names__guess">
                <span className="names__artist">{row.guess.artist ?? "—"}</span>
                <span className="names__title">{row.guess.title ?? "—"}</span>
                {row.guess.track !== null && <span className="names__track">#{row.guess.track}</span>}
                {(row.guess.bpm !== null || row.guess.key !== null) && (
                  <span
                    className="names__aside"
                    title="Found in the name but not written — the analyser measures both"
                  >
                    {row.guess.bpm !== null ? `${row.guess.bpm} BPM` : ""}
                    {row.guess.key !== null ? ` ${row.guess.key}` : ""}
                  </span>
                )}
              </span>
            </label>
          ))}
        </div>

        {error && <p className="sheet__error">{error}</p>}

        <footer className="sheet__foot">
          <span className="sheet__hint">
            Writes to {chosen.size} file{chosen.size === 1 ? "" : "s"}. There is no undo.
          </span>
          <div className="sheet__actions">
            <button type="button" className="chip" onClick={onClose} disabled={busy}>
              Cancel
            </button>
            <button
              type="button"
              className="chip chip--primary"
              onClick={apply}
              disabled={busy || chosen.size === 0}
            >
              {busy ? "Writing…" : `Apply to ${chosen.size}`}
            </button>
          </div>
        </footer>
      </div>
    </div>
  );
}
