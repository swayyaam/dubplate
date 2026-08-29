import { memo } from "react";
import type { ScannedTrack } from "../types";
import { formatSpec } from "../lib/format";

/**
 * Codec plus spec, always visible in the row. Being able to tell a real 24/96
 * FLAC from a 320k MP3 at a glance is most of the point of this player.
 *
 * Lossiness is a fact, not a warning, so it is rendered as a weight and colour
 * difference rather than a green/amber signal. Those two colours stay reserved
 * for the bit-perfect verdict in the signal path panel.
 */
function FormatBadgeImpl({ track }: { track: ScannedTrack }) {
  const spec = formatSpec(track);
  return (
    <span className={`badge badge--${track.lossiness}`} title={describe(track)}>
      <span className="badge__codec">{track.codec.toUpperCase()}</span>
      {spec && <span className="badge__spec">{spec}</span>}
    </span>
  );
}

function describe(track: ScannedTrack): string {
  const parts: string[] = [track.codec.toUpperCase()];
  if (track.lossiness === "lossless") parts.push("lossless");
  if (track.lossiness === "lossy") parts.push("lossy");
  if (track.lossiness === "unknown") parts.push("AAC or ALAC — resolved from the stream in phase 2");
  if (track.sampleRate) parts.push(`${track.sampleRate} Hz`);
  if (track.bitDepth) parts.push(`${track.bitDepth} bit`);
  if (track.lossiness === "lossy") parts.push("no bit depth — decodes to 32-bit float");
  if (track.bitrate) parts.push(`${track.bitrate} kbps`);
  if (track.channels) parts.push(track.channels === 2 ? "stereo" : `${track.channels} ch`);
  return parts.join(" · ");
}

export const FormatBadge = memo(FormatBadgeImpl);
