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

/**
 * How strongly the not-yet-played part of the bar is drawn.
 *
 * Faded rather than darkened. Scaling the colour towards black turns a pale
 * gold into olive, so the part of the track you have not reached ends up a
 * different hue from the part you have.
 */
const AHEAD_FADE = 0.46;

/**
 * The seek bar, drawn from the track's own analysis.
 *
 * Canvas rather than a thousand DOM nodes: this redraws on every position tick,
 * and a thousand elements re-laid-out thirty times a second is exactly the kind
 * of thing that steals CPU from the audio callback.
 *
 * Drawn as one continuous shape rather than a row of bars: the envelope curves
 * through the midpoints between samples, at one sample per pixel, so it reads
 * as a waveform and not as a barcode.
 *
 * Three things are shown at once. The solid body is RMS -- what the
 * arrangement is doing. The diffuse aura around it is peak -- what the limiter
 * is doing, which on a modern master is pinned at the ceiling nearly
 * everywhere, which is exactly why it gets light ink and cannot be the only
 * thing drawn. The colour is the low/mid/high mix measured against the track's
 * own average, so a breakdown reads differently from a drop.
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
  /**
   * The shape, resolved to this bar's width.
   *
   * Kept between renders because it depends only on the track and the width,
   * while this component redraws on every position tick. Rebuilding it thirty
   * times a second to move a playhead would be work the audio thread has to
   * compete with.
   */
  const shapeRef = useRef<Shape | null>(null);

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
    // The artwork colour, not the interface accent. See DESIGN.md.
    const accent = styles.getPropertyValue("--art").trim() || "#1ed760";
    const middle = height / 2;
    const boundary = width * Math.min(1, Math.max(0, progress));

    if (!data || data.buckets === 0) {
      // No analysis yet: a plain progress bar, so the control is usable
      // immediately rather than absent until a decode finishes. Rounded and
      // the same weight as the one in the transport, so its arrival as a
      // waveform is the only thing that changes.
      const track = Math.max(4, Math.round(height * 0.1));
      const bar = (to: number, colour: string) => {
        context.fillStyle = colour;
        context.beginPath();
        context.roundRect(0, middle - track / 2, Math.max(track, to), track, track / 2);
        context.fill();
      };
      bar(width, styles.getPropertyValue("--border").trim() || "#4d4d4d");
      if (boundary > 0) bar(boundary, styles.getPropertyValue("--text").trim() || "#ffffff");
      return;
    }

    const cached = shapeRef.current;
    const shape =
      cached && cached.data === data && cached.width === width && cached.accent === accent
        ? cached
        : buildShape(data, width, accent);
    shapeRef.current = shape;
    const { peaks, bodies, colours, points } = shape;
    const usable = height - 2;
    const half = (levels: Float32Array, index: number) =>
      Math.max(0.75, (levels[index] * usable) / 2);

    const paint = (from: number, to: number, opacity: number) => {
      if (to - from < 0.5) return;
      context.save();
      context.beginPath();
      context.rect(from, 0, to - from, height);
      context.clip();

      const fill = gradient(context, colours, width, 1);

      // Peak first as a soft aura, then the body solid on top. The peak
      // carries far less information than the body on a modern master -- it is
      // pinned at the ceiling nearly everywhere -- so it gets light, diffuse
      // ink rather than either a dark slab or a hard outline with a hollow
      // gap inside it.
      context.globalAlpha = 0.22 * opacity;
      ribbon(context, peaks, half, middle, points, width);
      context.fillStyle = fill;
      context.fill();

      context.globalAlpha = opacity;
      ribbon(context, bodies, half, middle, points, width);
      context.fillStyle = fill;
      context.fill();
      context.restore();
    };

    paint(boundary, width, AHEAD_FADE);
    paint(0, boundary, 1);
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

interface Shape {
  data: WaveformData;
  width: number;
  accent: string;
  points: number;
  peaks: Float32Array;
  bodies: Float32Array;
  colours: Rgb[];
}

/**
 * Resolve a stored waveform to one sample per pixel of this bar.
 *
 * One per pixel because the shape is a curve, not a row of bars, so there is
 * no reason to quantise it to anything coarser than the screen.
 */
function buildShape(data: WaveformData, width: number, accent: string): Shape {
  const palette = bandPalette(accent);
  const points = Math.max(2, Math.floor(width));
  const peaks = new Float32Array(points);
    const bodies = new Float32Array(points);
    const colours: Rgb[] = new Array(points);
    const mix = new Float32Array(points * 3);

    for (let index = 0; index < points; index += 1) {
      const from = Math.floor((index * data.buckets) / points);
      const to = Math.max(from + 1, Math.floor(((index + 1) * data.buckets) / points));

      // Peak collapses with a maximum -- a transient that survives all the way
      // to the screen is what makes a waveform recognisable. RMS collapses with
      // a mean, because it is already an average and taking maxima of averages
      // just adds a comb of noise to a smooth envelope.
      let peak = 0;
      let total = 0;
      let count = 0;
      // Colour is weighted by level, so one loud bucket decides a column's
      // colour rather than being averaged away by the silence either side.
      let low = 0;
      let mid = 0;
      let high = 0;
      let weight = 0;
      for (let bucket = from; bucket < to && bucket < data.buckets; bucket += 1) {
        const body = data.rms[bucket];
        if (data.peak[bucket] > peak) peak = data.peak[bucket];
        total += body;
        count += 1;
        const w = body + 1;
        low += data.low[bucket] * w;
        mid += data.mid[bucket] * w;
        high += data.high[bucket] * w;
        weight += w;
      }
      peaks[index] = level(peak);
      bodies[index] = level(count > 0 ? total / count : 0);
      mix[index * 3] = weight > 0 ? low / weight : 85;
      mix[index * 3 + 1] = weight > 0 ? mid / weight : 85;
      mix[index * 3 + 2] = weight > 0 ? high / weight : 85;
    }

    // Colour by how each moment differs from this track's own average, not by
    // the absolute split. Music is bass-dominant nearly everywhere, so absolute
    // proportions give every column of every track the same colour; measured
    // against the track's own norm, the kick dropping out of a breakdown and
    // the hats arriving both show up.
    const average = [0, 0, 0];
    for (let index = 0; index < points; index += 1) {
      average[0] += mix[index * 3];
      average[1] += mix[index * 3 + 1];
      average[2] += mix[index * 3 + 2];
    }
    for (let band = 0; band < 3; band += 1) average[band] = average[band] / points || 1;
    for (let index = 0; index < points; index += 1) {
      colours[index] = mixed(
        palette,
        mix[index * 3] / average[0],
        mix[index * 3 + 1] / average[1],
        mix[index * 3 + 2] / average[2],
      );
    }

    // A light smoothing pass. The measurements are honest either way; this only
    // decides whether the eye reads a shape or a picket fence.
    // The peak envelope is jagged by nature, so it is smoothed harder: it is
    // drawn as an outline and an outline that jitters reads as noise.
    smooth(peaks, 4);
    smooth(bodies, 2);


  // A light smoothing pass. The measurements are honest either way; this only
  // decides whether the eye reads a shape or a picket fence.
  //
  // The peak envelope is smoothed harder: it is drawn as a diffuse aura, and
  // an aura that jitters reads as noise.
  smooth(peaks, 4);
  smooth(bodies, 2);

  return { data, width, accent, points, peaks, bodies, colours };
}


/**
 * A closed ribbon: the envelope along the top, mirrored along the bottom.
 *
 * Curves through the midpoints between samples rather than joining them with
 * straight lines, which is what stops a loud passage looking like a row of
 * fence posts.
 */
function ribbon(
  context: CanvasRenderingContext2D,
  levels: Float32Array,
  half: (levels: Float32Array, index: number) => number,
  middle: number,
  points: number,
  width: number,
) {
  const x = (index: number) => (index * width) / (points - 1);
  context.beginPath();
  context.moveTo(0, middle - half(levels, 0));
  for (let index = 0; index < points - 1; index += 1) {
    const cx = (x(index) + x(index + 1)) / 2;
    const cy = middle - (half(levels, index) + half(levels, index + 1)) / 2;
    context.quadraticCurveTo(x(index), middle - half(levels, index), cx, cy);
  }
  context.lineTo(width, middle - half(levels, points - 1));
  context.lineTo(width, middle + half(levels, points - 1));
  for (let index = points - 1; index > 0; index -= 1) {
    const cx = (x(index) + x(index - 1)) / 2;
    const cy = middle + (half(levels, index) + half(levels, index - 1)) / 2;
    context.quadraticCurveTo(x(index), middle + half(levels, index), cx, cy);
  }
  context.lineTo(0, middle + half(levels, 0));
  context.closePath();
}

/** A small moving average, in place. */
function smooth(values: Float32Array, radius: number) {
  const source = Float32Array.from(values);
  for (let index = 0; index < values.length; index += 1) {
    let total = 0;
    let count = 0;
    for (let offset = -radius; offset <= radius; offset += 1) {
      const at = index + offset;
      if (at < 0 || at >= source.length) continue;
      total += source[at];
      count += 1;
    }
    values[index] = total / count;
  }
}

/**
 * One gradient across the whole bar, sampled from the per-column colours.
 *
 * A gradient rather than a colour per bar: the point of drawing this as a
 * continuous shape is that the colour moves continuously too, so a breakdown
 * bleeds into the drop after it instead of stepping.
 */
function gradient(
  context: CanvasRenderingContext2D,
  colours: Rgb[],
  width: number,
  brightness: number,
): CanvasGradient {
  const fill = context.createLinearGradient(0, 0, width, 0);
  const stops = Math.min(64, colours.length);
  for (let stop = 0; stop < stops; stop += 1) {
    const at = stop / (stops - 1);
    const [r, g, b] = colours[Math.min(colours.length - 1, Math.round(at * (colours.length - 1)))];
    fill.addColorStop(
      at,
      `rgb(${Math.round(r * brightness)},${Math.round(g * brightness)},${Math.round(b * brightness)})`,
    );
  }
  return fill;
}

/**
 * Three tones for the three bands, derived from the artwork accent.
 *
 * Spread widely on purpose. Keeping all three within a few degrees of the
 * accent is coherent and unreadable -- every track comes out one flat colour.
 * Bass runs hot and deep, air runs bright and cool, and the mids sit on the
 * accent itself, so the colour says something before you have read anything.
 */
function bandPalette(accent: string): [Rgb, Rgb, Rgb] {
  const [h, s, l] = toHsl(parseHex(accent) ?? [30, 215, 96]);
  const saturated = Math.max(0.62, Math.min(1, s * 1.35));
  return [
    // Deep and hot.
    toRgb(h - 26, Math.min(1, saturated * 1.05), Math.max(0.26, l * 0.58)),
    toRgb(h, saturated, Math.min(0.66, l * 1.05)),
    // Bright, and still coloured -- desaturating the top band to near-white was
    // what washed the whole waveform out to beige.
    toRgb(h + 46, Math.max(0.5, saturated * 0.8), Math.min(0.86, l * 1.45)),
  ];
}

/**
 * Mix the three band tones from a column's share of each band relative to the
 * track's own average.
 *
 * A column that is exactly average in all three comes out on the accent. More
 * bass than usual pulls it deep and hot, more air than usual pulls it bright.
 * The exponent widens the gap between those, because the underlying ratios
 * spend most of their time close to one.
 */
function mixed(palette: [Rgb, Rgb, Rgb], low: number, mid: number, high: number): Rgb {
  const CONTRAST = 2.6;
  low = Math.pow(Math.max(0, low), CONTRAST);
  mid = Math.pow(Math.max(0, mid), CONTRAST);
  high = Math.pow(Math.max(0, high), CONTRAST);
  const total = low + mid + high || 1;
  const channel = (index: number) =>
    Math.round(
      (palette[0][index] * low + palette[1][index] * mid + palette[2][index] * high) / total,
    );
  return [channel(0), channel(1), channel(2)];
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
