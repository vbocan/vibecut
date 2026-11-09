# Vibecut MCP + Web UI — Docker

All-in-one image that ships the vibecut binary, ffmpeg with NVENC,
ONNX Runtime, the Rust MCP server, and the web UI with its
REST/WebSocket API. One container, two ways in:

- Connect Claude Desktop or LM Studio over MCP (`:8080`); the LLM
  does the rest.
- Open the web UI directly in a browser (`:8090`) for hands-on
  control — sources, vibe picker, config editor, render, preview with
  an interactive timeline, and history — no LLM client required (see
  [README.md](README.md)'s Architecture section for the full design).

The traditional CLI workflow (no MCP, no LLM, just `vibecut run
my_project.toml`) still works — see [scripts/](scripts/) for
PowerShell helpers and the **CLI workflow** section at the bottom.

## Build

From the project root:

```bash
docker build -t vibecut:latest .
```

Image size lands around 3 GB once. Subsequent builds are layer-diffed.

## Run

```bash
docker compose up -d
```

Set `VIBECUT_FOOTAGE` and `VIBECUT_PROJECTS` env vars first so
the bind-mounts point at the right host paths (defaults are relative
`./footage` and `./projects`).

Or directly:

```bash
docker run -d --rm --name vibecut \
    --gpus all \
    -p 127.0.0.1:8080:8080 \
    -p 127.0.0.1:8090:8090 \
    -e ANTHROPIC_API_KEY \
    -v "E:/footage:/footage:ro" \
    -v "C:/Users/vboca/vibecut-projects:/projects" \
    -v vibecut-cache:/cache \
    vibecut:latest
```

The MCP server listens on `http://localhost:8080/`; the web UI on
`http://localhost:8090/` (see **Web UI** below). `-e ANTHROPIC_API_KEY`
forwards the host's env var through if set — optional, only needed for
the web UI's free-text brief feature.

## Web UI

Open `http://localhost:8090/` in a browser. Sources → Vibe → Config →
Render → Preview → History, all in one page, talking to the same
pipeline the MCP tools use — see [README.md](README.md)'s Architecture
section for what each step does.

The free-text brief box ("Ask AI") calls the Anthropic API directly
from the container and needs `ANTHROPIC_API_KEY` set in the
container's environment (`compose.yml` passes it through from the host
env var of the same name if present; `docker run` needs `-e
ANTHROPIC_API_KEY` as shown above). Without it, that one box returns a
clear error — everything else in the UI works regardless.

## Test the server in isolation

Two options, both LLM-free:

**Headless HTTP smoke** (Python stdlib only) — validates that the
container starts, the MCP `initialize` handshake succeeds, all nine
tools register, and `discover_sources` returns a clean structured
response on an empty mount:

```bash
docker run -d --rm --name vibecut-smoke -p 18080:8080 vibecut:latest
python src/vibecut-mcp/http_smoke.py --call-discover
docker stop vibecut-smoke
```

**Real edit on real footage:**

```bash
python src/vibecut-mcp/run_edit.py --gpus --vibe action_highlight --footage "E:/footage"
```

Drives `discover_sources` → `propose_vibe` → `render` end-to-end.

**Interactive UI** (`mcp-inspector`, requires Node):

```bash
npx @modelcontextprotocol/inspector http://localhost:8080/
```

## Wire it up to a client

### Claude Desktop

`claude_desktop_config.json` (Windows: `%APPDATA%\Claude\claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "vibecut": {
      "url": "http://localhost:8080/"
    }
  }
}
```

After editing, restart Claude Desktop. The container must already be running.

### LM Studio

**Settings → Model Context Protocol → Add server.** Use the same URL
form: `http://localhost:8080/`.

## Volumes

| Container path | Purpose |
|---|---|
| `/footage` | Source GoPro / DJI footage. Mount **read-only**. |
| `/projects` | Generated TOMLs, rendered outputs, conversation logs. |
| `/cache` | Persisted visual-analysis cache; speeds up re-renders. |

A timeline export (Share → **Open in an editor**, `GET /api/export`; see
CONFIG.md, "Open in another editor") points Final Cut / Resolve at the
footage and music by path. Inside the container those are `/footage/…` and
`/projects/…`; pass the host folders as `VIBECUT_HOST_FOOTAGE` and
`VIBECUT_HOST_PROJECTS` (absolute paths, e.g. `-e
VIBECUT_HOST_FOOTAGE=E:/footage`) and the exported timeline uses the host
paths instead. `compose.yml` fills them from `VIBECUT_FOOTAGE` /
`VIBECUT_PROJECTS` when those are absolute.

`/projects/fonts` and `/projects/models` are optional drop-in folders on
the projects mount; see below.

## Fonts and models

Title cards and speech-driven soundtrack ducking work out of the box: the
image carries the fonts and the voice-activity model they need.

**Fonts.** Installed from Ubuntu packages, with fontconfig:

| Family | Package | Notes |
|---|---|---|
| Inter, Inter Display | `fonts-inter` | Thin … Black, with italics. Inter Regular is the default card face. |
| Bebas Neue | `fonts-bebas-neue` | Condensed all-caps display face (Thin, Light, Book, Regular, Bold). |
| DejaVu Sans / Serif / Sans Mono | `fonts-dejavu-core` | The default when Inter is missing. |
| Liberation Sans / Serif / Mono | `fonts-liberation` | Metric-compatible with Arial, Times New Roman and Courier New, so a config written on Windows with `font = "arialbd.ttf"` keeps its layout. |

A text layer's `font` can name a file (`"Inter-Black.otf"`; the extension
is optional), a family (`"Bebas Neue"`, `"Inter Display:bold"`) or a
path. [CONFIG.md](CONFIG.md#fonts) has the lookup rules. `docker exec
vibecut fc-list : family style` lists what's installed. For your own
fonts, put the `.ttf` / `.otf` files in `<projects>/fonts` and name them
by file name: that folder is `$VIBECUT_FONTS_DIR` and is searched first.

**Silero VAD.** `/app/models/silero_vad.onnx` is Silero VAD v6.2 from
[snakers4/silero-vad](https://github.com/snakers4/silero-vad), fetched at
build time from the v6.2.3 release commit and checked against the size
and git blob id GitHub lists for it (the pinned values are build args in
the Dockerfile's `models` stage, with how to bump them). Its MIT licence
sits next to it as `/app/models/LICENSE.silero-vad`, with a provenance
note in `NOTICE.silero-vad`. The build needs to reach
`raw.githubusercontent.com` for this.

Renders look for the model in `[project].models_dir`, then
`$VIBECUT_MODELS_DIR`, then `/app/models`, and log which one they used at
info level ("Silero VAD model found"). To use another copy, put
`silero_vad.onnx` in `<projects>/models`. To keep ducking from favouring
speech, set `[soundtrack].vad_speech_weight = 0`.

| Variable | Image default | Purpose |
|---|---|---|
| `VIBECUT_FONTS_DIR` | `/projects/fonts` | Extra font folders (`:`-separated), searched before the system fonts. |
| `VIBECUT_MODELS_DIR` | `/projects/models` | Extra model folders (`:`-separated), searched after `[project].models_dir` and before `/app/models`. |
| `VIBECUT_HOST_FOOTAGE` / `VIBECUT_HOST_PROJECTS` | `$VIBECUT_FOOTAGE` / `$VIBECUT_PROJECTS` | Absolute host folders behind `/footage` and `/projects`, for the paths in a timeline export (see Volumes). |

Missing folders are skipped. `compose.yml` sets both to these container
paths itself rather than reading them from your shell, where the same
names may hold host paths for native renders (a `D:\Fonts` there would
replace the drop-in folder and match nothing in the container). To point
them elsewhere, override `environment:` in a `compose.override.yml`, or
pass `-e` to `docker run`.

## CPU-only fallback

Drop `--gpus all` from the docker invocation. The pipeline detects the
missing encoder and falls back to libx264 — slower (typically 4–8× per
render on commodity hardware) but bit-identical to a `crf`-matched
NVENC output for our purposes.

## Dev loop

Rebuilding inside the image on every Rust change is slow. The
`compose.dev.yml` overlay bind-mounts the host-built binaries (all
three — `vibecut`, `vibecut-mcp`, `vibecut-web`) over the
baked-in copies:

```bash
# Build once on the host (requires Linux build target — use WSL on Windows).
cargo build --release

# Then iterate without rebuilding the image.
docker compose -f compose.yml -f compose.dev.yml up
```

For frontend iteration, rebuilding the image on every `src/vibecut-ui/`
change is even slower — `npm run dev` against a running container is
the faster loop:

```bash
docker compose up -d          # vibecut-web now reachable on :8090
cd src/vibecut-ui
npm install                   # first time only
npm run dev                   # proxies /api to localhost:8090; see vite.config.ts
```

Open the URL Vite prints (`http://localhost:5173` by default) instead
of `:8090` while iterating — that's the dev server with hot reload,
not the container's baked-in static build.

## Tools exposed

| Tool | Purpose |
|---|---|
| `discover_sources(folder)` | Walk a footage folder, group GoPro chapters, return per-source rollup. In-process — calls `vibecut::media_probe` directly. |
| `list_vibes()` | Catalog of curated vibes (key, name, rationale, default target duration). |
| `propose_vibe(folder, vibe, ...)` | Render a vibe template into a TOML, write it under `/projects`. Six vibes available: `kid_reel`, `family_chronicle`, `drone_cinematic`, `action_highlight`, `documentary`, `memory_keeper`. |
| `render(config_path)` | Run the vibecut pipeline in-process. Blocking; takes 1–10 min. |
| `render_async(config_path)` | Same as `render` but returns a `job_id` immediately. Pair with `render_status`. |
| `render_status(job_id)` | Poll a background render job: queued / running / done / failed / cancelled. |
| `cancel_render(job_id)` | Stop a background render: a queued job never starts; a running one has its ffmpeg processes killed and reports `cancelled` within about a second. The previous output is left untouched. (Web: `POST /api/jobs/{job_id}/cancel` with a JSON body, `{}`; `GET /api/jobs` lists recent jobs, each with its `kind`: `render`, or `reformat` for a Share-sheet platform version, which stops the same way.) |
| `apply_feedback(config_path, action)` | Mutate a TOML in response to user feedback (six action variants: scale_duration, calmer, punchier, different_music, reroll, drop_segment). |
| `inspect_output(output_path)` | Curated 12-frame overview of a rendered mp4 as inline JPEG content. |
| `inspect_at(output_path, timestamps)` | Frames at specific output-time positions. |
| `export_timeline(output_path, format, fps?)` | The render's edit as a timeline for another editor — `fcpxml` (Final Cut Pro, DaVinci Resolve) or `otio` (OpenTimelineIO) — written next to the render as `<stem>.fcpxml` / `<stem>.otio`. (Web: `GET /api/export`, a download.) |

Each tool returns a JSON-serialisable string (or `Content::image`
entries for the `inspect_*` tools); the LLM client stitches them into
a conversation.

## CLI workflow (no MCP)

The vibecut binary's CLI hasn't gone anywhere. If you'd rather
hand-author a TOML and render directly, that path is still supported.

```bash
# On the host (requires Rust toolchain).
cargo run --release -p vibecut -- run my-project.toml
cargo run --release -p vibecut -- probe E:/footage --json
cargo run --release -p vibecut -- validate my-project.toml

# Inside the container (uses the same binary the MCP server calls).
docker run --rm --gpus all \
    -v "E:/footage:/footage:ro" \
    -v "$PWD/projects:/projects" \
    vibecut:latest \
    /usr/local/bin/vibecut run /projects/my-project.toml
```

The PowerShell helpers in [`scripts/`](scripts/) wrap these for the
common cases:

| Script | What it does |
|---|---|
| `scripts/build.ps1` | `cargo build --release` |
| `scripts/run-cli.ps1 <config.toml>` | Render a TOML with the host binary |
| `scripts/probe.ps1 <path>` | Probe a file or folder with `vibecut probe` |
| `scripts/validate.ps1 <config.toml>` | Dry-run validate a config |
| `scripts/build-image.ps1` | `docker build -t vibecut:latest .` |
| `scripts/serve-mcp.ps1` | Bring up the MCP server in detached mode |
