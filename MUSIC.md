# Music in Vibecut

Every cut lands on the beat, so every video needs a song. Vibecut can't
bundle the tracks people usually download from royalty-free sites (their
terms forbid redistribution), so music works in three stages:

1. **Bring your own** (now): drop downloads into a folder; Vibecut
   analyses them, picks one per video and keeps track of their licences.
2. **Starter pack** (next): a small set of vetted CC0 / CC BY tracks that
   ship with Vibecut. The loader support and folder layout below exist;
   the audio doesn't yet.
3. **Vibecut Originals** (later): commissioned music licensed for any use.

---

## Where music comes from

| Folder | Listed as | Notes |
|---|---|---|
| `/projects/music` | `yours` | Your own downloads. Inside the Docker setup this is the `music` folder in your projects folder (`VIBECUT_PROJECTS`, `./projects` by default). |
| each entry of `VIBECUT_MUSIC_DIRS` | `library` | Extra, shared or bundled libraries. OS path-list syntax (`:` on Linux, `;` on Windows); absolute paths only. |
| `<any of the above>/starter/` | `starter` | The starter pack — see [below](#the-starter-pack). |

MP3, WAV, M4A, AAC, FLAC, OGG and Opus files are picked up, up to four
folders deep; names starting with `.` are skipped. Each track goes through
the same analysis a render uses (fitted beat grid, energy curve, loud
region), so the BPM and mood the library shows are what the render sees.
Results are cached in memory and in `/projects/.vibecut-cache`
(`*.music.json`, keyed by path, size and modification time), so a large
folder is analysed once rather than on every server start.

`GET /api/music` returns every track with its analysis, licence, credit
line and metadata source, plus `import_dir` (where downloads go),
`dirs` and `warnings` (metadata files that couldn't be used). The web UI's
**Add music** panel in the song picker shows the same information.

---

## "Picked for you"

A proposal made without a soundtrack (`POST /api/propose`, the MCP
`propose_vibe` tool) gets one from the library, so the first render
always has music. The config says why, in a comment above `[soundtrack]`,
and the result carries the pick:

```toml
# Picked for you: Full Send Night Drive. 132 BPM sits in Action highlight's
# 120–175 BPM range, its driving feel suits the vibe, and at 3:12 it covers
# the whole 30 s cut.
[soundtrack]
file              = "/projects/music/Full_Send_Night_Drive.mp3"
beat_align        = true
duck_count        = 2
...
```

The rest of the section is the vibe's own soundtrack settings (ducking,
beat-burst, fade-out). How a track is scored:

| | Weight | |
|---|---|---|
| Tempo | 0.35 | Inside the vibe's BPM band scores fully; half or double time inside it scores 0.75 (cuts land on every other beat); outside, the score falls off over 25 BPM. |
| Length | 0.25 | At least as long as the cut scores fully; a shorter track that **loops cleanly** — a tight beat grid and a loud section of at least 8 s, which the engine's smart loop repeats on whole beats with a beat-aligned crossfade — scores 0.75; anything else scores by how much of the cut it covers. |
| Energy | 0.20 | The track's mean loudness relative to its peak against the vibe's energy band. |
| Mood | 0.10 | The library's mood word (chill / warm / upbeat / driving) against the vibe's preferred moods. |
| Beat | 0.10 | Tight beat grid > loose grid > none. |

| Vibe | BPM | Energy | Moods |
|---|---|---|---|
| `kid_reel` | 100–135 | 0.35–0.80 | upbeat, driving, warm |
| `family_chronicle` | 85–120 | 0.25–0.65 | warm, upbeat, chill |
| `drone_cinematic` | 60–100 | 0.10–0.50 | chill, warm |
| `action_highlight` | 120–175 | 0.45–1.00 | driving, upbeat |
| `documentary` | 60–100 | 0.05–0.45 | chill, warm |
| `memory_keeper` | 70–110 | 0.20–0.60 | warm, chill |

Tracks within 0.04 of the best score are equally good; the proposal's
`seed` chooses among up to three of them. The same vibe, length, seed and
library always give the same song; a new seed may give another.

- `soundtrack = "<path>"` in the propose request uses that file instead.
- `soundtrack = "none"` proposes without music (the clips keep their sound).
- `GET /api/music/pick?vibe=…&target_duration=…&seed=…` returns the pick a
  proposal with those settings would make — the web UI shows it as
  "Picked for you" before anything is proposed, and still lets you choose
  another song.

The propose result has `soundtrack` (what the config uses: path, title,
`picked_by` = `vibecut` or `you`, `reason`), `auto_pick` (what Vibecut
would pick, even when you chose your own) and `music_note` (why there's no
music, when there isn't).

---

## Track metadata and licences

Vibecut reads who made a track and under what terms from optional JSON
next to the music — never from the audio file. Most specific first:

1. A sidecar: `song.mp3.json` or `song.json` next to `song.mp3`.
2. The folder's `library.json`: an entry in `tracks` for the file, then
   folder-wide defaults (any field at the top level).

```json
{
  "licence": "Pixabay",
  "source_url": "https://pixabay.com/music/",
  "tracks": [
    { "file": "Full_Send_Night_Drive.mp3", "title": "Full Send", "artist": "Example Artist", "bpm": 132 }
  ]
}
```

| Field | |
|---|---|
| `title` | Display title (default: from the file name). |
| `artist` | Who made it. |
| `licence` (or `license`) | `CC0-1.0`, `CC-BY-4.0`, `CC-BY-SA-4.0`, `CC-BY-NC-4.0`, `Pixabay`, `YouTube-Audio-Library`, `custom`, or any other text (treated as a custom licence). Spelling is forgiving: `CC BY 4.0`, `cc-by-3.0`, `public domain`. |
| `source_url` (or `url`) | Where it was downloaded from. |
| `credit` (or `attribution`) | The exact credit line the site asks for. |
| `bpm` | Tempo the site declares (informational — renders use the detected beat grid). |
| `attribution_required` | `true` / `false` to override whether a post must credit it (e.g. a YouTube Audio Library track marked "attribution required"). |

A missing file means "no metadata". A file that is unreadable, not valid
JSON or has the wrong shape — or a `library.json` entry naming a file
that isn't in the folder — is ignored with a warning (in the server log,
in `GET /api/music`'s `warnings` and in the **Add music** panel). It never
hides the track or fails a render.

What each licence means when you post:

| Licence | Credit | Advice shown |
|---|---|---|
| unknown | — | Fine for personal videos; check the site's terms before posting publicly. |
| CC0 | no | Use it anywhere. |
| CC BY | **yes** | Needs credit — Vibecut adds it to your caption. |
| CC BY-SA | **yes** | Needs credit; share-alike terms may also apply to your video. |
| CC BY-NC | **yes** | Non-commercial only: not for monetised or sponsored videos. |
| CC BY-ND | yes | No adaptations: cutting it into a video may not be allowed. |
| Pixabay | no | Free to use, but some Pixabay tracks are registered with Content ID (the shield icon on the site), so YouTube may flag the video — the licence lets you dispute it. |
| YouTube Audio Library | when marked | Safe on YouTube; terms on other platforms are unclear. |
| custom | when a `credit` is given | Read its terms before posting publicly. |

When credit is required and no `credit` line is given, Vibecut builds one
in the form Creative Commons recommends (title, author, source, licence):

```text
Music: "Night Drive" by Ada (https://example.org/night) — licensed under CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/)
```

The web UI's share caption uses that line for tracks that need credit,
and `Music: <title>` otherwise.

### Credits in the manifest

Every render with music writes a `soundtrack` list to its manifest
(`<output>.manifest.json`), one entry per soundtrack file:

```json
"soundtrack": [
  {
    "file": "/projects/music/night_drive.mp3",
    "title": "Night Drive",
    "artist": "Ada",
    "licence": "CC-BY-4.0",
    "licence_label": "CC BY 4.0",
    "source_url": "https://example.org/night",
    "credit": "Music: \"Night Drive\" by Ada (…) — licensed under CC BY 4.0 (…)",
    "attribution_required": true,
    "sha256": "9f2c…",
    "size_bytes": 5123456
  }
]
```

The SHA-256 identifies the exact file the video used. If a platform raises
a Content ID claim on music you're licensed to use, the manifest gives you
the title, artist, licence, source and file fingerprint to dispute it.
Renders without music don't have the field.

---

## The starter pack

### Layout

```text
<music dir>/starter/
  library.json          metadata for every track (licence is required)
  <track>.mp3 …
```

`<music dir>` is any music folder — `/projects/music` or a
`VIBECUT_MUSIC_DIRS` entry (a Docker image would ship it at, for example,
`/opt/vibecut/music/starter` with `VIBECUT_MUSIC_DIRS=/opt/vibecut/music`).
Tracks under `starter/` whose licence is **CC0** or **CC BY** are listed
with collection `starter` ("Starter pack" in the UI). A track in
`starter/` with any other or no licence is listed as an ordinary track of
its folder, with a warning — the label is only ever earned by metadata.

```json
{
  "tracks": [
    {
      "file": "sunrise_loop.mp3",
      "title": "Sunrise Loop",
      "artist": "Composer Name",
      "licence": "CC-BY-4.0",
      "source_url": "https://…/sunrise-loop",
      "credit": "\"Sunrise Loop\" by Composer Name, CC BY 4.0, https://…/sunrise-loop",
      "bpm": 120
    }
  ]
}
```

### Vetting a track

A track goes into the starter pack only when all of these hold:

- **Licence is CC0 1.0 or CC BY (3.0 or 4.0).** Excluded: CC BY-SA
  (share-alike could reach the user's video), anything NC (non-commercial
  rules out monetised posts), anything ND (a cut-to-the-beat edit is an
  adaptation), GPL and other software licences, and site licences that
  forbid redistribution (Pixabay, YouTube Audio Library, most "royalty
  free" sites).
- **The licence is verifiable at the source**: the track's page states it,
  and `source_url` points there. Keep a copy of the page with the
  submission.
- **The uploader is the author** (or the page names the author and shows
  their licence) — no re-uploads of someone else's work.
- **Not registered with Content ID** or another fingerprinting service, as
  far as the source and a test upload can tell.
- **No uncleared samples** or vocals the author doesn't own.
- **It cuts well**: steady tempo (a tight beat grid in `vibecut beats
  <file>`), at least 60 s or a loud section of 8 s or more that loops
  cleanly, and it fills a gap in tempo or mood the pack doesn't cover yet.

### Adding a track

1. Check it against the list above and record the evidence (source page,
   licence, author).
2. Run `vibecut beats <file>` and note the BPM and whether the grid is tight.
3. Copy the file into `starter/` and add its entry to `starter/library.json`
   with `title`, `artist`, `licence`, `source_url` and, for CC BY, the
   `credit` line exactly as the author asks for it.
4. Restart or refresh the music library and confirm the track shows as
   "Starter pack" with no warnings.
