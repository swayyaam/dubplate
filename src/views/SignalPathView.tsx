import { invoke } from "@tauri-apps/api/core";
import type { OutputSettings, PlayerState, RateMode, Stage } from "../types";
import { formatKhz } from "../lib/format";
import { usePlayer } from "../lib/playerStore";

/**
 * What the file is, everything that could have touched it, and what the
 * hardware is actually running.
 *
 * The OUTPUT block is read back from the device rather than echoed from what we
 * asked for. Claiming bit-perfect because we asked politely is how players lie
 * without intending to, and this panel exists to not do that.
 */
export function SignalPathView({ onBack }: { onBack: () => void }) {
  const state = usePlayer();
  const signal = state?.signal ?? null;

  return (
    <div className="path">
      <button className="linkish" onClick={onBack}>
        ← Now playing
      </button>

      {!signal ? (
        <p className="empty__body">Nothing playing, so there is no signal path to show.</p>
      ) : (
        <>
          <Verdict signal={signal} />

          <Block title="Source" note="What is on disk, read from the stream and never from tags.">
            <Row label="Codec" value={signal.source.codec.toUpperCase()} />
            <Row label="Sample rate" value={`${signal.source.sampleRate} Hz`} />
            <Row
              label="Bit depth"
              value={
                signal.source.bitsPerSample
                  ? `${signal.source.bitsPerSample} bit`
                  : "none — lossy codecs have no bit depth"
              }
              muted={!signal.source.bitsPerSample}
            />
            <Row label="Channels" value={String(signal.source.channels)} />
          </Block>

          <Block title="Decoder" note="What came out of Symphonia.">
            <Row label="Sample rate" value={`${signal.decoderSampleRate} Hz`} />
            <Row label="Sample format" value={signal.decoderFormat} />
          </Block>

          <Block
            title="Processing"
            note="Every stage that could alter the samples. All four saying nothing is the point."
          >
            {signal.processing.map((stage) => (
              <StageRow key={stage.name} stage={stage} />
            ))}
          </Block>

          <Block title="Output" note="Read back from the device, not echoed from the request.">
            <Row label="Device" value={signal.deviceName ?? "—"} />
            {signal.deviceFormat ? (
              <>
                <Row label="Sample rate" value={`${signal.deviceFormat.sampleRate} Hz`} />
                <Row
                  label="Format"
                  value={`${signal.deviceFormat.sampleFormat} · ${signal.deviceFormat.bitsPerChannel} bit`}
                />
                <Row label="Channels" value={String(signal.deviceFormat.channels)} />
              </>
            ) : (
              <Row label="Format" value="the device would not report it" muted />
            )}
            <Row label="Access" value={signal.exclusive ? "exclusive" : "shared"} />
          </Block>
        </>
      )}

      {state && <Settings state={state} />}
    </div>
  );
}

function Verdict({ signal }: { signal: NonNullable<PlayerState["signal"]> }) {
  return (
    <div className={`verdict ${signal.bitPerfect ? "verdict--perfect" : "verdict--altered"}`}>
      <span className="verdict__badge">
        {signal.bitPerfect
          ? "bit-perfect"
          : `altered, ${signal.alteredStages} stage${signal.alteredStages === 1 ? "" : "s"}`}
      </span>
      <span className="verdict__reason">
        {signal.bitPerfect
          ? "Nothing altered the audio, and the device is running at the file's rate."
          : signal.reason}
      </span>
    </div>
  );
}

function Block({
  title,
  note,
  children,
}: {
  title: string;
  note: string;
  children: React.ReactNode;
}) {
  return (
    <section className="block">
      <h2 className="block__title">{title}</h2>
      <p className="block__note">{note}</p>
      <dl className="block__rows">{children}</dl>
    </section>
  );
}

function Row({ label, value, muted }: { label: string; value: string; muted?: boolean }) {
  return (
    <div className="block__row">
      <dt>{label}</dt>
      <dd className={muted ? "block__muted" : undefined}>{value}</dd>
    </div>
  );
}

function StageRow({ stage }: { stage: Stage }) {
  return (
    <div className="block__row">
      <dt>{stage.name}</dt>
      <dd className={stage.active ? "block__active" : "block__muted"}>
        {stage.active ? (stage.detail ?? "active") : "none"}
      </dd>
    </div>
  );
}

/** The choices that decide whether any of the above can say bit-perfect. */
function Settings({ state }: { state: PlayerState }) {
  const push = (next: OutputSettings) => {
    void invoke("set_output_settings", { settings: next }).catch(() => {});
  };
  const settings = state.settings;
  const mode = settings.rateMode.mode;

  return (
    <section className="block">
      <h2 className="block__title">Output settings</h2>
      <p className="block__note">
        Exclusive access is per device. An interface deserves it; laptop speakers
        never do. While it is held, every other application on this machine is
        silent.
      </p>

      <label className="toggle">
        <input
          type="checkbox"
          checked={settings.exclusive}
          onChange={(event) => push({ ...settings, exclusive: event.target.checked })}
        />
        <span>
          Take the device exclusively
          {settings.exclusive && state.signal && !state.signal.exclusive && (
            <em className="toggle__warn"> — requested, but the device refused</em>
          )}
        </span>
      </label>

      <label className="toggle">
        <input
          type="checkbox"
          checked={settings.replayGain}
          onChange={(event) => push({ ...settings, replayGain: event.target.checked })}
        />
        <span>Apply ReplayGain when a track carries it</span>
      </label>

      <div className="modes">
        {(
          [
            ["followFile", "Follow file", "Best fidelity. A rate change costs a small gap, and gapless cannot cross one."],
            ["followAlbum", "Follow album", "Switch only between albums, since an album shares one rate."],
            ["fixed", "Fixed rate", "Resample everything to one rate. Gapless always works, nothing is bit-perfect."],
          ] as const
        ).map(([value, label, note]) => (
          <label key={value} className={`mode${mode === value ? " mode--on" : ""}`}>
            <input
              type="radio"
              name="rate-mode"
              checked={mode === value}
              onChange={() =>
                push({
                  ...settings,
                  rateMode:
                    value === "fixed"
                      ? { mode: "fixed", rate: preferredRate(state) }
                      : ({ mode: value } as RateMode),
                })
              }
            />
            <span className="mode__label">{label}</span>
            <span className="mode__note">{note}</span>
          </label>
        ))}
      </div>

      {mode === "fixed" && state.deviceRates.length > 0 && (
        <div className="rates">
          {state.deviceRates.map((rate) => (
            <button
              key={rate}
              className={`rate${
                settings.rateMode.mode === "fixed" && settings.rateMode.rate === rate
                  ? " rate--on"
                  : ""
              }`}
              onClick={() => push({ ...settings, rateMode: { mode: "fixed", rate } })}
            >
              {formatKhz(rate)} kHz
            </button>
          ))}
        </div>
      )}

      <button className="button" onClick={() => void invoke("reopen_output")}>
        Reopen output device
      </button>
    </section>
  );
}

/** A sensible fixed rate: whatever the device is already running. */
function preferredRate(state: PlayerState): number {
  return (
    state.signal?.deviceFormat?.sampleRate ??
    state.deviceSampleRate ??
    state.deviceRates[0] ??
    48000
  );
}
