# Guided Config Editor

The web UI's Config step has two tabs: **Guided** and **Raw TOML**. Guided is a step-by-step wizard bound directly to the same `Config` struct the Rust pipeline deserializes — every field maps 1:1 to a TOML key, but you never see or write TOML syntax. Raw TOML is the original hand-editable textarea, still there for anyone who wants direct access.

Both tabs edit the same file. Save from either one and the other refreshes to match — there's no "guided config" and "raw config", just one file viewed two ways.

## Why it exists

The MCP/LLM workflow never required knowing TOML — you'd describe what you want and the model would write the config. But the web UI's Config step originally *was* a TOML textarea, which meant anyone using Vibecut without an LLM client still had to learn the schema to change anything beyond the six Quick Feedback buttons. The guided editor closes that gap: every option the pipeline supports (with one exception, see [What's not here](#whats-not-here)) is reachable through form controls — sliders, selects, add/remove lists — organized into eight steps.

## The eight steps

### 1. Basics

Project name, target duration, FPS, seed, video fade in/out, and the output list (path, platform, fit, resolution, encoder, quality, audio bitrate, encoder preset). Outputs are a real list — add more than one and the body renders once in the master shape, then transcodes to each; a second output is typically the same cut at a different delivery size, so a new row starts by cloning the last one's settings. Picking a platform (YouTube, Reels / TikTok / Shorts, Square, WhatsApp) clears that row's resolution, quality and audio bitrate so the platform preset decides them, and the fit (blur fill, crop or black bars) says how a different aspect is filled; see [CONFIG.md](CONFIG.md#platform-presets-preset).

### 2. Sources

Three ways to extend or override what `discover_sources` found automatically: explicit video files (alternative or addition to folder scanning), manual time-range overrides for a specific file, and still images animated into clips with a Ken Burns-style motion effect (8 variants, or `auto` to pick deterministically from the project seed).

### 3. Selection & pacing

Clip length bounds, segment order (chronological, energy-arc, or strict-chronological), variety (how much the picker shakes up its choices between renders at the same seed), per-source weights (boost or drop a specific source's clips), and excluded time ranges (windows the picker must avoid). Weights and excludes are the same thing the `MoreOf`/`LessOf`/`DropSegment` feedback actions write — editable directly here instead of only through those coarse actions.

### 4. Transitions

Default transition and duration, plus the speed-ramp ("whip-up") controls: probability (how many seams get ramped), ramp duration, speed factor, and the Quik-style brightness flash. A vibe-specific note explains whether ramping fits the current vibe's default character — several vibes (documentary, family chronicle, memory keeper, drone cinematic) never ramp by default, though the controls still work if you turn it on.

### 5. Soundtrack

Toggle a soundtrack on or off; when on, an ordered list of audio files (multiple files concatenate into one virtual track before beat-detection), beat-align, beat-burst mode, duck count, ducking level, and speech-duck weight. For vibes whose whole character depends on voice ducking (documentary, family chronicle), speech-duck weight renders as a labeled slider with an explanation instead of a plain number field.

### 6. Intro / outro

Enable/disable each card independently. Duration, mode (solid card vs. overlay on the first/last selected segment), background color, and a real layer list — any number of text or image layers, each with its own content or image path, size or height, color or opacity, position, x/y offset, and optional appear-at/disappear-at timing for staged reveals (e.g. a title line that fades out before a subtitle fades in).

### 7. Multi-camera

Enable multi-angle grouping, choose a sync method (auto, creation-time, audio cross-correlation, or manual), minimum hold time before switching angles, and explicit angle groups (master camera + comma-separated members). When sync is manual, each group gets its own per-source offset editor.

### 8. Advanced

The eight scoring weights that decide what counts as an interesting moment (HiLight tags, accelerometer peaks, GPS speed, faces, motion, sharpness/exposure floors, diversity), and the stabilization toggle.

## Vibe-aware guidance

The schema is identical no matter which vibe you proposed — there's one `Config`, not six. What changes is which fields the wizard calls out: a short note grounded in that vibe's own template explains things like why a vibe doesn't ramp by default, or that DJI footage usually has nothing to duck for. Propose a different vibe and the same steps stay in the same order, but the notes and defaults shift to match.

## What's not here

Two things are deliberately absent, for different reasons:

- **`[[text]]` (top-level lower-thirds)** — not exposed on purpose. It's dead in the pipeline: `run_pipeline.rs` parses it, counts it, and prints `"N text overlays (not yet rendered)"` to the CLI banner, then never reads a single field from it again. No code path renders one. A wizard control for a field that silently does nothing would be worse than not having the control — so it stays out until (if) it's actually implemented. This is unrelated to `[intro]`/`[outro]`'s layer list above, which is fully wired and already in the wizard.
- **`schema_version`** — always `1`, nothing to configure.

Everything else in the schema is reachable from one of the eight steps above.

## Tips

- The **Save** button is disabled until something actually changed — dirty-tracking compares a snapshot taken after the last load or save, not on every keystroke.
- Switching to Raw TOML always shows the current saved state, including whatever the guided editor last wrote — useful for confirming exactly what a form control produced, or for the handful of things (multiple output files beyond resolution/encoder tuning, per-segment excludes typed by hand, anything unusual) that read more naturally as text.
- A config that doesn't validate — an empty required field, `min_clip > max_clip`, an empty outputs list — fails to save with the same error `apply_feedback` and the CLI would give; the guided editor doesn't add extra validation of its own; it relies on the same `Config::validate()` every other write path uses.
