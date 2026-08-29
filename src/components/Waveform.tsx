import { memo, useCallback, useEffect, useRef } from "react";

interface Props {
  /** True sample peaks, not normalised. Null while they are still being computed. */
  peaks: number[] | null;
  progress: number;
  height: number;
  onSeek: (fraction: number) => void;
  onScrub?: (fraction: number | null) => void;
}

/**
 * The seek bar, drawn from the track's own peaks.
 *
 * Canvas rather than a thousand DOM nodes: this redraws on every position tick,
 * and a thousand elements re-laid-out thirty times a second is exactly the kind
 * of thing that steals CPU from the audio callback.
 *
 * Peaks arrive unnormalised because several real files peak above full scale.
 * They are scaled to the track's own maximum here, for display only -- a quiet
 * track should still fill the bar rather than look broken.
 */
export const Waveform = memo(function Waveform({
  peaks,
  progress,
  height,
  onSeek,
  onScrub,
}: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const wrapRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    const wrap = wrapRef.current;
    if (!canvas || !wrap) return;

    const ratio = window.devicePixelRatio || 1;
    const width = wrap.clientWidth;
    if (width === 0) return;
    canvas.width = Math.floor(width * ratio);
    canvas.height = Math.floor(height * ratio);

    const context = canvas.getContext("2d");
    if (!context) return;
    context.setTransform(ratio, 0, 0, ratio, 0, 0);
    context.clearRect(0, 0, width, height);

    const styles = getComputedStyle(document.documentElement);
    const played = styles.getPropertyValue("--accent").trim() || "#e8a33d";
    const ahead = "rgba(255,255,255,0.16)";
    const middle = height / 2;
    const boundary = width * Math.min(1, Math.max(0, progress));

    if (!peaks || peaks.length === 0) {
      // No peaks yet: a plain bar, so the control is usable immediately rather
      // than absent until a decode finishes.
      const track = Math.max(2, Math.round(height * 0.12));
      context.fillStyle = ahead;
      context.fillRect(0, middle - track / 2, width, track);
      context.fillStyle = played;
      context.fillRect(0, middle - track / 2, boundary, track);
      return;
    }

    const ceiling = Math.max(...peaks, 0.0001);
    const barWidth = 2;
    const gap = 1;
    const columns = Math.max(1, Math.floor(width / (barWidth + gap)));

    for (let column = 0; column < columns; column += 1) {
      // Take the loudest peak covered by this column, so downsampling to the
      // screen keeps transients for the same reason the decode pass does.
      const from = Math.floor((column * peaks.length) / columns);
      const to = Math.max(from + 1, Math.floor(((column + 1) * peaks.length) / columns));
      let peak = 0;
      for (let index = from; index < to && index < peaks.length; index += 1) {
        if (peaks[index] > peak) peak = peaks[index];
      }

      const x = column * (barWidth + gap);
      const amplitude = Math.max(1, (peak / ceiling) * (height - 2));
      context.fillStyle = x + barWidth <= boundary ? played : ahead;
      context.fillRect(x, middle - amplitude / 2, barWidth, amplitude);
    }
  }, [peaks, progress, height]);

  const fractionAt = useCallback((clientX: number) => {
    const box = wrapRef.current?.getBoundingClientRect();
    if (!box || box.width === 0) return 0;
    return Math.min(1, Math.max(0, (clientX - box.left) / box.width));
  }, []);

  return (
    <div
      ref={wrapRef}
      className="waveform"
      style={{ height }}
      onPointerDown={(event) => {
        event.currentTarget.setPointerCapture(event.pointerId);
        onScrub?.(fractionAt(event.clientX));
      }}
      onPointerMove={(event) => {
        if (event.buttons === 1) onScrub?.(fractionAt(event.clientX));
      }}
      onPointerUp={(event) => {
        onSeek(fractionAt(event.clientX));
        onScrub?.(null);
      }}
    >
      <canvas ref={canvasRef} className="waveform__canvas" style={{ height }} />
    </div>
  );
});
