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
quality         = "final"              # optional; "draft" = a fast 540p preview (default: "final")
```

`models_dir` points at a folder that may contain `silero_vad.onnx` (and
future face / scene-detection nets). The pipeline auto-loads any model
present and gracefully skips ML features when files are missing — no
explicit on/off switch. A relative `models_dir` is relative to the
working directory. A model missing there is looked up next in each folder
of `$VIBECUT_MODELS_DIR` (separated like `PATH`), then in `/app/models`,
where the Docker image ships `silero_vad.onnx`; the render log says which
copy it loaded.

### Drafts and the plan (`quality`)

Every render is two phases. The **plan** probes the footage, analyses the
music and the footage (from the cache when it was analysed before), picks
the moments, places the beat-burst and the speed ramps, beat-locks every
cut and lays out the exact output timeline. The **render** then encodes
that plan: cards, clips, the concat, the soundtrack mux and ducking, the
outputs, the beat-sync check and the manifest. The plan doesn't depend on
the quality, so a draft and a final of the same config are the same edit,
cut for cut.

`quality = "draft"` renders that edit as a quick preview:

- the **first output only**, scaled so its short side is at most 540 px
  (960x540 for a 16:9 master, 540x960 for a vertical `reels` output, a
  smaller project keeps its size);
- written **beside the final** as `<stem>.draft.mp4` with its own
  `<stem>.draft.manifest.json` (`"quality": "draft"`, the draft's
  `resolution`), so it never replaces the final's video or manifest;
- every encode at the encoder's fastest preset (`superfast` for x264/x265,
  `p1` NVENC, `speed` AMF, `veryfast` QSV) and CRF 28, without
  stabilization; card titles and logos are scaled with the frame.

The default, `"final"`, renders every output as configured; a final's
manifest carries no `quality` key. A request overrides the key: `vibecut
run --quality draft|final`, the web `POST /api/render` and `POST
/api/render/async` body field `"quality": "draft" | "final"` (anything
else is a 400), and the MCP `render` / `render_async` argument `quality`.
The result of a web or MCP render says which it was (`"quality"`) and
names the draft's files as its `output_path` / `manifest_path`; its kept
copy is `versions/<stem>.draft__<tag>.mp4`. On the demo project (30 s,
20 clips, software x264) a draft takes about 10 s against about 22 s for
the final, and a second draft with every clip cached about 9 s.

**The plan on its own** — `vibecut plan <config>` (prints it as JSON after
a `--- plan ---` line), `POST /api/plan {"config_path": "…"}`, the MCP
`plan_edit` tool — takes seconds on analysed footage and returns:
`duration_secs`, `target_duration_secs`, `fps`, `seed`, the `song` (path,
`tempo_bpm`, `beat_interval_secs`) and its `soundtrack` credits, `beats`
(seconds, as they will play under the video, loop copies included),
`segments` and `timeline` in exactly the manifest's shape (the render
writes the same timeline), and one `thumbnails` entry per timeline clip
(`source_path`, `t_secs` in the source, `output_secs`) for `GET
/api/frames/at`. The web answer adds `config_path`, `elapsed_ms` and
`planned_by`: while a render of the same config text runs (or ran in the
last ten minutes), the request waits for that render's own plan rather
than planning the footage a second time (`"render"`); otherwise it plans
it (`"plan"`). The web UI shows this plan as a storyboard — the shots on
the beat grid, with thumbnails — while the render runs.

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
file    = "GX010023.MP4"                   # filename (suffix) or path ending; see [selection]
start   = "1m12s"
end     = "1m30s"
caption = "March"                          # optional lower third over this clip
```

**Ranges.** Every `[[input.ranges]]` plays on top of the picker's picks.
`file` names the footage by file name (any letter case) or a suffix of
it (`"0023.MP4"`). When two folders hold files of the same name — GoPro
numbers repeat from card to card — give a path instead
(`"/footage/alps/GX010023.MP4"`, or just `"alps/GX010023.MP4"`): it is
matched against the trailing components of the file's path, whole
components only. A `caption` draws a small lower third — white text on
a translucent box, centred in the lower third of the frame, sized to it
(4.5 % of the height) — over the clip that plays the range, for about
2.4 s from just after its incoming crossfade. A clip too short to read
it gets none; when two clips play parts of one range, the one that plays
more of it carries the caption.

**Exactly these moments: `ranges_only`.** With `ranges_only = true`
under `[input]` the render plays the ranges and nothing else, in the
order they are listed: the picker adds no clips, `[selection].order`
is not applied, and there is no beat-burst or multi-angle pass — so no
footage analysis either, which makes such a render quick. Each range's
file must still be found: list it under `[[input.videos]]` (or point
`folder` at it). With a soundtrack and `beat_align`, the clip lengths
are beat-locked as in any render, and speed ramps and crossfades work
as usual. A `ranges_only` config without a range (or an image) fails
validation, and a render whose ranges match no video stops with an
error. Generated configs such as the [year recap](#year-recap) use it.

"Drop this moment" in the app (the `drop_segment` feedback) adds a
`[[selection.exclude]]` entry for what played; when that moment came
from a range, it also removes the range — the picker's excludes never
applied to ranges. In a `ranges_only` config, whose list is the playing
order, a dropped range's caption moves to the next range unless that
one has its own (so a recap's month keeps its caption).

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

Each `path` is written as `<name>.partial-<pid>-<n>.<ext>` next to it while
the render runs and only renamed into place once the whole render has
succeeded, so a failed or stopped render (the web UI's **Stop**,
`POST /api/jobs/{id}/cancel`, the MCP `cancel_render` tool) leaves the
previous video and its manifest exactly as they were. The staging name is
the writer's own, so a render and a `reformat` that write the same file at
once (see below) don't touch each other's partial file; a partial file left
by a process that died is removed by the next writer. Every committed file
is dated by the moment it lands, which is what makes a reformat's "is the
version newer than the render?" check hold.

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
fit    = "blur"                           # blur (default) | smart | crop | pad; only used when the aspect differs
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
| `portrait`                  | 4:5           | 1080x1350  | 20  | 192k  | none              |
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
- `smart` ("Follow the action"): a crop that fills the output and moves
  to keep the action in shot — see below.
- `crop`: a centre crop that fills the output (a 9:16 crop of a 16:9
  frame keeps its middle third).
- `pad`: the whole frame with black bars.

**`fit = "smart"`** measures where things move in the finished video and
slides the crop window along with it; no model, no download. The video is
decoded once more, small (192 px wide, 10 frames a second, about 2–3 s for
a 30 s 1080p render). For each pair of frames the camera's own movement
is estimated and taken out, and what still moves — above the frame's
motion floor — decides where the window goes (detail breaks ties). The
window then follows like a camera operator would:

- it snaps at every cut (the manifest's exact `timeline`, plus hard cuts
  it sees in the pictures) instead of panning across one;
- a shot with no clear subject (clear in under 30 % of its frames)
  keeps the centre crop, and one whose subject stays within a dead zone
  (15 % of the window) of one place is locked off;
- otherwise it holds until the subject leaves the dead zone, pans after
  it on a critically damped spring — never faster than 0.6 windows a
  second, looking 0.4 s ahead so it doesn't trail — and holds again once
  the subject settles; moments without motion hold the last position.

The path is drawn by the crop filter's per-frame position (a few linear
pieces, at most 200 however long the video, so the ffmpeg command stays
small). A 9:16, 1:1 or 4:5 output from a 16:9 master slides left and
right; a wider output from a vertical master slides up and down. Titles
are treated as with `crop` (below). The render log says how many shots
it followed (`smart reframe: followed the action in 10 of 21 shots`).

**Titles in vertical outputs.** Intro/outro cards are rendered once, at
the body's resolution (their `size` and offsets are master pixels), and
reach other shapes through the reframe. In a portrait output narrower
than the master, the frame is shown at 76 % of its fitted width during
the intro and outro — so a title as wide as the master frame stays inside
the 9:16 safe area, clear of the platform's side buttons — and eases back
to full width over 0.4 s after the title. A `crop` or `smart` output shows
the blur fit during those spans instead, so a title is never cut. The
spans are recorded in the manifest as `card_windows`.

**A platform version of a finished render**, without re-rendering: `vibecut
reformat out/edit.mp4 --preset reels` (optionally `--fit smart|crop|pad`,
`--out <path>`) writes `out/edit.reels.mp4` in one ffmpeg pass with the same
reframe, reading `card_windows` (and, for `smart`, the cuts in `timeline`)
from the render's manifest. A fit other than the default is part of the
name: `out/edit.reels-smart.mp4`. `--show-path` prints where a smart crop
sits every half second and how steady it is (cuts snapped, largest move
between frames, pan reversals). The web UI's Share sheet (`POST
/api/reformat`, body `{ "output_path", "preset", "fit"?, "wait"? }`) and
the MCP `reformat` tool do the same; the Share sheet asks for `smart`
("Follow the action") for its vertical, square and 4:5 versions, with
"Blur fill" one click away, and shows the render and the version side by
side at the moment the crop sat furthest from the centre. A smart
version's result carries the path it followed as `smart`: `{ "axis": "x",
"window": 0.3164, "shots", "followed_shots", "pieces": [[t0, t1, c0, c1],
…] }`, the window's centre as a share of the render's frame, linear from
`t0` to `t1` (seconds); it is `null` when an up-to-date version was reused.
A smart version spends the first quarter of its progress finding the
action.
Given a version instead (`out/edit.square.mp4`), it works from the render
next to it (`out/edit.mp4`), so a vertical version of the square one is
still one reframe of the master; if the render is gone, the result keeps
the version's whole name (`out/edit.square.reels.mp4`). Like the render's
outputs, the version is written under a staging name
(`out/edit.reels.partial-<pid>-<n>.mp4`) and renamed into place only when
the pass has finished — and only if the render it read is still the one
there: a render that landed meanwhile (say one with `out/edit.reels.mp4`
as an extra output) means the pass runs again over the new cut. A version
counts as up to date (and is reused) while it is at least as new as its
render. In the web UI each version is
a job of kind `reformat` in the same queue as renders (`GET /api/jobs`,
the Renders list), stopped the same way (`POST /api/jobs/{id}/cancel`,
which keeps the last good version); asking again for a version that is
still being made joins that job instead of starting a second pass.

### Open in another editor (timeline export)

A finished render can also leave as an *edit*: a timeline of the original
clips to finish, grade or re-time in Final Cut Pro, DaVinci Resolve or any
OpenTimelineIO tool. The web UI's Share sheet has **Open in an editor**
(Final Cut / Resolve → `.fcpxml`, OpenTimelineIO → `.otio`) for the
version on screen. Behind it:

```text
GET /api/export?output_path=out/coastline.mp4&format=fcpxml   (or otio)
                [&fps=29.97]
```

`output_path` is a render (`output_path` or `version_path`), one of its
platform versions or its manifest, under `/projects`. The answer is a
download (`Content-Disposition: attachment`, named after the video:
`coastline.fcpxml`); 400 for a path outside `/projects`, an unknown
`format` or `fps`, or a render without a `timeline`; 404 when there is no
manifest. The MCP `export_timeline` tool (`output_path`, `format`, `fps`)
writes the file next to the render's manifest instead and returns its path.

It is built from the manifest's `timeline`, so it is the cut you watched:

- **Frame-exact.** The sequence runs at the render's `fps` (`fps=29.97`,
  `25` or `30000/1001` picks another; NTSC rates are exact fractions, so
  FCPXML times read `17017/30000s`). Cards and clips run cut to cut and add
  up to the sequence length exactly. Source times sit on the source's own
  frames (probed with ffprobe, so 29.97 GoPro footage stays on its 29.97
  grid) after its timecode; music times on its samples. No clip reads
  outside its file, dissolve handles included: where the two frame grids
  (or the last frame the render held at the end of a chapter) would run a
  clip past the end, its in point slips back by that fraction of a frame;
  a music piece ends on the frame before. A file shorter than that (not
  the one the render read) is declared as long as the edit needs and
  noted in the summary.
- **Cuts and dissolves.** The render cuts mid-crossfade, often half-way
  through a frame; the export puts each cut on whichever of the two frames
  either side is nearer the beat, and centres an SMPTE / Cross Dissolve on
  it inside the frames the render blended (so no dissolve needs media the
  render didn't read). Beat-burst cuts stay hard cuts.
- **Speed ramps** become the clip at normal speed plus its sped-up tail
  (FCPXML: a constant-speed `timeMap`; OTIO: `LinearTimeWarp`).
- **Title cards** become gaps named after the card's text, with a marker
  carrying it (title generators differ per editor; a gap imports
  everywhere). Overlay titles become range markers.
- **Music** follows the layout the mux used: loop copies (crossfaded in
  OTIO, spliced at the crossfade's middle in FCPXML), and several joined
  `files` split back into their own files. OTIO puts it on a `Music` audio
  track; FCPXML connects it below the storyline on lane −1 (role `music`).
  Every beat is a marker on the music; the duck windows are range markers
  ("Clip sound up"). In FCPXML the clips' camera sound follows the
  render's mix, which goes by time rather than by clip: −96 dB outside the
  duck windows, full inside them, and volume keyframes where a window
  ramps in or out within a clip — including the neighbours a window runs
  into. Beat-burst cuts stay silent, as in the render. The fade-out is a
  marker too.
- **Not carried over:** the card design (colour, fonts), LUT and
  stabilisation, the speed-ramp flash, the project fade in/out and the
  ducking levels — the editor's own tools do those.

FCPXML is version 1.10 (Final Cut Pro 10.6 or later; DaVinci Resolve imports it too): one
`asset` per file with a percent-encoded `file://` `media-rep`, a `format`
per frame size and rate. OTIO uses the schema versions every OTIO reader
accepts (`Timeline.1`, `Track.1`, `Clip.1` with an `ExternalReference.1`,
`Gap.1`, `Transition.1`, `Marker.1`); cuts are red markers on the video
track, beats green and duck windows cyan on the music track.

**Paths.** Clips point at the files the render read. In Docker those are
container paths (`/footage/…`, `/projects/music/…`): set
`VIBECUT_HOST_FOOTAGE` and `VIBECUT_HOST_PROJECTS` to the absolute host
folders mounted there (`compose.yml` passes `VIBECUT_FOOTAGE` /
`VIBECUT_PROJECTS` through) and the export points at the host files;
otherwise relink in the editor. A manifest from before the music layout was
recorded exports with the music from the start of the track (the summary
notes it); one from before the `timeline` existed can't be exported —
render it again.

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
energy_drives_pacing  = true             # clip lengths follow the song: short in loud parts, long in quiet ones (default: true)
pacing_strength       = 0.6              # how far the song bends clip lengths, 0..1 (default: 0.6)
musical_ending        = true             # end on a phrase / the song's final hit, fade to a bar line (default: true)
hero_on_drop          = true             # best moment starts on the downbeat of the song's drop (default: true)
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
rather than repeat a shot. When the cut is paced to the music (below) the
burst instead fills a loud section of the song — from its first beat, never
past its end — leaves the best moment and the final clip alone, and ends on
one of the plan's cuts, so everything after it stays where the plan put it.

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

### Music-led pacing: bars, phrases and a real ending

With `beat_align = true` Vibecut reads the song's form from the beat grid
and edits to it. `vibecut beats <file>` prints what it found:

```text
bars:        first downbeat 0.625s (beat 0), 2.500s per bar, downbeat confidence 0.48
phrases:     4-bar, at 0.62 10.62 20.62 30.62 40.62 …
section  1:    0.62s –    5.62s  high  intensity 0.76 (-19.9 dB)
section  2:    5.62s –   10.62s  quiet intensity 0.27 (-22.9 dB)
section  3:   10.62s –   15.62s  high  intensity 0.76 (-19.9 dB)  ← drop
ending:      final hit 79.375s, sound ends 79.99s
```

- **Bars.** 4/4 is assumed. The bar's "1" is the beat phase whose beats
  carry the most accent — onset strength, low-band (kick / bass) energy and
  the step up into the beat; the confidence (0–1) says how clearly it
  stands out. Without clear evidence the beat the music starts on is a "1".
- **Phrases** are 4-bar groups (every other one an 8-bar boundary), placed
  where the level steps up most consistently.
- **Sections** start where the level over the next two bars departs from
  the previous two, snapped to a phrase start; each is quiet / mid / high
  against the song's own range. **The drop** is the high section with the
  most level, rise and length (of equally big ones, the first).
- **The final hit** is the last strong onset before the song's tail fades
  out (the sound ends where it drops 40 dB below the music).

What the edit does with it, each part behind its own key:

- **`energy_drives_pacing`** — clip lengths follow the song where it plays
  under them: loud, busy music cuts faster, quiet music holds longer, always
  within `[selection]` `min_clip`–`max_clip` and what each clip's footage
  holds. `pacing_strength` sets the swing: at 1 the loudest music cuts
  ~2.8× faster than average and the quietest holds ~2.8× longer; 0 gives
  even lengths. A loud song also gets more, shorter clips than a quiet one.
  Every cut is still on a beat; a cut to a new source or scene prefers a
  bar line and above all a phrase start, and long holds avoid running
  through a phrase start. The plan is made for the whole video at once, so
  the last quiet stretch gets its hold too. With a narrow clip range the
  swing is narrow too (1.5–4 s is 3–6 beats at 96 BPM): widen
  `min_clip` / `max_clip` for one-beat cuts and multi-bar holds.
- **`hero_on_drop`** — the highest-scoring moment starts on the downbeat
  that opens the song's drop as it plays in the video, and with
  `order = "energy_arc"` the arc peaks there (it is arranged around it).
  When the drop doesn't play in the video, or the clips can't reach it, the
  moment takes the biggest section that does — of equally big ones, the
  one nearest 45 % of the video; a song without sections gets it on a
  phrase start. Other orders never reorder: the best moment keeps its
  place and gets the biggest section its slot can reach.
- **`musical_ending`** — when the song's own ending fits the target length
  (up to 12 % or two bars short, 5 % or a bar long), the song plays once
  through, un-looped, the last cut lands on its final hit and the last
  clip (or the outro card) holds through the ring-out. Otherwise the edit
  ends on the phrase start nearest the target — up to a bar or 8 % early,
  half that late — or else on the nearest bar line, and the music's
  `fade_out`, rounded to whole beats, completes exactly there: at the end
  of a bar. An outro card (card mode) starts on that cut and runs a whole
  number of bars (the nearest to its own length), so the fade completes on
  a bar line after it.

All three are on by default; set any to `false` for the old behaviour of
that part (the song's form is still analysed and recorded). The run log
shows the plan:

```text
bars: 2.500s, 4-bar phrases, downbeat confidence 0.48; 16 sections (high → quiet → …); drop at 10.62s
music-led plan: 11 clips, 3–6 beats each (strength 0.60); last cut on the phrase start at 30.62s, video ends 30.62s
music-led pacing: 11 clips, beats@song intensity: 3@0.75 5@0.27 4@0.33 4@0.79 3@0.76 5@0.28 …
  best moment (clip 4, score 0.325) starts on the drop at 10.62s
```

and the manifest records it as `music`: the bar lines (`downbeats`) and
`phrases` in output seconds, the `sections` as they play (with `level`,
`intensity` and the `drop`), where the best moment went (`hero`, with `on`
= `drop` / `section` / `phrase`), how it ended (`ending`: `kind` =
`song_end` / `phrase_end` / `bar_end` / `beat`, the last cut, the fade and
the final hit) and each clip's length in beats against the song's
intensity under it (`pacing.clips`). The Studio's timeline draws bar lines
heavier than beats, phrase starts heavier still, and the sections as
bands — loud parts lit, the drop labelled.

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
`x_offset` / `y_offset` move a layer from its anchor in both modes,
`card` and `overlay`.

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
gps_speed            = 0.4               # fast stretches, speed bursts, hard braking (GoPro GPS)
gyro                 = 0.4               # spins, flips, carving (GoPro gyroscope)
landings             = 0.7               # jumps: free fall then a landing (accelerometer)
faces                = 0.3               # reserved: no face model is bundled, so unused
motion_peaks         = 0.3               # visual frame-difference MSD
sharpness_floor      = 0.2               # reject bottom 20% by Laplacian variance (0 = no floor)
exposure_floor       = 0.15              # reject bottom 15% by exposure score (0 = no floor)
diversity_per_source = 0.5               # boost segments far from already-picked moments in same source
```

A weight of `0` turns a signal off. Configs written before `gyro` and
`landings` existed get the defaults above.

### Telemetry signals

GoPro cameras record a metadata track (GPMF) next to the video. Each
signal below is used **only when that camera recorded the stream behind
it** — a phone clip, a drone clip or a GoPro with GPS off simply goes
without — and footage with no telemetry is scored exactly as before
(the demo footage's picks are unchanged). `vibecut gpmf <file>` lists
what a file has.

| Signal | Stream | What scores |
|---|---|---|
| `hilight_tags` | HiLight tags (GPMF or the MP4's `HMMT` atom) | a bump at every tag |
| `accel_peaks` | `ACCL` | jolts above the take's steady vibration |
| `gps_speed` | `GPS5` (HERO5–10) or `GPS9` (HERO11+) | half speed (against the take's top speed, never below 18 km/h), half bursts above the ~20 s cruising speed — or sudden deceleration (1.5–5 m/s²), whichever is higher |
| `gyro` | `GYRO` | net rotation rate: 0 below ~30°/s, full at ~230°/s; vibration averages out |
| `landings` | `ACCL` | free fall (< 0.45 g for 0.2–3 s) followed within 0.4 s by a ≥ 1.8 g landing; the bump peaks mid-flight and grows with airtime |
| `motion_peaks` | the video itself | on-screen movement above the local baseline |

GPS samples are dropped when the camera reports no lock (`GPSF` < 2, or
`GPS9`'s per-sample fix) or a dilution of precision above 5 (`GPSP` >
500), and as glitches (a 0,0 position, faster than 360 km/h); short
gaps (< 3 s) are bridged, longer ones score nothing. Every stream is
timed by its own stamps — `STMP` (the camera clock, anchored to the
file), else `TSMP` (so dropped samples leave a gap), else spread evenly
over each one-second payload.

How they combine: the **continuous** signals (`accel_peaks`,
`gps_speed`, `gyro`, `motion_peaks`) are a weighted mean over the ones
the camera has — a missing stream's weight is shared by the rest —
scaled so each take's most eventful moment scores `motion_peaks`. So a
GoPro with GPS and gyro neither outranks a phone clip just for having
more sensors nor loses to it because its sensors rarely all fire at
once; the telemetry decides *which* moments of a take are its best.
HiLight tags and landings are **events**, added on top at their own
weight: they decide what beats a take's best. The sharpness and exposure
floors apply last.

Faces: `faces` is accepted but does nothing yet — it needs a face
detection model, which isn't bundled (and isn't downloaded).

### Scene cuts

When the visual analysis runs (any of `motion_peaks`, `sharpness_floor`,
`exposure_floor` above 0), it also finds **hard scene cuts** (a frame
difference at least 4× the typical one around it and 2× either
neighbour) and **big changes** (a 2.5× jump that comes with a lasting
exposure or sharpness shift: into a tunnel, under water). A picked clip
that would straddle one moves into the scene its middle is in, and a
clip that starts within 1 s after one starts right on it — unless that
would run into another pick from the same take. One continuous recording
split into chapter files is never a cut.

### Why a moment was picked

Each scored pick in the manifest carries `signals`: its strongest
scoring signals (up to three), strongest first, each with its share of
the moment's score and the measured value in plain words — then a
placement note when it opens on a scene cut:

```json
"signals": [
  { "kind": "landing", "share": 0.61, "label": "airtime 0.6 s" },
  { "kind": "accel", "share": 0.26, "label": "3.0 g jolt" },
  { "kind": "scene_start", "share": 0.0, "label": "opens on a scene change" }
]
```

`kind` is one of `hilight`, `accel`, `gps_speed` ("38 km/h", "52 km/h,
top speed", "hard braking from 40 km/h", "hard stop from 31 km/h"),
`gyro` ("fast spin (458°/s)", "spin", "sharp turn", "turning"),
`landing`, `motion` ("lots of movement on screen") and `scene_start`.
Photos, forced ranges, beat-burst cuts and filler picks have none. The
web UI's moment inspector shows them as "why this moment".

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
file  = "GX010320.MP4"                   # chapter file name (or suffix), or a path ending
start = "12s"                            # chapter-local range the picker must avoid
end   = "17s"
```

`weight = 0` takes the source out (it only fills slots no other source
can). "Less of this clip" halves the
weight (down to 0.125, never 0 — the source still appears, just less) and
"more of" doubles it (up to 8), so one undoes the other. "Drop this
moment" adds an `exclude` for the footage that played there.

The `file` of an exclude or an `[[input.ranges]]` entry is compared
ignoring letter case. A bare name (`GX010320.MP4`, or a suffix such as
`0320.MP4`) matches that file in any folder; a path
(`camA/100GOPRO/GX010001.MP4`, `/` or `\`) matches only the files whose
path ends with those whole folders and name. Two cameras' cards both hold a
`100GOPRO/GX010001.MP4`, so "Drop this moment" writes the file's path
relative to `[input].folder` (just the name for a file straight in the
folder, the path as written for an `[[input.videos]]` entry): it drops the
seconds from that camera's footage only. A range whose bare name matches
more than one file uses the first and logs a warning.

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
in the manifest (`sources[].chapters[].color`). The same conversion is
applied to the frames the visual analysis scores (so an HDR clip's
exposure and sharpness are judged on the picture the render shows; the
GPU `scale_cuda` path stands aside for those clips) and to the frames the
web UI and the MCP `inspect_*` tools grab, footage thumbnails included.

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

## Style codes (recipes)

A **style code** shares how a video was made — not what it was made of —
as one short line anyone can paste to get the same look on their own
footage:

```
VC1-gwGhssPU5faoChVGdWxsIFNlbmQgTmlnaHQgRHJpdmUAAgABEqwCIy25
```

In the web UI: **Share this style** in the Studio copies the code (and can
save it by name); **Use a style code** in Create previews what a code sets
and which song it gets from your library, and **Make my video** then makes
the video from it. The MCP server has `share_style`, `describe_style`,
`apply_style` and `list_styles`.

### What a code carries

| Carried | Not carried |
|---|---|
| The vibe it started from | `[input]`: the footage, ranges, images |
| `[project]` `target_duration`, `fps`, `seed`, `fade_in`, `fade_out` | `[project]` `name` (the title), `temp_dir`, `models_dir` |
| `[soundtrack]` everything but the files: beat alignment, ducking, beat-burst, fade-out | `[soundtrack].files` — the song travels as a reference (below) |
| `[intro]` / `[outro]` cards and their text layers | Image layers (a logo is a file on your disk) |
| `[scoring]`, `[transition]` | `[selection]` `exclude` and `source_weights` (per-clip choices) |
| `[selection]` clip lengths, order, variety, `max_per_source` | `[multi_angle].groups` (camera ids) |
| `[multi_angle]` `enabled`, `sync`, `min_hold`; `[grading].stabilize` | `[grading].lut` (a file path) |
| Each `[[outputs]]`: `preset`, `resolution`, `crf`, `audio_bitrate`, `fit`, `max_bitrate` | `path`, `encoder`, `encoder_preset` (machine-specific) |

Card text keeps its style but not the trip: text that is the project's
title becomes `{title}`, dates (`YYYY-MM-DD`) become `{date}`, and the trip
folder's name — alone, or before `" · "` — becomes `{trip}`. Applying the
code fills them in for the new footage (title: the one you give, else
`"<folder> · <today>"`). Other card text ("See you next ride") is shared as
written.

**Music** is the clips' own sound, "let Vibecut pick" (a library song for
the vibe and length), or the song itself, named by its *track id* — a
fingerprint of the file's size and first 256 KiB, so the same file matches
on any machine whatever it's called — with its title, artist and tempo.
Applying a code looks for the same file, then a library song with the same
title (and artist, when both have one), then — when the song isn't there —
a library song within 8 % of its tempo (half or double counts), then
Vibecut's pick for the vibe. The preview and the new config's header say
which, in words. Sharing with **Let Vibecut pick their song** sends
"auto" instead of the song.

### The format

`VC<version>-` + URL-safe base64 of a binary payload and a 24-bit checksum.
The payload names the vibe, the music, and only the settings that differ
from the vibe's own style — paths and common values from two built-in
dictionaries, durations as numbers, floats in their shortest decimal form —
sorted, so the same style always gives the same code. An untouched vibe is
10 characters; a vibe with its length changed and a song about 60; the demo
project (action highlight, 30 s, its song with title and artist) 76; a heavily
tweaked one (hard cuts, a Reels output, a new end card) about 190.

Format 1's settings are differences from the six vibes' styles as they were
when it was defined (`src/vibecut-ops/src/recipe_baselines_v1.json`), so a
later change to a vibe template doesn't change what an old code means. A
code is refused, with a message saying why, when its checksum doesn't match
(a typo, a cut-off paste, an edit), when it is from a newer format (`VC2-…`:
"made by a newer Vibecut"), or when it sets something this version doesn't
know. Whitespace, line breaks and quotes around a pasted code are ignored.
The checksum catches accidents, not forgery; a hand-made code still can't
set a path (footage, output, LUT, temp dir, image layer) — those keys are
dropped before anything is written.

### Saved styles

Named styles live in `/projects/recipes/<name>.json` (`{ "name", "code",
"created_at" }`); saving under an existing name replaces it. Anywhere a
code is accepted, a saved style's name works too.

### Routes

| Route | Does |
|---|---|
| `POST /api/recipes/code` `{config_path, manifest_path?, vibe?, let_vibecut_pick?}` | The code for a project's style, with a summary. `manifest_path` (a render's) fills in a song the library doesn't know from its credit and tempo. |
| `GET /api/recipes/preview?code=` | What a code (or saved name) sets — `style.chips` in words, `style.settings` that differ from the vibe — and `music_match`: the song it gets here and why. |
| `POST /api/recipes/apply` `{code, folder, title?}` | A proposal for `folder` in that style: written to `/projects` and answered like `POST /api/propose` (plus `code`, `style`, `music_match`), ready for `POST /api/render/async`. |
| `GET /api/recipes` | Saved styles, newest first, with any unreadable files in `warnings`. |
| `POST /api/recipes` `{name, code}` or `{name, config_path, …}` | Save a style by name. |

Bad codes answer `400` with `error` and `hint`; an unknown footage folder or
saved name `404`.

---

## Year recap

"Your year" is a reel of the best moment of each month — or of each trip,
when the footage spans fewer than three months — made from the versions
every render keeps (`out/versions/<name>__<tag>.mp4` and its
`.manifest.json`). It is not a second renderer: Vibecut writes an
ordinary project config and renders it like any other.

1. **Moments.** Every kept manifest under `/projects` is read (not the
   recaps' own, in `/projects/recaps`; not hidden folders, `music/` or
   `sessions/`). Each version offers its four best segments by score,
   scored against its own best pick; photos and folded beat-bursts don't
   count, and footage that is gone is skipped. The same file with an
   overlapping range is one moment however many versions picked it (each
   extra version counts a little in its favour). A moment is dated by
   when it was filmed: the file's `creation_time` plus the offset into
   it, else — no date, or a camera clock never set (before 2000) — the
   file's modification time. Never the render's date.
2. **Plan.** The period is a calendar `year`, a `from` / `to` range
   (`2026`, `2026-03` or `2026-03-14`; an open end reaches the first or
   last moment), or by default the latest year with moments. Moments from
   at least three months make one bucket per month; fewer, one per trip
   (the project's name before its ` · date`). A recap needs at least
   three months or three trips. Every bucket gets its best moment, then
   the rest of the length goes round the buckets, the one with the fewest
   picks first; when there are more buckets than the length can hold, the
   strongest make it. The length (default 60 s, kept within 20 s – 5 min)
   sets about 4 s per moment (2.5–6 s), after a 3 s title card. Moments
   play in the order they were filmed (a trip's stay together), each
   window centred on the moment and kept inside its file; two windows on
   one file never share footage. With a song, every moment lasts a whole
   number of its beats, so the beat lock keeps the recap at its length.
3. **Config.** `/projects/recaps/recap_<period>.toml` (`recap_2026.toml`,
   `recap_2026-03_2026-06.toml`) with `ranges_only = true`, one
   `[[input.videos]]` per file and one `[[input.ranges]]` per moment by
   full path, the month (or trip) as the `caption` of its first moment, a
   title card ("2026 in moments", "March – June 2026", or your `title`)
   over "12 moments · 5 months", 300 ms crossfades, and a library song
   auto-picked with the `family_chronicle` vibe's music profile (or
   another `vibe`, a `soundtrack` file, or `"none"`), beat-aligned, with
   no beat-burst. Its render goes to
   `/projects/recaps/out/recap_<period>.mp4` and keeps versions like any
   other. Making the same period again rewrites the file.

**Web API.** `GET /api/recap?year=2026` (or `from` / `to`, and `length`)
answers what a recap would play without writing anything: `plan` (the
period, title, `grouping` — `month` or `trip` — every bucket with how
many of its moments play, the moments in playing order with their
windows and captions, the estimated length, `eligible`, and notes such
as months left out or missing footage) and `years` (each year's months,
trips and moments, and whether that is enough for a recap).
`POST /api/recap` with `{ "year": 2026 }` (or `from` / `to`; optional
`length` in seconds or `"1m30s"`, `title`, `vibe`, `soundtrack`, `seed`)
writes the config and submits its render as an ordinary job: the answer
carries `config_path`, `output_path`, the `plan`, the song, and `job_id`
(`"render": false` writes the config only). Three more fields serve the
Studio's changes to a recap on screen: `exclude`, the windows already
dropped from it (its `[[selection.exclude]]` entries, `{ file, start,
end }`), which stay out and are written into the new config;
`transition`, its `[transition]` table (as Calmer / More energy left
it), written into the new config instead of the default 300 ms
crossfades; and `shuffle`, any number, for another draw of each month's
strong moments (the same number draws the same; a much weaker moment
never leads). Too few months or trips is a 400. The MCP server's `make_recap` tool does
the same (`preview_only` for the plan alone).

**In the app.** The Create screen shows a "Your 2026 recap" card when a
year has enough months or trips: the months (or trips) with a frame of
each, how many moments each gives, which are left out at the chosen
length (45, 60 or 90 s), and "Make my 2026 recap", which renders it into
the Studio. There, Shorter / Longer and Shuffle make the recap again,
keeping its song, the moments you dropped and its transitions; Another
mix does the same but lets the next vibe pick the song; "Drop this
moment" removes that moment's range; and "More / Less of this clip" are
off, as there is no picker to steer.

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
  seed is logged so you can lock it in later). Fresh seeds — this flag and
  the Shuffle feedback — stay below 2^53, so they fit a TOML integer and a
  JavaScript number; a hand-written `seed` can be any value up to 2^63 − 1.
- **Cache fingerprinting.** Each rendered segment is cached in the
  workspace as `seg_<hash>.mp4`, the hash a blake3 of `(source_path,
  in-chapter start, duration, width, height, fps, crf, lut, stabilize,
  speed-ramp params)`, plus the colour conversion for HDR / BT.2020 clips,
  a draft's fast preset and an image's effect seed — each only where it
  applies. The name holds no position: after a tweak, a moment that is
  still in the edit (with the same window, length and ramp) reuses its
  clip wherever it now plays, and only new or changed clips are encoded.
  On the demo project, "Shorter" (30 s → 22.5 s) re-picks the cut and
  re-encodes 10 of its 18 clips; the other 8 come from the cache (a
  position-keyed name would have reused none). Drafts and finals never
  share or replace each other's clips (different size, CRF and preset),
  and nothing is evicted. A clip with an overlay title burnt in is
  `seg_<hash>_titled.mp4` and is encoded every time. Visual analysis
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
  "drop this moment" feedback all read the same layout. With a soundtrack
  the timeline also holds `music` — the files (several play as one joined
  track), one span per copy of the track as the mux laid it (output start
  and end, track time), the loop crossfade, the fade-out and every beat in
  output time — and with titles, `titles` (each card's text and span,
  card or overlay): what the [timeline export](#open-in-another-editor-timeline-export)
  needs to rebuild the edit elsewhere.
