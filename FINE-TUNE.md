# Fine-tune

**Fine-tune** (top bar, or **D** in the Studio) is the web UI's advanced page. It edits the project's TOML file directly, through controls grouped by what you want to change, with the video and a plan of the edit on screen.

## What's on the page

- **The video** you last rendered, so you can see what you're tuning.
- **The edit as set now.** After every change the server plans the edit (`POST /api/plan`) without rendering: one block per shot, the number of shots, the length and the song's tempo. "Render draft" (or "Render" with Fast previews off) makes the video.
- **Changes from the vibe.** Every setting the project changed from the vibe it was made from, with the project's value, the vibe's value and a **Reset**. **Reset all** goes back to the vibe as it comes.
- **Controls**, in seven groups:

| Group | Settings (TOML keys) |
|---|---|
| Story | Length (`project.target_duration`), order (`selection.order`), fade in / out (`project.fade_in`, `project.fade_out`) |
| Rhythm | Shortest / longest shot (`selection.min_clip`, `selection.max_clip`), follow the music's energy and how strongly (`soundtrack.energy_drives_pacing`, `soundtrack.pacing_strength`), shake it up between renders (`selection.variety`), whip-ups and their flash (`transition.speed_ramp_probability`, `transition.speed_ramp_flash`) |
| Music | The song (opens **Change the music**), cut on the beat (`soundtrack.beat_align`), how far and how often the music dips for voices (`soundtrack.ducking_db`, `soundtrack.duck_count`, `soundtrack.vad_speech_weight`), beat burst, best moment on the drop, musical ending, music fade-out |
| Look | Transition between shots and its length (`transition.default`, `transition.duration`), steady shaky shots (`grading.stabilize`) |
| Titles | Opening / closing text and card length (`intro.layers.0.content`, `intro.duration`, and the same for `outro`), when the project has those cards |
| What matters | What Vibecut looks for when it picks moments: jumps, speed, spins, bumps, movement, faces, HiLight tags, a mix of clips, and how strictly it skips blurry or dark shots (`[scoring]`) |
| Export | Quality Good / Better / Best (`outputs.0.crf` 23 / 20 / 17) and size 720p / 1080p / 4K (`outputs.0.resolution`) |

Each control says what it does, shows the vibe's setting (a yellow mark on sliders) and, once changed, offers a Reset. A group with changes has a dot on its tab.

## The TOML view

**TOML** shows the same file as text. Use it for what the controls don't show: extra video files, time ranges, still images, several cameras, more outputs, fonts and card layers. [CONFIG.md](CONFIG.md) describes every key. **Save TOML** checks the file before writing: one that doesn't load or validate is refused with the reason, and the file on disk is left as it was.

## How changes are written

A control sends one change to `POST /api/config/patch`:

```json
{ "config_path": "/projects/proposal_trip.toml",
  "changes": [{ "key": "selection.max_clip", "value": "5s" }] }
```

Keys are dotted paths; a number is an array index (`intro.layers.0.content`). `"value": null` removes the key, so the built-in default applies. The file is edited in place with a format-preserving TOML editor: comments, key order, alignment and keys you added by hand stay as they were. The result must still load and validate, or nothing is written (a 400 says why).

`GET /api/config/defaults?config_path=…&vibe=…` returns the vibe's settings and the list of changes; without `vibe`, the closest vibe is used, as for a style code.

Configs written by the server (feedback, structured saves) now write numbers the way people type them: `0.6`, not `0.6000000238418579`.
