import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useEscape } from "../lib/sheet";
import type { FieldValue, TagField, TrackRow, WriteReport } from "../types";

const LABELS: Record<TagField, string> = {
  title: "Title",
  artist: "Artist",
  album: "Album",
  albumArtist: "Album artist",
  trackNumber: "Track",
  trackTotal: "of",
  discNumber: "Disc",
  discTotal: "of",
  year: "Year",
  genre: "Genre",
  composer: "Composer",
  comment: "Comment",
};

/** Fields that share a row, because they are two halves of one number. */
const PAIRS: [TagField, TagField][] = [
  ["trackNumber", "trackTotal"],
  ["discNumber", "discTotal"],
];

const NUMERIC: TagField[] = ["trackNumber", "trackTotal", "discNumber", "discTotal", "year"];

interface Props {
  tracks: TrackRow[];
  onClose: () => void;
  /** Fired after a successful write so the caller can refresh the library. */
  onWritten: (report: WriteReport) => void;
}

/**
 * Edits the tags in the files themselves, for one track or for many.
 *
 * The multi-track rule is the whole reason this exists as a separate component:
 * a field the selection disagrees on shows "multiple" and is *not written* --
 * only fields actually typed into are sent. Loading twelve tracks and saving
 * would otherwise stamp one title across all of them.
 */
export function TagEditor({ tracks, onClose, onWritten }: Props) {
  const ids = useMemo(() => tracks.map((track) => track.id), [tracks]);
  const [loaded, setLoaded] = useState<FieldValue[] | null>(null);
  const [edits, setEdits] = useState<Partial<Record<TagField, string>>>({});
  const [artwork, setArtwork] = useState<"keep" | "remove" | string>("keep");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setLoaded(null);
    setEdits({});
    setArtwork("keep");
    setError(null);
    invoke<FieldValue[]>("track_tags", { ids })
      .then((values) => {
        if (live) setLoaded(values);
      })
      .catch((err) => live && setError(String(err)));
    return () => {
      live = false;
    };
  }, [ids]);

  const byField = useMemo(() => {
    const map = new Map<TagField, FieldValue>();
    for (const value of loaded ?? []) map.set(value.field, value);
    return map;
  }, [loaded]);

  const dirty = Object.keys(edits).length > 0 || artwork !== "keep";

  const save = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const fields = Object.entries(edits).map(([field, value]) => ({
        field: field as TagField,
        // An emptied box clears the tag; a box never touched is not in here at
        // all, which is what protects the fields showing "multiple".
        value: value.trim() === "" ? null : value,
      }));
      const report = await invoke<WriteReport>("write_track_tags", {
        ids,
        edit: {
          fields,
          artwork:
            artwork === "keep"
              ? null
              : artwork === "remove"
                ? { kind: "remove" }
                : { kind: "set", data: { path: artwork } },
        },
      });
      if (report.failed > 0) {
        const first = report.outcomes.find((outcome) => outcome.error);
        setError(
          `${report.failed} of ${report.failed + report.written} failed — ${first?.error ?? "unknown"}`,
        );
        if (report.written === 0) return;
      }
      onWritten(report);
      if (report.failed === 0) onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }, [edits, artwork, ids, onWritten, onClose]);

  const pickImage = useCallback(async () => {
    const chosen = await open({
      multiple: false,
      filters: [{ name: "Image", extensions: ["jpg", "jpeg", "png", "webp", "gif"] }],
    });
    if (typeof chosen === "string") setArtwork(chosen);
  }, []);

  useEscape(() => !busy && onClose());

  const single = tracks.length === 1;

  return (
    <div
      className="sheet"
      role="dialog"
      onClick={() => !busy && onClose()}
      aria-label="Edit tags"
    >
      <div className="sheet__panel" onClick={(event) => event.stopPropagation()}>
        <header className="sheet__head">
          <h2 className="sheet__title">
            {single ? "Edit tags" : `Edit ${tracks.length} tracks`}
          </h2>
          <span className="sheet__subtitle">
            {single ? tracks[0].fileName : "Only fields you change are written"}
          </span>
        </header>

        {loaded === null ? (
          <p className="sheet__note">Reading tags…</p>
        ) : (
          <div className="fields">
            {(["title", "artist", "album", "albumArtist"] as TagField[]).map((field) => (
              <Row key={field} field={field} value={byField.get(field)} edits={edits} setEdits={setEdits} />
            ))}
            {PAIRS.map(([a, b]) => (
              <div className="field field--pair" key={a}>
                <label className="field__label">{LABELS[a]}</label>
                <Input field={a} value={byField.get(a)} edits={edits} setEdits={setEdits} />
                <span className="field__of">of</span>
                <Input field={b} value={byField.get(b)} edits={edits} setEdits={setEdits} />
              </div>
            ))}
            {(["year", "genre", "composer", "comment"] as TagField[]).map((field) => (
              <Row key={field} field={field} value={byField.get(field)} edits={edits} setEdits={setEdits} />
            ))}

            <div className="field">
              <label className="field__label">Artwork</label>
              <div className="field__artwork">
                <button type="button" className="chip" onClick={pickImage}>
                  {artwork !== "keep" && artwork !== "remove" ? "Image chosen" : "Choose image…"}
                </button>
                <button
                  type="button"
                  className={`chip${artwork === "remove" ? " chip--on" : ""}`}
                  onClick={() => setArtwork(artwork === "remove" ? "keep" : "remove")}
                >
                  Remove
                </button>
                {artwork !== "keep" && (
                  <button type="button" className="chip chip--quiet" onClick={() => setArtwork("keep")}>
                    Cancel
                  </button>
                )}
              </div>
            </div>
          </div>
        )}

        {error && <p className="sheet__error">{error}</p>}

        <footer className="sheet__foot">
          <span className="sheet__hint">
            {dirty
              ? `Writes to ${tracks.length} file${tracks.length === 1 ? "" : "s"}. There is no undo.`
              : "Nothing changed yet."}
          </span>
          <div className="sheet__actions">
            <button type="button" className="chip" onClick={onClose} disabled={busy}>
              Cancel
            </button>
            <button
              type="button"
              className="chip chip--primary"
              onClick={save}
              disabled={busy || !dirty}
            >
              {busy ? "Writing…" : "Save"}
            </button>
          </div>
        </footer>
      </div>
    </div>
  );
}

interface RowInputProps {
  field: TagField;
  value: FieldValue | undefined;
  edits: Partial<Record<TagField, string>>;
  setEdits: React.Dispatch<React.SetStateAction<Partial<Record<TagField, string>>>>;
}

function Row(props: RowInputProps) {
  return (
    <div className="field">
      <label className="field__label">{LABELS[props.field]}</label>
      <Input {...props} />
    </div>
  );
}

function Input({ field, value, edits, setEdits }: RowInputProps) {
  const edited = edits[field];
  const varies = value?.varies ?? false;
  return (
    <input
      className={`field__input${varies && edited === undefined ? " field__input--varies" : ""}`}
      type="text"
      inputMode={NUMERIC.includes(field) ? "numeric" : undefined}
      // An untouched field shows what is in the files; once typed into, it
      // shows what will be written.
      value={edited ?? (varies ? "" : (value?.value ?? ""))}
      placeholder={varies ? "⟨multiple⟩" : ""}
      onChange={(event) => setEdits((current) => ({ ...current, [field]: event.target.value }))}
    />
  );
}
