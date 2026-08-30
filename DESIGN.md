# Design System

Adapted from a Spotify-derived specification. The rules below are what the
application actually implements; where it departs from the source spec, the
departure is stated and the reason given.

## Theme

Near-black, content-first. The interface recedes so that artwork and waveforms
carry the colour. Depth comes from shade variation rather than borders.

| Role | Value |
|---|---|
| Base background | `#121212` |
| Surface (cards, bars) | `#181818` |
| Interactive surface | `#1f1f1f` |
| Raised card | `#252525` |
| Text | `#ffffff` |
| Secondary text | `#b3b3b3` |
| Muted text / borders | `#7c7c7c` |
| Accent | `#1ed760` |
| Accent pressed | `#1db954` |
| Negative | `#f3727f` |
| Warning | `#ffa42b` |
| Announcement | `#539df5` |

### One departure: two accents

The source spec says the accent is functional only and the interface is
otherwise achromatic, and also that album art is the primary source of colour.
Those pull in opposite directions for an application whose main visual element
is a waveform.

So there are two:

- `--accent` (`#1ed760`) is the functional brand colour. Play controls, active
  navigation, primary actions, progress. Never decorative.
- `--art` is sampled from the current cover and is used by the waveform alone.
  It defaults to `--accent`, so a track with no artwork is green.

Everything else stays achromatic.

## Typography

`SpotifyMixUI` is proprietary and not shipped, so the stack falls through to the
spec's own fallbacks. Weights are binary: 700 for emphasis and navigation, 400
for everything else, 600 only on button labels.

| Role | Size | Weight | Notes |
|---|---|---|---|
| Section title | 24px | 700 | |
| Feature heading | 18px | 600 | line-height 1.3 |
| Body bold | 16px | 700 | |
| Body | 16px | 400 | |
| Button | 14px | 700 | uppercase, letter-spacing 1.4px |
| Nav / caption | 14px | 400/700 | |
| Small | 12px | 400/700 | |
| Badge | 10.5px | 600 | |

Numeric data uses a monospaced face with tabular figures. That is a departure:
this application shows a great deal of measured data -- sample rates, bit
depths, cutoff frequencies, loudness -- and proportional digits in a column do
not line up.

## Geometry

Pills and circles, never square.

| Token | Value | Use |
|---|---|---|
| `--r-badge` | 2px | Tags |
| `--r-sm` | 4px | Small elements |
| `--r` | 8px | Cards, dialogs, art |
| `--r-panel` | 12px | Overlay panels |
| `--r-pill` | 500px | Primary buttons, inputs |
| `--r-full` | 9999px | Navigation pills, chips |
| circle | 50% | Play controls, avatars |

## Elevation

Shadows are heavy, because on a near-black background a subtle one is invisible.

| Level | Treatment |
|---|---|
| Base | `#121212` |
| Surface | `#181818` / `#1f1f1f` |
| Elevated | `rgba(0,0,0,0.3) 0 8px 8px` |
| Dialog | `rgba(0,0,0,0.5) 0 8px 24px` |
| Inset border | `rgb(18,18,18) 0 1px 0, rgb(124,124,124) 0 0 0 1px inset` |

## Spacing

Base unit 8px. Content is dense: this is an application for scanning a library,
not a page for reading.

## Don't

- Use the accent decoratively, or as a background for large areas
- Expose raw grey borders -- use shade differences or inset shadows
- Use light backgrounds for primary surfaces
- Use square buttons
- Use relaxed line heights
