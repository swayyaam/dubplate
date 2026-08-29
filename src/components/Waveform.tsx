import { memo, useCallback, useEffect, useRef } from "react";
import type { WaveformData } from "../lib/waveforms";
import { FLOOR_DB } from "../lib/waveforms";

interface Props {
  /** Five lanes of bytes. Null while it is still being computed. */
  data: WaveformData | null;
  progress: number;
  height: number;
  onSeek: (fraction: number) => void;
  onScrub?: (fraction: number | null) => void;
}

/**
 * Where the drawing stops, in decibels below full scale.
 *
 * Narrower than the stored range on purpose: 60dB of headroom is the right
 * thing to keep on disk, but drawing all of it gives a brickwalled master a
 * permanent floor of visible fuzz. 48dB puts the quietest thing worth seeing
 * at one pixel.
 */
const DISPLAY_FLOOR_DB = 48;

/** How much of its colour the not-yet-played part of the bar keeps. */
const AHEAD_DIM = 0.45;

/**
 * The seek bar, drawn from the track's own analysis.
 *
 * Canvas rather than a thousand DOM nodes: this redraws on every position tick,
 * and a thousand elements re-laid-out thirty times a second is exactly the kind
 * of thing that steals CPU from the audio callback.
 *
 * Three things are drawn at once, and each answers a different question. The
 * filled body is RMS -- what the arrangement is doing. The faint outline is
 * peak -- what the limiter is doing, which on a modern master is a flat line
 * near the top and is precisely why it cannot be the only thing shown. The
 * colour is the low/mid/high mix, so a breakdown reads differently from a drop
 * rather than merely being slightly shorter.
 */
export const Waveform = memo(function Waveform({
  data,
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
    const accent = styles.getPropertyValue("--accent").trim() || "#e8a33d";
    const middle = height / 2;
    const boundary = width * Math.min(1, Math.max(0, progress));

    if (!data || data.buckets === 0) {
      // No analysis yet: a plain bar, so the control is usable immediately
      // rather than absent until a decode finishes.
      const track = Math.max(2, Math.round(height * 0.12));
      context.fillStyle = "rgba(255,255,255,0.16)";
      context.fillRect(0, middle - track / 2, width, track);
      context.fillStyle = accent;
      context.fillRect(0, middle - track / 2, boundary, track);
      return;
    }

    const palette = bandPalette(accent);
    const barWidth = 2;
    const gap = 1;
    const columns = Math.max(1, Math.floor(width / (barWidth + gap)));
    const usable = height - 2;

    for (let column = 0; column < columns; column += 1) {
      const from = Math.floor((column * data.buckets) / columns);
      const to = Math.max(from + 1, Math.floor(((column + 1) * data.buckets) / columns));

      // Peak collapses with a maximum -- a transient that survives all the way
      // to the screen is what makes a waveform recognisable. RMS collapses with
      // a mean, because it is already an average and taking maxima of averages
      // just adds a comb of noise to a smooth envelope.
      let peak = 0;
      let rmsTotal = 0;
      let rmsCount = 0;
      // Colour is weighted by level, so one loud bucket in a column decides its
      // colour rather than being averaged away by the silence either side.
      let low = 0;
      let mid = 0;
      let high = 0;
      let weight = 0;
      for (let index = from; index < to && index < data.buckets; index += 1) {
        const body = data.rms[index];
        if (data.peak[index] > peak) peak = data.peak[index];
        rmsTotal += body;
        rmsCount += 1;
        const w = body + 1;
        low += data.low[index] * w;
        mid += data.mid[index] * w;
        high += data.high[index] * w;
        weight += w;
      }
      if (weight > 0) {
        low /= weight;
        mid /= weight;
        high /= weight;
      }
      const rms = rmsCount > 0 ? rmsTotal / rmsCount : 0;

      const x = column * (barWidth + gap);
      const played = x + barWidth <= boundary;
      const colour = blend(palette, low, mid, high, played ? 1 : AHEAD_DIM);

      const peakHeight = Math.max(1, level(peak) * usable);
      const rmsHeight = Math.max(1, level(rms) * usable);

      // Outline first, body over it, so the peak reads as a halo around the
      // RMS rather than a separate shape competing with it.
      context.fillStyle = colour(played ? 0.22 : 0.12);
      context.fillRect(x, middle - peakHeight / 2, barWidth, peakHeight);
      context.fillStyle = colour(1);
      context.fillRect(x, middle - rmsHeight / 2, barWidth, rmsHeight);
    }
  }, [data, progress, height]);

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

/**
 * A stored byte to a fraction of the bar's height.
 *
 * The byte is linear in decibels, so this only has to move the floor: anything
 * below the display floor collapses to nothing rather than drawing a permanent
 * band of noise along the middle.
 */
function level(byte: number): number {
  if (byte === 0) return 0;
  const db = (byte / 255) * FLOOR_DB - FLOOR_DB;
  return Math.max(0, Math.min(1, (db + DISPLAY_FLOOR_DB) / DISPLAY_FLOOR_DB));
}

type Rgb = [number, number, number];

/**
 * Three tones for the three bands, derived from the artwork accent rather than
 * fixed.
 *
 * A red/green/blue split would read instantly and look like a test card in a
 * player whose whole palette is pulled from the cover. Staying on the accent's
 * hue and separating the bands by depth and paleness keeps the picture: bass is
 * the deep, saturated end, air is the pale end, and the mids are the accent
 * itself.
 */
function bandPalette(accent: string): [Rgb, Rgb, Rgb] {
  const [h, s, l] = toHsl(parseHex(accent) ?? [232, 163, 61]);
  return [
    toRgb(h - 12, Math.min(1, s * 1.15), l * 0.45),
    toRgb(h, s, l),
    toRgb(h + 20, s * 0.3, Math.min(0.96, l * 1.62)),
  ];
}

/** Mix the three band tones by a column's energy split. */
function blend(
  palette: [Rgb, Rgb, Rgb],
  low: number,
  mid: number,
  high: number,
  brightness: number,
): (alpha: number) => string {
  const total = low + mid + high || 1;
  const channel = (index: number) =>
    Math.round(
      ((palette[0][index] * low + palette[1][index] * mid + palette[2][index] * high) / total) *
        brightness,
    );
  const r = channel(0);
  const g = channel(1);
  const b = channel(2);
  return (alpha: number) => `rgba(${r},${g},${b},${alpha})`;
}

function parseHex(value: string): Rgb | null {
  const match = /^#?([0-9a-f]{6})$/i.exec(value.trim());
  if (!match) return null;
  const n = parseInt(match[1], 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

function toHsl([r, g, b]: Rgb): [number, number, number] {
  const rn = r / 255;
  const gn = g / 255;
  const bn = b / 255;
  const max = Math.max(rn, gn, bn);
  const min = Math.min(rn, gn, bn);
  const l = (max + min) / 2;
  const d = max - min;
  if (d === 0) return [0, 0, l];
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h: number;
  if (max === rn) h = ((gn - bn) / d + (gn < bn ? 6 : 0)) * 60;
  else if (max === gn) h = ((bn - rn) / d + 2) * 60;
  else h = ((rn - gn) / d + 4) * 60;
  return [h, s, l];
}

function toRgb(h: number, s: number, l: number): Rgb {
  const hue = ((h % 360) + 360) % 360;
  const c = (1 - Math.abs(2 * l - 1)) * s;
  const x = c * (1 - Math.abs(((hue / 60) % 2) - 1));
  const m = l - c / 2;
  const [r, g, b] =
    hue < 60
      ? [c, x, 0]
      : hue < 120
        ? [x, c, 0]
        : hue < 180
          ? [0, c, x]
          : hue < 240
            ? [0, x, c]
            : hue < 300
              ? [x, 0, c]
              : [c, 0, x];
  return [
    Math.round((r + m) * 255),
    Math.round((g + m) * 255),
    Math.round((b + m) * 255),
  ];
}
