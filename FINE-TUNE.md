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
| Titles | The opening and closing cards (`[intro]`, `[outro]`): see [Titles](#titles) below |
| What matters | What Vibecut looks for when it picks moments: jumps, speed, spins, bumps, movement, faces, HiLight tags, a mix of clips, and how strictly it skips blurry or dark shots (`[scoring]`) |
| Export | Quality Good / Better / Best (`outputs.0.crf` 23 / 20 / 17) and size 720p / 1080p / 4K (`outputs.0.resolution`) |

Each control says what it does, shows the vibe's setting (a yellow mark on sliders) and, once changed, offers a Reset. A group with changes has a dot on its tab.

## Titles

The **Titles** group (or **Titles** next to Shortcuts in the Studio) edits the opening and closing cards.

- **Opening / Closing**: add a card ("Add an opening card" starts from the vibe's own card, or a plain dark one) or remove it.
- **The card**: show it as a card on a solid background, or over the first (last) shot (`mode = "card" | "overlay"`); background colour (`bg_color`); length, fade in and fade out (`duration`, `fade_in`, `fade_out`).
- **Text and images**: each line of text (`[[intro.layers]]`, `kind = "text"`) has its text, font (the fonts in the image: Inter, Inter Display, Bebas Neue, DejaVu, Liberation), size, colour, position (the seven anchors the renderer knows: top left / centre / right, centre, bottom left / centre / right), a nudge left/right and up/down (`x_offset`, `y_offset`; picking a new anchor clears them), when it appears and disappears (`appear_at`, `disappear_at`; left unset at the card's edges) and an optional box behind it with its colour and opacity (`background.color` as `#RRGGBBAA`). Image layers already in the config get position, height and opacity. "Add a line of text" puts a new, smaller line under the last one.
- **Preview**: the card at the video's shape, laid out the way the renderer lays it out (20 px from the edges, 40 px under bottom text), with a time slider that shows lines appearing and fading. The render has the exact fonts.

Each change is saved into the TOML in place; whole cards and lines are written as `[intro]` / `[[intro.layers]]` tables, the way a person writes them.

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
