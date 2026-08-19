# Vibecut TOML configuration manual

A Vibecut project is one TOML file that drives the entire pipeline (input
discovery → telemetry analysis → segment selection → render → mux → transcode).
You run it with:

```bash
vibecut run my_project.toml          # full pipeline
vibecut validate my_project.toml     # parse + validate, no rendering
vibecut run my_project.toml --explain --print-effective-config   # debug
```

Unknown fields are rejected with `deny_unknown_fields` — typos surface as
errors instead of silently dropping. Time durations everywhere accept the
[humantime](https://docs.rs/humantime) format: `"500ms"`, `"2.5s"`, `"1m30s"`,
`"5m"`. Resolutions are `"WIDTHxHEIGHT"` strings. Colors are `#RRGGBB` or
`#RRGGBBAA`.

## Minimum viable project

```toml
schema_version = 1

[project]
name            = "My First Edit"
target_duration = "30s"
fps             = 30

[input]
folder = "footage/"

[[outputs]]
path       = "out/edit.mp4"
resolution = "1920x1080"
```

Everything else is optional and defaulted.

---

## `schema_version` (required)

```toml
schema_version = 1
```

Must be `1` — the current schema. Mismatches abort with a clear error.

---

## `[project]` (required)

Where the build is identified and overall pacing is set.

```toml
[project]
name            = "Summer Surf"      # used in default workspace dir name
target_duration = "60s"              # any humantime; final output ≈ this length
fps             = 30                 # output frame rate
seed            = 42                 # optional; deterministic picker output (default: random)
fade_in         = "500ms"            # video fade-in from black at start (default: 500ms; 0 = none)
fade_out        = "500ms"            # video fade-out to black at end (default: 500ms; 0 = none)
temp_dir        = "out/.cache_summer"  # optional; default: <first output's parent>/.vibecut_<name>
models_dir      = "models"             # optional; where ONNX model files live (default: "models")
```

`models_dir` points at a folder that may contain `silero_vad.onnx` (and
future face / scene-detection nets). The pipeline auto-loads any model
present and gracefully skips ML features when files are missing — no
explicit on/off switch.

---

## `[input]` (required)

Where to find the source footage.

```toml
[input]
folder  = "footage/"                       # walk this directory recursively
include = ["**/*.MP4", "**/*.mp4"]         # glob patterns (default shown)

# Optional: hard-coded "must include this exact range" overrides the picker
# for one or more sources. Time strings accept any humantime form.
[[input.ranges]]
file  = "GX010023.MP4"                     # match by filename suffix
start = "1m12s"
end   = "1m30s"
```

---

## `[[outputs]]` (required, ≥ 1 entry)

One or more target files. The body is rendered once at the **highest**
resolution among all entries, then transcoded to each output's resolution +
encoder + CRF settings — so adding a 1080p alongside a 4K render only adds
a fast resize pass.

```toml
[[outputs]]
path          = "out/edit_4k.mp4"
resolution    = "3840x2160"               # WIDTHxHEIGHT
encoder       = "auto"                    # auto | libx264 | libx265 | nvenc | amf | qsv
crf           = 20                        # quality factor (-crf libx264, -cq NVENC, etc.); lower = better
preset        = "p5"                      # optional; encoder-specific (libx264: "veryfast", NVENC: "p5", …)
audio_bitrate = "192k"                    # AAC bitrate; default "192k"

# A second output at 1080p alongside the 4K above:
[[outputs]]
path          = "out/edit_1080.mp4"
resolution    = "1920x1080"
crf           = 22
```

`encoder = "auto"` picks the best available hardware encoder
(NVENC > AMF > QSV > libx264) at runtime.

---

## `[soundtrack]` (optional)

A single section configures music + ducking + the beat-burst effect.

```toml
[soundtrack]
# Either a single file (shorthand) ...
file = "music/track.wav"
# ... or several, concatenated in order into one virtual track:
# files = ["intro.mp3", "main.mp3", "outro.mp3"]

beat_align            = true             # snap clip lengths to beat-multiples (default: true)
energy_drives_pacing  = true             # let soundtrack energy modulate pacing (default: true)
fade_out              = "1500ms"         # tail fade-out at end of output (default: 1500ms)

# --- Ducking: dip the music to expose source audio ---
duck_count            = 2                # number of duck windows; 0 disables (default: 2)
duck_min_spacing      = "4s"             # min gap between ducks in output time (default: 4s)
duck_min_dur          = "4s"             # shortest duck window length (default: 4s)
duck_max_dur          = "7s"             # longest; actual length scales with audio interestingness (default: 7s)
duck_ramp             = "300ms"          # fade in/out each duck window (default: 300ms)
ducking_db            = -18.0            # how far the music dips during a duck (default: -18 dB)
vad_speech_weight     = 1.5              # speech presence boost for duck pick (default: 1.5)
                                         # only consulted when silero_vad.onnx is in [project].models_dir

# --- Beat burst: rapid music-video cuts at the soundtrack's energy peak ---
beat_burst            = "auto"           # auto | off | on
beat_burst_min_beats  = 8                # silently skipped if fewer beats fit (default: 8)
beat_burst_max_beats  = 12               # cap on burst length (default: 12)
```

`beat_burst = "auto"` enables the burst iff: tempo ∈ [80, 180] BPM,
pulse strength ≥ 1.8, and the loud region is ≥ 6 s long. Cuts every beat
≤ 140 BPM, every-other above. Placed in the middle 25–70% of the output.

---

## `[intro]` and `[outro]` (optional)

Title and end cards. Both share the same shape — `[outro]` just appears
after the body.

```toml
[intro]
duration = "3.0s"
fade_in  = "500ms"                       # default: 400ms
fade_out = "500ms"                       # default: 400ms
mode     = "card"                        # "card" (solid bg + layers) | "overlay" (layers on real footage)
bg_color = "#000000"                     # ignored in overlay mode (default: #000000)

# Stack any number of layers; later layers paint on top of earlier ones.
[[intro.layers]]
kind         = "text"                    # required: "text" | "image"
content      = "Costa Rica · 2026"       # also accepted as `text = "..."`
font         = "arialbd.ttf"             # absolute path or Windows Fonts/<name> (optional)
size         = 96                        # default: 96
size_end     = 110                       # optional; if set & ≠ size, fontsize ramps linearly across visibility window ("growing text")
color        = "#FFFFFF"                 # default: #FFFFFF
position     = "center"                  # see anchor names below
y_offset     = -40                       # pixels; negative = up. Default 0
x_offset     = 0                         # pixels; default 0
appear_at    = "0s"                      # when this layer becomes visible (default: 0s)
disappear_at = "2.5s"                    # when it disappears (default: card duration)
fade_in      = "400ms"                   # alpha ramp anchored at appear_at
fade_out     = "400ms"                   # alpha ramp ending at disappear_at
# Optional translucent box behind the text:
background   = { color = "#000000AA", padding = "20px" }

# Stage 2: a second text layer that appears after the first disappears.
[[intro.layers]]
kind         = "text"
content      = "March 2026"
size         = 36
size_end     = 56
position     = "center"
y_offset     = 60                        # stacked below the title
appear_at    = "2.8s"
disappear_at = "5s"

[[intro.layers]]
kind         = "image"
path         = "logos/brand.png"         # PNG (transparency supported) or JPEG
position     = "bottom-right"
height       = 120                       # px; width auto-scales (default: 200)
x_offset     = -40
y_offset     = -40
opacity      = 0.85                      # 0..1 (default: 1.0)
appear_at    = "1s"                      # logo bumps in 1 s into the card
disappear_at = "5s"
fade_in      = "500ms"
fade_out     = "500ms"
```

**Position anchors** (text and image): `center` (default), `top-left`,
`top-right`, `top-center`, `bottom-left`, `bottom-right`, `bottom-center`.

**Visibility windows** (`appear_at` / `disappear_at`): default to `0s` →
card duration. Set them to stage layers in and out of view (e.g. a
two-stage GoPro-Quik-style title sequence: title appears 0–2.5 s, date
2.8–5 s). The layer's alpha ramp (`fade_in`/`fade_out`) is anchored
inside the window.

**Growing text** (`size_end`): when set on a text layer and different
from `size`, the font size linearly interpolates from `size` to
`size_end` across the layer's visibility window. Common pattern for
emphasis: title grows 80 → 110 px while it fades in.

**`mode = "overlay"`** draws the layers on top of the first selected
segment (intro) or last segment (outro) — no separate clip is generated.
**Both text and image layers work in overlay mode**, so a logo can ride
on the last few seconds of real footage (the GoPro Quik outro pattern).
Use a PNG with transparency for clean compositing; JPEGs work too but
their solid background will be visible.

---

## `[scoring]` (optional)

Weights that combine into one score curve per source. Defaults are
sensible — only override when you know what you're tuning.

```toml
[scoring]
hilight_tags         = 1.0               # GoPro HiLight button presses
accel_peaks          = 0.6               # accelerometer spikes (above-baseline)
gps_speed            = 0.4               # high-speed moments
faces                = 0.3               # ML face-detection peaks
motion_peaks         = 0.3               # visual frame-difference MSD
sharpness_floor      = 0.2               # reject bottom 20% by Laplacian variance (0 = no floor)
exposure_floor       = 0.15              # reject bottom 15% by exposure score (0 = no floor)
diversity_per_source = 0.5               # boost segments far from already-picked moments in same source
```

---

## `[selection]` (optional)

How the picker chooses clip lengths and how it spreads across sources.

```toml
[selection]
min_clip       = "1.5s"                  # default: 1.5s
max_clip       = "6s"                    # default: 6s; must be ≥ min_clip
order          = "chronological"         # "chronological" | "energy_arc"
max_per_source = 5                       # optional override; default: ceil(K / n_sources_with_signal)
variety        = 0.0                     # 0..1; deterministic top-K when 0; see below
```

Use `max_per_source` when target duration is large vs. number of sources
(e.g., 5-min cut over 80 sources where each has only a couple of strong
moments) — without it the auto-cap = 1 will under-fill.

`order` arranges the picker's chosen segments along the output timeline:

| Value | Behaviour |
|---|---|
| `chronological` (default) | Segments appear in their actual recording order. Safe for multi-day / multi-event projects where a backwards jump (sunset → morning) feels jarring. Images from `[[input.images]]` are inserted at evenly-spaced positions and do **not** participate in the chronological sort. |
| `strict_chronological` | Like `chronological`, but images join the timeline by their declared `timestamp` (see [`[[input.images]]`](#input)) instead of being interleaved at fixed slots. Use this for reportage-style edits — multi-day trips, events with many phone photos mixed with sparse video — where the natural reading order is "by the clock." Every image must carry a `timestamp`; validation fails otherwise. |
| `energy_arc` | Center-out zigzag placement: strongest pick at the middle output position, second-strongest just to its left, third just to its right, alternating outward. Result: weakest segments at the edges, climax in the middle — a triangular score profile across the timeline. Best for single-event highlight reels where pacing-driven cuts beat strict chronology. Ignores recording time entirely. |

Pairs naturally with `[selection].variety` to vary which moments occupy the
peak and edges across renders, and with `--random-seed` for a fresh
arrangement on each run while keeping the same arc shape.

`variety` controls per-candidate score jitter. The picker is otherwise
deterministic (same `[project].seed` always produces the same cut), so to
get a different video on each render you change either the seed or this
value. Each candidate's score becomes `score * (1 + r * variety)` where
`r ∈ [-1, 1]` is keyed by the seed and the candidate index — meaning
"same seed + same variety = same cut", but bumping either re-rolls the
picks. Practical settings:

| `variety` | Behaviour |
|---|---|
| `0.0` (default) | strict top-K. Same seed → same cut, every time. |
| `0.15` | two close-scoring candidates might swap. Subtle re-mix. |
| `0.30` | noticeable shake-up. Quality stays good. |
| `0.50+` | real variety; you'll occasionally see a much weaker moment win — useful when you want a "deliberately different" cut, less so for "best-quality automatic edit". |

**Cache impact:** changing `variety` or `seed` does **not** invalidate the
expensive visual-analysis cache (~22 min cold). Only the per-segment
normalize cache for *segments whose pick changed* re-encodes — typically
2–5 of 30 at `variety = 0.15`. So iterating on variety is fast.

---

## `[transition]` (optional)

```toml
[transition]
default                = "fade"          # any ffmpeg xfade name; "cut" / "none" disables xfade
duration               = "400ms"         # default: 400ms
palette                = ["fade", "dissolve", "circleopen"]   # currently informational
# --- Speed-ramp ("whip-up"): the last X ms of the outgoing clip plays at
#     Y× speed before the cut, with an optional GoPro-Quik flash. ---
speed_ramp_probability = 0.0             # 0..1; 0 = never, 0.4 = "some, like a typical reel"
speed_ramp_duration    = "350ms"         # length of the SOURCE speed-up portion (default: 350ms)
speed_ramp_factor      = 2.5             # how fast — 1.0 = off, 2.5 = typical reel, 3.0+ = aggressive
speed_ramp_flash       = true            # GoPro-Quik fade-to-white at the seam (default: true)
```

`speed_ramp_*` triggers a Reels/TikTok-style "whip-up" on a deterministic
random subset of transitions, sized by `speed_ramp_probability`. Which
transitions get ramped is keyed by `[project].seed`, so renders stay
reproducible. The renderer extends each ramped segment's input window by
`duration × (1 − 1/factor)` so the output's total duration is unchanged —
the sped-up tail consumes the same wall-clock as the original tail; you
just see more source content fly by in that final stretch. Audio (when
present) is sped up to match via `atempo`.

The xfade between a ramped segment and its successor automatically
collapses to ~1.5 frames (a near-hard cut) so the speed change itself
reads clearly instead of being masked by a long crossfade.

**Tuning notes** (output ramp duration = `speed_ramp_duration / speed_ramp_factor`):

| `speed_ramp_factor` | Reads as | Use when |
|---|---|---|
| 1.5 – 2.0 | Subtle, more like rhythm | Calm or contemplative footage; documentary cuts |
| 2.5 – 3.0 | Typical reel "whip-up" | Active footage; kids, sports, road trips |
| 4.0+ | Aggressive | Very dynamic content only; otherwise nauseating |

Pick `speed_ramp_duration` so the OUTPUT ramp (= duration / factor) is
**at least 300 ms (~9 frames at 30 fps)** — anything shorter is too
brief for the eye to register. With `factor = 3.0`, that means
`duration ≥ 900 ms`; for a clearly visible Quik-style effect, **2000–
3000 ms @ 3× → 670–1000 ms output ramp** works well.

`speed_ramp_flash = true` (default) bakes a GoPro-Quik kinetic flash
into each ramped tail:

- **Brightness warm-up** — `eq=brightness` ramps from 0 to ~0.05 across
  the whole sped-up portion. Subtle, but it lifts the tail noticeably.
- **Fade-to-white** over the last ~67 ms (≈ 2 frames at 30 fps) so the
  segment ends on a near-pure-white frame. The white frame *becomes*
  the visible transition into the next clip — instead of a hard cut you
  get a flash of light, the unmistakable Quik signature.

Set `speed_ramp_flash = false` for a clean, neutral speed-up with no
colour treatment.

Excluded from ramping automatically:
- the **last** segment (no transition out)
- **beat-burst** segments and the segment immediately preceding one
  (already a kinetic moment — piling a ramp on top is busy)
- segments where the source chapter doesn't have the extra footage
  available past `start + duration`

When `speed_ramp_probability > 0`, the run prints a one-line summary like
`speed-ramp: 14 of 32 transitions (50%, 2500ms @ 3.0x)` so you can
sanity-check how many seams will get the effect.

---

## `[multi_angle]` (optional)

Multi-camera angle planning. Off by default. See `examples/multicam.toml`.

```toml
[multi_angle]
enabled  = true
sync     = "auto"                        # auto | creation_time | audio | manual
min_hold = "1.5s"                        # min time on one angle before switching (default: 1500ms)

# Optional explicit groups. When empty and enabled=true, groups are
# auto-discovered from overlapping recording windows (creation_time +
# duration overlap by ≥ 1 s).
[[multi_angle.groups]]
master  = "0042"                         # source id (trailing digits for GoPro chapters)
members = ["0099", "0123"]
# Manual offsets, only used when sync = "manual". Sign convention:
# member_local_file_time = master_scene_time + offset.
# Negative offset → member started LATER than master.
offsets = { "0099" = "+2.4s", "0123" = "-1.20s" }
```

`sync` modes:
- **auto** — audio xcorr if a 30 s overlap window exists, else `creation_time`
- **creation_time** — fast; trusts MP4 `creation_time` deltas
- **audio** — explicit audio xcorr refinement; falls back if confidence < 0.5
- **manual** — reads `offsets` from the group entry above

---

## `[grading]` (optional)

```toml
[grading]
lut       = "luts/teal_orange.cube"      # optional 3D LUT applied to body footage
stabilize = false                        # vidstab two-pass stabilization (default: false)
```

**Note on `stabilize`**: leave it `false` for modern GoPro footage that
was recorded with HyperSmooth on. Stacking vidstab on top of in-camera
stabilization wastes ~90 s per render (vidstab pass-1 is the slowest
stage on most projects), softens the image, and adds an extra edge crop
because vidstab needs border pixels to absorb its corrections. Only
enable when the source is HyperSmooth-Off, an older non-GoPro camera, or
otherwise actually shaky.

---

## ML signals

There's no separate `[ml]` section anymore — the pipeline always tries to
load any model files it finds and gracefully skips ML behaviour when
they're missing.

- **Model location**: `[project].models_dir` (default `"models"`).
- **Tuning knobs**: live with the feature they affect (e.g.
  `[soundtrack].vad_speech_weight` controls speech-driven duck-window
  selection).
- **Runtime**: ONNX Runtime probes CUDA EP first and falls back to CPU
  automatically. No configuration needed.

To enable speech-driven ducking, drop `silero_vad.onnx` into your
`models_dir`. To disable any ML feature, remove the corresponding model
file (or move it elsewhere).

---

## Complete kitchen-sink example

```toml
schema_version = 1

[project]
name            = "Egipt · highlights"
target_duration = "5m"
fps             = 30
seed            = 23
fade_in         = "800ms"
fade_out        = "1200ms"

[[outputs]]
path          = "out/egypt_5min.mp4"
resolution    = "1920x1080"
encoder       = "auto"
crf           = 20
audio_bitrate = "192k"

[[outputs]]
path       = "out/egypt_5min_4k.mp4"
resolution = "3840x2160"
crf        = 22

[input]
folder  = "E:/footage/Egipt 2025"
include = ["**/*.MP4"]

[soundtrack]
file                  = "music/beach moments.mp3"
beat_align            = true
duck_count            = 6
duck_min_spacing      = "30s"
duck_min_dur          = "5s"
duck_max_dur          = "10s"
ducking_db            = -18
fade_out              = "4000ms"
beat_burst            = "auto"
vad_speech_weight     = 2.0    # heavier weight on speech for vacation content

[selection]
min_clip       = "2s"
max_clip       = "5s"
max_per_source = 5

[transition]
default  = "fade"
duration = "500ms"

[intro]
duration = "5.5s"
mode     = "card"
bg_color = "#0a1628"
fade_in  = "700ms"
fade_out = "600ms"

[[intro.layers]]
kind     = "text"
content  = "Egipt · 2025"
font     = "arialbd.ttf"
size     = 120
color    = "#FFE4B5"
position = "center"

[[intro.layers]]
kind     = "image"
path     = "logos/brand.png"
position = "bottom-right"
height   = 100
x_offset = -40
y_offset = -40
opacity  = 0.9

[outro]
duration = "7.0s"
mode     = "overlay"
fade_in  = "600ms"
fade_out = "1200ms"

[[outro.layers]]
kind       = "text"
content    = "@vbocan · vacanță 2025"
font       = "arialbd.ttf"
size       = 64
color      = "#FFFFFF"
position   = "bottom-center"
background = { color = "#000000AA", padding = "20px" }

[grading]
stabilize = true
```

---

## See also

- `examples/basic.toml` — simplest working config
- `examples/atv.toml` — ATV ride, 25 s
- `examples/atv_2min.toml` — full feature showcase (2-minute, beat-burst, ML, stabilize)
- `examples/egypt.toml` — long-form vacation cut (5 min, 80 sources)
- `examples/multicam.toml` — multi-camera example
- `examples/sag_timiseni.toml` — two-stage growing-text intro + logo-on-footage outro (GoPro-Quik style)
- `examples/masinuta.toml` — kid-themed family edit; aggressive speed-ramps + flash + beat-burst
- `examples/tomesti_2026.toml` — DJI drone footage; speed-ramps deliberately OFF for contemplative pacing
- `examples/smoke.toml` — minimal smoke-test
- `examples/vartop.toml`, `examples/ziua_mariei.toml` — winter / family vacation projects

---

## Pipeline guarantees and known gotchas

A few invariants worth knowing if you're using Vibecut programmatically
or building tooling on top of it:

- **Determinism.** With `[project].seed` set, the same TOML always
  produces the same picks, the same segment order, the same ramped
  transitions, and the same beat-burst placement. Pass `--random-seed`
  on the CLI to override the seed with a fresh one per render (the chosen
  seed is logged so you can lock it in later).
- **Cache fingerprinting.** Each rendered segment's filename includes a
  blake3 hash of `(source_path, in-chapter start, duration, width,
  height, fps, crf, lut, stabilize, speed-ramp params)`. Toggling any of
  these forces re-encode of the affected segments only. Visual analysis
  results live in `<output_parent>/.vibecut-cache/` keyed by source
  hash + ffprobe metadata — that 22-min cold-start cost only happens once
  per source file.
- **Audio length capping.** The final mux probes the body's actual video
  stream duration and passes it as a hard `-t` bound, so AAC frame
  quantisation in `acrossfade` chains can't leave the soundtrack playing
  past the picture (which would otherwise show as a held last frame at
  the very end). No user knob — this is automatic.
- **No-audio sources.** DJI drone footage and mic-off GoPro recordings
  are auto-detected; the renderer synthesises silent stereo for those
  segments so the downstream `acrossfade` chain sees a consistent stream
  shape. A single-line note prints during the run when this kicks in.
- **Manifest output.** Each render writes `<output>.manifest.json`
  alongside the mp4 with the resolved segment list, sources, and
  effective settings. Useful for postprocessing, audits, or driving
  external tooling.
