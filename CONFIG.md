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
explicit on/off switch. A relative `models_dir` is relative to the
working directory. A model missing there is looked up next in each folder
of `$VIBECUT_MODELS_DIR` (separated like `PATH`), then in `/app/models`,
where the Docker image ships `silero_vad.onnx`; the render log says which
copy it loaded.

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

**GoPro chapters.** Long GoPro recordings are split into chapter files;
the files of one recording in one folder are stitched back into one
continuous source, in chapter order. Both naming schemes are recognised
(any letter case, `.MP4` / `.mp4`):

| Camera | Chapter 1 | Chapter 2 | Chapter 3 | Source id |
|---|---|---|---|---|
| Newer, HEVC | `GX010042.MP4` | `GX020042.MP4` | `GX030042.MP4` | `0042` |
| Newer, H.264 | `GH010042.MP4` | `GH020042.MP4` | `GH030042.MP4` | `0042` |
| Older | `GOPR0042.MP4` | `GP010042.MP4` | `GP020042.MP4` | `0042` |

The source id (used by `[selection].source_weights` and
`[multi_angle].groups`) is the 4-digit recording number. When two
recordings would share it — an older and a newer camera both at 0042,
or two cards in different folders — they stay separate sources and
get new ids, in up to three steps until every id is unique:

1. the first chapter's file name, lowercased: `gopr0042`, `gx010042`;
2. prefixed with the first folder that differs between them:
   `carda-gx010042` and `cardb-gx010042` for two cards copied as
   `cardA/DCIM/100GOPRO/…` and `cardB/DCIM/100GOPRO/…`, `day2-gx010042`
   for a copy in a `day2/` subfolder;
3. numbered in path order (`gx010042`, `gx010042-2`) when even the
   folders don't tell them apart.

Standalone clips with the same file name (`a/clip.mp4`, `b/clip.mp4`)
go through the same steps (`a-clip`, `b-clip`); ids that differ only in
letter case count as the same. Generated ids use only lowercase letters,
digits, `_` and `-` (other characters become `-`, so `Card A` →
`card-a`), which is what "more of / less of clip …" in the app accepts.
Ids that don't clash keep their usual form.

GoPro's low-res proxies (`.LRV`: `GL010042.LRV` in the GX / GH scheme,
`GOPR0042.LRV` / `GP010042.LRV` in the older one) and thumbnails
(`.THM`) are never sources, even if `include` names their extension or
a proxy was renamed to `GL…MP4`; DJI `.LRF` proxies are skipped the
same way.

---

## `[[outputs]]` (required, ≥ 1 entry)

One or more target files. The body is rendered once in the **master
shape** — the largest entry that isn't a vertical or square platform
preset — then transcoded to each output's resolution + encoder + CRF
settings, so adding a 1080p alongside a 4K render only adds a fast resize
pass. An output whose aspect differs from the body's (a 9:16 Reel next to
a 16:9 master) is reframed in that same pass; see `fit` below.

Each `path` is written as `<name>.partial.<ext>` next to it while the render
runs and only renamed into place once the whole render has succeeded, so a
failed or stopped render (the web UI's **Stop**, `POST /api/jobs/{id}/cancel`,
the MCP `cancel_render` tool) leaves the previous video and its manifest
exactly as they were.

```toml
[[outputs]]
path           = "out/edit_4k.mp4"
resolution     = "3840x2160"              # WIDTHxHEIGHT; optional when `preset` sets it
encoder        = "auto"                   # auto | libx264 | libx265 | nvenc | amf | qsv
crf            = 20                       # quality factor (-crf libx264, -cq NVENC, etc.); lower = better. Default: the preset's, else 20
encoder_preset = "slow"                   # optional; for this output's own encode pass, see below
audio_bitrate  = "192k"                   # AAC bitrate. Default: the preset's, else "192k"

# A second output at 1080p alongside the 4K above:
[[outputs]]
path          = "out/edit_1080.mp4"
resolution    = "1920x1080"
crf           = 22

# The same cut, vertical, for Reels / TikTok / Shorts:
[[outputs]]
path   = "out/edit.reels.mp4"
preset = "reels"                          # platform preset, see the table below
fit    = "blur"                           # blur (default) | crop | pad; only used when the aspect differs
```

`encoder = "auto"` picks the best available hardware encoder
(NVENC > AMF > QSV > libx264) at runtime.

`encoder_preset` names a preset of the encoder that actually runs:
libx264 / libx265 `ultrafast` … `veryslow` (`placebo`), NVENC `p1` … `p7`
(and its older `slow`, `medium`, `fast`, `hq`, …), AMF `speed`, `balanced`
or `quality`, QSV `veryfast` … `veryslow`. Since `encoder = "auto"` only
settles the encoder at render time, a name that encoder doesn't take (`p5`
on a machine without NVENC, as in the Docker image) is skipped with a
warning and the encoder keeps its default (libx264 `veryfast`, NVENC `p5`,
AMF `balanced`, QSV `medium`). It applies to an output's own encode pass;
a single output written straight from the body uses the default.

### Platform presets (`preset`)

A `preset` fills in every key the output leaves unset; keys you set
yourself (`resolution`, `crf`, `audio_bitrate`, `max_bitrate`) always win.

| `preset`                    | Shape         | Resolution | CRF | Audio | Video bitrate cap |
|-----------------------------|---------------|------------|-----|-------|-------------------|
| `master`                    | as configured | 1920x1080  | 20  | 192k  | none              |
| `youtube`                   | 16:9          | 1920x1080  | 18  | 192k  | none              |
| `reels`, `tiktok`, `shorts` | 9:16          | 1080x1920  | 20  | 192k  | none              |
| `square`                    | 1:1           | 1080x1080  | 20  | 192k  | none              |
| `whatsapp`                  | 9:16          | 720x1280   | 23  | 128k  | 3M peak: 30 s stays at or under ~12 MB, inside WhatsApp's 16 MB |

Vertical and square presets never set the body's shape. The body is the
largest `master` / `youtube` / no-preset output; when every output is
vertical or square, it is a 16:9 master as tall as the largest output's
short side, and at least 1920x1080 (so 1920x1080 for `reels` and for
`whatsapp`, 3840x2160 for a `reels` output at `resolution = "2160x3840"`):
card text `size`s are pixels of a 1080p frame, and a smaller body would cut
a long title off before the reframe. An explicit
`resolution` with no platform preset sets the body as before, vertical or
not.

`max_bitrate = "3M"` (or `"2500k"`) caps any output's peak video bitrate;
`"none"` drops a preset's cap. A capped output always gets its own encode
pass. Before platform presets, `preset` held the encoder preset: a value
like `"p5"` or `"veryfast"` there still works and is read as
`encoder_preset` (with a warning), under the same rule — skipped when the
encoder that runs doesn't take it.

**`fit`** — how a frame fills an output of a different aspect:

- `blur` (default): the whole frame, fitted, over a blurred and darkened
  fill made from the same frame — no black bars, nothing cut off.
- `crop`: a centre crop that fills the output (a 9:16 crop of a 16:9
  frame keeps its middle third).
- `pad`: the whole frame with black bars.

**Titles in vertical outputs.** Intro/outro cards are rendered once, at
the body's resolution (their `size` and offsets are master pixels), and
reach other shapes through the reframe. In a portrait output narrower
than the master, the frame is shown at 76 % of its fitted width during
the intro and outro — so a title as wide as the master frame stays inside
the 9:16 safe area, clear of the platform's side buttons — and eases back
to full width over 0.4 s after the title. A `crop` output shows the blur
fit during those spans instead, so a title is never cut. The spans are
recorded in the manifest as `card_windows`.

**A platform version of a finished render**, without re-rendering: `vibecut
reformat out/edit.mp4 --preset reels` (optionally `--fit crop|pad`, `--out
<path>`) writes `out/edit.reels.mp4` in one ffmpeg pass with the same
reframe, reading `card_windows` from the render's manifest. The web UI's
Share sheet (`POST /api/reformat`) and the MCP `reformat` tool do the same.
Given a version instead (`out/edit.square.mp4`), it works from the render
next to it (`out/edit.mp4`), so a vertical version of the square one is
still one reframe of the master; if the render is gone, the result keeps
the version's whole name (`out/edit.square.reels.mp4`). Like the render's
outputs, the version is written as `out/edit.reels.partial.mp4` and renamed
into place only when the pass has finished. In the web UI each version is
a job of kind `reformat` in the same queue as renders (`GET /api/jobs`,
the Renders list), stopped the same way (`POST /api/jobs/{id}/cancel`,
which keeps the last good version); asking again for a version that is
still being made joins that job instead of starting a second pass.

---

## `[soundtrack]` (optional)

A single section configures music + ducking + the beat-burst effect.

```toml
[soundtrack]
# Either a single file (shorthand) ...
file = "music/track.wav"
# ... or several, concatenated in order into one virtual track:
# files = ["intro.mp3", "main.mp3", "outro.mp3"]

beat_align            = true             # cut on the beat: whole-beat clips, every cut on a beat (default: true)
energy_drives_pacing  = true             # let soundtrack energy modulate pacing (default: true)
fade_out              = "1500ms"         # tail fade-out at end of output (default: 1500ms)

# --- Ducking: dip the music to expose source audio ---
duck_count            = 2                # number of duck windows; 0 disables (default: 2)
duck_min_spacing      = "4s"             # min gap between ducks in output time (default: 4s)
duck_min_dur          = "4s"             # shortest duck window length (default: 4s)
duck_max_dur          = "7s"             # longest; actual length scales with audio interestingness (default: 7s)
duck_ramp             = "300ms"          # fade in/out each duck window (default: 300ms)
ducking_db            = -18.0            # how far the music dips during a duck (default: -18 dB)
vad_speech_weight     = 1.5              # speech presence boost for duck pick (default: 1.5; 0 = off)
                                         # only consulted when silero_vad.onnx is found in [project].models_dir,
                                         # $VIBECUT_MODELS_DIR or /app/models (always there in the Docker image)

# --- Beat burst: rapid music-video cuts at the soundtrack's energy peak ---
beat_burst            = "auto"           # auto | off | on
beat_burst_min_beats  = 8                # silently skipped if fewer beats fit (default: 8)
beat_burst_max_beats  = 12               # cap on burst length (default: 12)
```

`beat_burst = "auto"` enables the burst iff: tempo ∈ [80, 180] BPM,
pulse strength ≥ 1.8, and the loud region is ≥ 6 s long. Cuts every beat
≤ 140 BPM, every-other above. Starts in the middle 25–70% of the output, at
the loudest moment of the song that actually *plays* there — through loop
copies of a looped soundtrack, and after the intro card — on the beat: the
clip it lands in is cut short so the burst starts right on it. Every burst
cut is a different moment. When the footage runs short of them the burst
gets fewer cuts — two beats long when one-beat cuts can't reach
`beat_burst_min_beats` — and when even that falls short it is skipped
rather than repeat a shot.

### Choosing the song

Proposals (`POST /api/propose`, MCP `propose_vibe`) fill in `file`
themselves: without a `soundtrack` in the request they pick a library song
whose tempo, energy and length suit the vibe, write the vibe's own
`[soundtrack]` settings around it, and explain the pick in a comment above
the section. `soundtrack = "none"` in the request leaves the section out;
a path there must be an audio file in the projects or music folders.
A song's title, artist, licence and credit line come from an optional
`<track>.json` or `library.json` next to it; every render writes them, with
the file's SHA-256, to the manifest's `soundtrack` list. See
[MUSIC.md](MUSIC.md) for the scoring, the metadata format, licences and
the starter pack.

### Beat grid

The soundtrack's beats come from a fitted grid, not just a tempo estimate.
The raw autocorrelation tempo is quantised to the analyser's ~23 ms frames
and can be a few percent off (e.g. 117.5 BPM for a 120 BPM track) — enough
to drift a whole beat every 10–15 s. Vibecut refines it to the exact
interval and phase, times every matched hit to ~1 ms in the raw audio, and
halves or doubles the tempo when the onset pattern shows it was
double-time or half-time. The run log reports the fit:

```text
tempo: 120.00 BPM (beat interval 0.5000s)  beats: 120  (0.1s)
beat grid: tight — 100% of beats matched, residual mean 0.0 ms / max 0.0 ms (raw autocorrelation 117.5 BPM)
```

**tight** means ≥ 80 % of beats had a clear hit and those hits sit within
15 ms of the grid on average (machine-timed music). **loose** grids (live
drummers, tempo changes, ambient tracks) still drive the cut, but trust
beat alignment less. `vibecut beats <file>` prints the same numbers for a
track before you use it.

### Beat-locked cuts

With `beat_align = true`, every cut lands on a beat of the soundtrack *as
it plays in the output* — not just "clips are a whole number of beats".
Those aren't the same thing: a crossfade overlaps its two clips, so 4 s
clips with 0.4 s fades cut every 3.6 s and drift through the beat. Vibecut
instead gives each clip the exact length that puts the cut after it on the
nearest beat to where the planner meant it:

- **Clip lengths** grow by about half of each adjacent crossfade (a clip
  runs on through half of its outgoing fade) and move by at most half a
  beat to reach the beat, rounded to whole frames. Every cut targets an
  absolute beat, so rounding never adds up: each cut is within half a frame
  of its beat (the most whole-frame cuts can do when beats fall between
  frames). Clip count is sized so the total still matches
  `target_duration`.
- **The intro card** lengthens or shortens by at most half a beat so the
  footage comes in on a beat.
- **Footage limits.** A clip that would read past the end of its source
  file slides its window earlier; if it can't, a speed-ramped clip drops
  its ramp (the ramp reads extra footage), and failing that the clip takes
  a nearer beat.
- **Beat-burst** cuts and cuts after a soundtrack loop seam are locked the
  same way.

The run log says what moved:

```text
beat-lock: 25 of 25 cuts placed on the beat (clip lengths -0.00…+0.36s; 1 window slid earlier, 0 speed-ramps dropped)
  intro card 2.50s → 2.633s so the footage comes in on a beat
```

When the soundtrack is shorter than the video it loops: the track plays
once through its natural intro, then its loudest section repeats, each copy
crossfaded into the next over ~2 s. That crossfade is rounded to a whole
number of beats, so the beats of both copies coincide through it (no
flamming kicks) and the grid runs on unbroken.

### Beat-sync report

After every render with a soundtrack, Vibecut checks where the cuts
actually landed against the beats and prints a summary:

```text
beat sync: 26 cuts vs the 128.00 BPM grid — on the beat
  mean |error| 10 ms (0.3 frames), worst 16 ms; 26/26 within ±3 frames (random placement would average 117 ms)
  audio offset in the render: +0.0 ms (correlation 1.00)
```

It accounts for everything between the plan and the file: each seam's real
crossfade width (cuts are measured at the crossfade midpoint, or at the
first frame after a straight cut), the intro card, the beat-burst's
internal cuts, the smart loop's crossfades, and the
audio track's offset in the encoded file, measured by cross-correlating the
rendered audio against the soundtrack. ±3 frames is the tolerance; the
"random placement" figure (a quarter beat) is what unaligned cuts would
average, so a mean error near it means the cuts aren't following the beat
at all — what you'd see with `beat_align = false`. `--explain` adds a
per-cut table, and the full report is written to the manifest as
`beat_sync`.

---

## `[intro]` and `[outro]` (optional)

Title and end cards. Both share the same shape — `[outro]` just appears
after the body. A card crossfades into the footage with the `[transition]`
default, or cuts straight to it in [hard-cut mode](#transition-optional).

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
font         = "arialbd.ttf"             # file name, family or path; see Fonts below (default: Inter / DejaVu Sans)
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

### Fonts

A text layer's `font` is looked up in this order; the first hit wins:

1. a path to an existing file, absolute or relative to the working
   directory;
2. a file of that name in `$VIBECUT_FONTS_DIR` (one or more folders,
   separated like `PATH`, searched recursively);
3. a file of that name in the system font folders (`C:/Windows/Fonts`
   and the per-user fonts folder on Windows; `/usr/share/fonts`,
   `/usr/local/share/fonts`, `~/.local/share/fonts` on Linux;
   `/Library/Fonts`, `/System/Library/Fonts` on macOS);
4. fontconfig's `fc-match`, where it is installed (Linux, the Docker
   image).

File names match case-insensitively and the extension is optional and
interchangeable: `"Inter-Bold.ttf"`, `"Inter-Bold"` and
`"inter-bold.otf"` all find `Inter-Bold.otf`. Through fontconfig you can
also name a family, optionally with a style: `"Bebas Neue"`,
`"Inter:bold"`, `"Inter Display:style=Black"`. Windows file names such as
`arial.ttf` / `arialbd.ttf` / `times.ttf` / `cour.ttf` map to their
families there, which fontconfig serves with the metric-compatible
Liberation fonts when the originals aren't installed. Verdana, Calibri,
Segoe UI, Impact and Arial Black have no such twin in the Docker image:
they get another face that sets wider or narrower, and the log warns.

With no `font`, a layer uses the first installed of `Inter-Regular`,
`DejaVuSans` and `arial`, then fontconfig's `sans-serif`. A font that
can't be found falls back to fontconfig's closest match, or to that
default, and the render log warns and names the file that stood in. The
match keeps the weight and slant the file name asks for where the
fallback family has them: a missing `Montserrat-Black.ttf` gets DejaVu
Sans Bold, not its regular face. If nothing at all is found, a warning
says so and ffmpeg picks, which fails on ffmpeg builds without
fontconfig.

The Docker image ships these families (see [DOCKER.md](DOCKER.md#fonts-and-models)):

| Family | File names | |
|---|---|---|
| Inter | `Inter-Thin` … `Inter-Regular` … `Inter-Black`, plus `…Italic` | The default face. |
| Inter Display | `InterDisplay-Thin` … `InterDisplay-Black`, plus `…Italic` | Inter tuned for large sizes. |
| Bebas Neue | `BebasNeue-Thin`, `-Light`, `-Book`, `-Regular`, `-Bold` | Condensed all-caps display face. |
| DejaVu Sans / Serif / Sans Mono | `DejaVuSans`, `DejaVuSans-Bold`, `DejaVuSerif`, `DejaVuSerif-Bold`, `DejaVuSansMono`, `DejaVuSansMono-Bold` | |
| Liberation Sans / Serif / Mono | `LiberationSans-Regular`, `-Bold`, `-Italic`, `-BoldItalic` (same for Serif, Mono) | Stand in for Arial, Times New Roman, Courier New. |

Your own fonts go in `<projects>/fonts` (the image's `$VIBECUT_FONTS_DIR`)
and are named by file name.

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

Per-source weights and excluded ranges tilt the picker for the next
render; the feedback actions write them, and hand edits work too:

```toml
[[selection.source_weights]]
source_id = "0320"                       # GoPro source id (trailing digits of the file name)
weight    = 0.5                          # × the source's score curve; 1.0 = unchanged, clamped to [0, 10]

[[selection.exclude]]
file  = "GX010320.MP4"                   # chapter file name (or suffix)
start = "12s"                            # chapter-local range the picker must avoid
end   = "17s"
```

`weight = 0` takes the source out (it only fills slots no other source
can). "Less of this clip" halves the
weight (down to 0.125, never 0 — the source still appears, just less) and
"more of" doubles it (up to 8), so one undoes the other. "Drop this
moment" adds an `exclude` for the footage that played there.

Source ids in `source_weights`, `[[multi_angle.groups]]` (`master`,
`members`, `offsets` keys) and the "more of / less of clip …" feedback
match ignoring letter case — `source_id = "dji_0001"` weights the clip
`DJI_0001.MP4` — and a feedback step keeps an existing key spelled as it
was written. No two sources ever differ only in case (such ids clash and
are renamed, above).

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
default                = "fade"          # any ffmpeg xfade name; "cut" / "none" = straight cuts (see below)
duration               = "400ms"         # default: 400ms
palette                = ["fade", "dissolve", "circleopen"]   # currently informational
# --- Speed-ramp ("whip-up"): the last X ms of the outgoing clip plays at
#     Y× speed before the cut, with an optional GoPro-Quik flash. ---
speed_ramp_probability = 0.0             # 0..1; 0 = never, 0.4 = "some, like a typical reel"
speed_ramp_duration    = "350ms"         # length of the SOURCE speed-up portion (default: 350ms)
speed_ramp_factor      = 2.5             # how fast — 1.0 = off, 2.5 = typical reel, 3.0+ = aggressive
speed_ramp_flash       = true            # GoPro-Quik fade-to-white at the seam (default: true)
```

**Hard-cut mode.** `default = "cut"` (or `"none"`, or `duration = "0s"`)
joins the clips with straight cuts instead of crossfades; nothing else is
dropped:

- `[intro]` / `[outro]` cards in `mode = "card"` are still their own clips,
  with their own layer fades, joined to the footage by a straight cut.
- `[project].fade_in` / `fade_out` still fade the whole video from and to
  black, cards included.
- Speed-ramps still play their sped-up tail (and flash); the cut after it
  is a plain cut.
- With `[soundtrack].beat_align`, every cut — intro card to footage, clip to
  clip, footage to the outro card — lands on a beat. A straight cut has no
  crossfade to sit in the middle of, so the beat lock puts the next clip's
  first frame on the beat, and the intro card still moves by at most half a
  beat to get there.

The body is re-encoded in one pass (as on the crossfade path), with every
clip held to exactly the frames the manifest `timeline` gives it; the
timeline records the card spans with `transition_secs = 0`, and the
beat-sync report scores all of those cuts.

The "punchier" and "calmer" feedback leave `duration` alone in hard-cut
mode (they only shorten or lengthen crossfades), so a `duration = "0s"`
project stays on straight cuts; the rest of each action still applies.

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

Multi-camera angle planning. Off by default.

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

**HDR and 10-bit footage** (automatic, no key). Every clip is conformed
to 8-bit BT.709 SDR. The probe reads each file's bit depth, transfer
and primaries, and the run log lists the clips that get converted:

- **HLG** (`arib-std-b67`, GoPro "HLG HDR") and **PQ** (`smpte2084`,
  HDR10) on BT.2020 are tone-mapped: `zscale` linearises (100-nit
  reference), converts BT.2020 → BT.709 primaries, `tonemap=mobius`
  rolls the highlights off, `zscale` re-applies the BT.709 curve.
- **SDR on BT.2020 primaries** is gamut-mapped to BT.709 with `zscale`.
- **10-bit BT.709** (GoPro 10-bit, Flat, Log) only drops to 8-bit —
  same pixels and cache files as before. Log / Flat footage is
  SDR-tagged, so it is *not* auto-graded: put the camera's
  Log → Rec.709 LUT in `lut` above.
- **Fallback**: if ffmpeg lacks `zscale` or `tonemap`, the native
  `colorspace` filter converts primaries and matrix to BT.709 and reads
  the HDR curve as BT.2020 gamma — brightness about right for HLG but
  colours still muted, flat and dim for PQ. A warning is logged. With
  neither filter, HDR clips are left as-is (washed out). `zscale` needs
  an ffmpeg built with zimg; the Debian and Ubuntu packages (dev
  container, Docker image) are.

The conversion happens before `lut`, so a creative LUT grades HDR clips
the same way as the rest. Each chapter's probed colour info is recorded
in the manifest (`sources[].chapters[].color`).

---

## ML signals

There's no separate `[ml]` section anymore — the pipeline always tries to
load any model files it finds and gracefully skips ML behaviour when
they're missing.

- **Model location**: `[project].models_dir` (default `"models"`), then
  `$VIBECUT_MODELS_DIR`, then `/app/models` (the Docker image's bundled
  Silero VAD v6.2).
- **Tuning knobs**: live with the feature they affect (e.g.
  `[soundtrack].vad_speech_weight` controls speech-driven duck-window
  selection).
- **Runtime**: ONNX Runtime probes CUDA EP first and falls back to CPU
  automatically. No configuration needed.

To enable speech-driven ducking outside Docker, drop `silero_vad.onnx`
(from [snakers4/silero-vad](https://github.com/snakers4/silero-vad),
`src/silero_vad/data/`, v5 or later) into your `models_dir`. To disable
any ML feature, remove the corresponding model file (or move it
elsewhere); for the model the Docker image bundles, set
`[soundtrack].vad_speech_weight = 0` instead.

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
  height, fps, crf, lut, stabilize, speed-ramp params)`, plus the colour
  conversion for HDR / BT.2020 clips only (SDR clips keep their old
  hashes). Toggling any of these forces re-encode of the affected
  segments only. Visual analysis
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
  effective settings — plus, with a soundtrack, the `beat_sync` report
  (see [Beat-sync report](#beat-sync-report)), and with an intro or outro,
  the `card_windows` title spans a platform version keeps safe (see
  [Platform presets](#platform-presets-preset)). Useful for postprocessing,
  audits, or driving external tooling. Its `timeline` records exactly
  where everything plays, frame for frame, as the concat stage built it:
  the intro/outro card spans, and per clip (one entry per beat-burst cut)
  its output start/end frame and seconds, the visible cut in and out
  (mid-crossfade), the source file and window read, the speed-ramp tail,
  and the crossfade in and out. Ducking, the beat-sync check and the
  "drop this moment" feedback all read the same layout.
