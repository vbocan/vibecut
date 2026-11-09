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
    -v /run/desktop/mnt/host:/host:ro,rslave \
    -v "C:/Users/vboca/vibecut-projects:/projects" \
    -v vibecut-cache:/cache \
    vibecut:latest
```

(`/run/desktop/mnt/host:/host` is Docker Desktop's view of every host
drive, for **Browse your drives…**; see below. Leave it out to keep the
server to `/footage`.)

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

## Auto-reel: watched footage folders

Plug in an SD card, or copy a trip into the footage folder, and Vibecut
makes the reel by itself and tells you when it's ready. Off by default.

**Turning it on.** `VIBECUT_WATCH=1` switches it on at start; the
**Auto-reel** switch in the web UI's top bar switches it at runtime. The
switch is remembered (in the state file below) and wins over
`VIBECUT_WATCH` from then on.

**What it watches.** `/footage` plus any folders in `VIBECUT_WATCH_DIRS`.
In each of them:

- every folder directly inside is one **trip** (`/footage/Alps Ski Day`);
- every folder with a camera card's layout — a `DCIM` folder holding DCF
  folders like `100GOPRO`, `101GOPRO` or DJI's `100MEDIA` — is one
  **card**, whose clips under `DCIM` make the reel (the watched folder
  itself counts too, for a card mounted straight at it).

**When it renders.** Every `VIBECUT_WATCH_INTERVAL` seconds it looks
again. A new trip or card waits ("Waiting for the copy to finish") until
every file in it has kept the same size and time for
`VIBECUT_WATCH_SETTLE` seconds, and while a downloader's partial file
(`*.part`, `*.crdownload`, rsync's hidden `.GX010001.MP4.xxxxxx`) is in
it. Then it proposes a project with what the Create screen would pick —
the vibe from the footage (no sound anywhere: drone, high-frame-rate
clips: action, else family), about a tenth of the footage as length
(30–90 s), the auto-picked song — and renders it as an ordinary job,
one at a time and only once no other render is running. The reel is
`/projects/out/<folder>.mp4` (a card's: `<card>_<date>.mp4`), never over
another video (`_2`, `_3`, … when the name is taken), and shows up in the
Renders list like any render; Stop works the same way.

**What it leaves alone.** The first time it runs it only takes note of
what is already there (footage that arrives while it is switched off is
picked up when it is switched back on, four folders at most — see
below); a folder with no clips yet, like an empty `Import`, becomes a
trip once clips arrive. That first look waits until a
watched folder exists, so a missing mount doesn't make the whole library
"new" once it appears. Likewise, more than four new trips or cards
turning up in a single look are a library appearing (a mount fixed, a
share back), not a copy: they are noted as already there, not rendered
one by one. Folders that already have a project (a `/projects/*.toml`
whose `[input].folder` is that folder) are listed as skipped ("Make it
anyway" renders them); the projects the watcher wrote itself don't
count. A trip folder is handled once: clips added to it later make no
new reel (make one from Create). A trip folder deleted and made again
with other clips — a recurring `Import` folder — is new footage; the
same clips moved away and back are not. A card is handled once per set
of clips, so the same card plugged in again is ignored, but brought back
with new footage it makes a new reel, at the same mount point every time.
Dismissing a folder that is still being copied holds for the rest of
that copy.

**State.** `/projects/.vibecut-watch.json` records every folder handled
(and the switch), so a restart never renders a folder twice; a render
cut short by a restart is made again, at most twice in all. Delete the
file to start over: the next run takes a new baseline.

**Notifications.** In the web UI, Create shows a **New footage found**
list (waiting, up next, cutting with progress, ready, failed, skipped),
and a finished reel pops a toast with its poster frame and beat score
that opens it in the Studio — plus a browser notification when the tab
is in the background and you allowed them (browsers only offer this on
`https://` or `localhost`). Elsewhere: set `VIBECUT_WATCH_WEBHOOK` and
each finished reel is POSTed there as JSON:

```json
{
  "event": "reel_ready",
  "id": "w3f9c…",
  "title": "Coastline Bike Ride · 2026-11-09",
  "message": "Your reel “Coastline Bike Ride · 2026-11-09” is ready: 30 s, 12/12 cuts on the beat.",
  "folder": "Coastline Bike Ride",
  "output": "/projects/out/coastline_bike_ride.mp4",
  "output_url": "http://nas.local:8090/api/output?path=/projects/out/versions/…mp4",
  "duration_secs": 30.03,
  "beat_sync": { "tempo_bpm": 132.0, "n_cuts": 12, "within_tolerance": 12,
                 "mean_abs_error_ms": 8.1, "summary": "12/12 cuts on the beat" },
  "poster_url": "http://nas.local:8090/api/output?path=/projects/out/versions/…poster.jpg",
  "poster_path": "/projects/out/versions/…poster.jpg",
  "app_url": "http://nas.local:8090/?watch=w3f9c…",
  "finished_at": "2026-11-09T10:15:00+00:00"
}
```

The `*_url` fields need `VIBECUT_PUBLIC_URL` (the address your phone
reaches the web UI at); without it they are `null`. `app_url` opens the
reel in the Studio. Point it at a Home Assistant webhook trigger, an
ntfy topic (`message` reads well as the notification text) or a small
relay for Telegram. Only that URL is ever called: redirects are not
followed, each POST times out after 10 s, and a failure is retried once,
30 s later, only when there was no answer or a 5xx. Logs show the
webhook's scheme and host, never its path or query (where tokens live).

| Variable | Default | Purpose |
|---|---|---|
| `VIBECUT_WATCH` | off | `1` / `true` / `on`: watcher on at start (the UI switch wins once used). |
| `VIBECUT_WATCH_DIRS` | — | More folders to watch, `:`-separated, absolute (e.g. where cards are mounted). |
| `VIBECUT_WATCH_INTERVAL` | `7` | Seconds between looks (1–600). |
| `VIBECUT_WATCH_SETTLE` | `20` | Seconds a folder must stay unchanged before it is rendered. |
| `VIBECUT_WATCH_VIBE` | from the footage | A fixed vibe key for watched reels. |
| `VIBECUT_WATCH_LENGTH` | from the footage | A fixed length (`45s`, `1m30s`). |
| `VIBECUT_WATCH_WEBHOOK` | — | http(s) URL that gets each finished reel as JSON. |
| `VIBECUT_PUBLIC_URL` | — | Base URL for links in the webhook (`http://nas.local:8090`). |

SD cards: the host mounts them, so bind-mount the host's removable-media
folder with mount propagation, so cards inserted later appear inside the
running container — for example
`-v /media/$USER:/cards:ro,rslave -e VIBECUT_WATCH_DIRS=/cards`.

**Routes.** `GET /api/watch` (switch, watched folders, the list),
`PUT /api/watch` with `{"enabled": true|false}`,
`POST /api/watch/{id}/dismiss` (a waiting or queued folder is then not
rendered, however its copy goes on) and `POST /api/watch/{id}/retry`
(a dismissed copy still in progress waits for the copy again), both
with a JSON body (`{}`),
and `GET /api/watch/events`, a server-sent event stream: `snapshot` (the
`GET /api/watch` body, first and after any gap), then `status`, `entry`,
`removed` and `reel_ready`.

## Browsing your drives

The Create screen's **Browse your drives…** finds footage anywhere on the
computer — a trip folder on `D:`, a card reader's `E:` — without copying it
into `/footage`. The host's file system is bind-mounted **read-only** at
`/host`; Vibecut lists it one folder at a time and reads the videos in the
folder you pick. Renders, configs and logs still go to `/projects` only.

| Host | Mount | What the browser shows |
|---|---|---|
| Windows (Docker Desktop, WSL2) | `-v /run/desktop/mnt/host:/host:ro,rslave` (the `compose.yml` default) | `C:`, `D:`, `E:` …, paths as `D:\GoPro\Alps` |
| Linux | `VIBECUT_HOST_DRIVES=/` (or `/media`, `/mnt`, `/home`) → `-v /:/host:ro,rslave` | one **Computer** root; with all of `/` mounted, paths read as on the host (`/home/you/Videos`). A narrower mount shows paths relative to it: name it with `VIBECUT_BROWSE_ROOTS` (below). |
| macOS | `VIBECUT_HOST_DRIVES=/Users` (or `/Volumes`) | one **Computer** root |

`/footage` stays a root of its own ("Footage"). Docker Desktop's own
`wsl` / `wslg` folders, hidden folders (`.git`, `.Trash…`) and Windows
system folders (`$RECYCLE.BIN`, `System Volume Information`) are never
shown, symbolic links are never followed out of a drive, and a whole drive
can't be used as a footage folder — pick the folder on it. The last five
folders picked come back as **Recent** cards in the trip list (stored in
the browser). The dialog is keyboard-driven: arrows move, Enter opens a
folder, Backspace goes up, Escape closes.

**Settings.**

| Variable | Default | Purpose |
|---|---|---|
| `VIBECUT_HOST_DRIVES` | `/run/desktop/mnt/host` | (`compose.yml` only) Host folder mounted at `/host`. |
| `VIBECUT_BROWSE_ROOTS` | `auto` | `auto`: every one-letter folder in `/host` is a drive (`/host/d` → `D:\`), or all of `/host` is one root when it has none. `off`: `/footage` only. Or a `;`-separated list of `auto` and `label=/container/path[=shown path]` entries, e.g. `auto;Card=/cards`, or `Home=/host/home/me=/home/me` so paths show as on the host. |
| `VIBECUT_ALLOWED_HOSTS` | — | Host names the web API answers to besides `localhost`, IP addresses and names without a dot (`nas.local,vibecut.lan`), comma-separated. `VIBECUT_PUBLIC_URL`'s host counts too. |

**Turning it off.** `VIBECUT_BROWSE_ROOTS=off` hides the drives (the mount
stays, but nothing lists or maps it). To not mount them at all, point
`VIBECUT_HOST_DRIVES` at an empty folder, or drop the `/host` entry in a
copy of `compose.yml`. The server starts and runs the same without `/host`:
browsing then offers `/footage` alone.

**Keep the web UI on `127.0.0.1`.** With the drives mounted, anyone who
can reach port 8090 can list every folder name on them and read any video
in them. `compose.yml` binds the ports to `127.0.0.1` by default
(`VIBECUT_WEB_BIND`); don't change that to a LAN address on a machine with
`/host` mounted unless you trust everyone on that network. The web API
(`/api/*`) also refuses requests whose `Host` is a DNS name other than
`localhost` (`403`), so a web page can't reach it through DNS rebinding;
IP addresses and names without a dot (`http://mybox:8090`) are fine. If
you open Vibecut by a dotted name (`http://nas.local:8090`), add it to
`VIBECUT_ALLOWED_HOSTS` (the host of `VIBECUT_PUBLIC_URL` counts already).

**New drives and cards.** The drive list is read fresh each time the
dialog opens, so it shows whatever `/host` holds at that moment. Whether a
drive or SD card the host mounts *after* the container started shows up
there depends on Docker Desktop passing the new mount through (`rslave`
asks for that); this hasn't been verified on every setup. If a card you
just inserted is missing, restart the container (`docker compose restart
vibecut`).

**Music from any drive.** The song step's **Pick a music folder…** opens
the same dialog, counting songs instead of videos. The folder you pick
joins the music library where it is (nothing is copied): its songs are
listed with yours, can be previewed, and renders read them from the
drive. Vibecut remembers the folders in
`/projects/.vibecut-music-folders.json`; **Add music → Also reading →
Remove** takes one out again. A whole drive can't be picked. **Add music**
also lists sites with free songs you may use in videos (Pixabay Music,
YouTube Audio Library, Free Music Archive, Incompetech, Mixkit, Uppbeat),
each with what its licence asks, and shows the music folder's host path
when `VIBECUT_HOST_PROJECTS` is set (`compose.yml` sets it from an
absolute `VIBECUT_PROJECTS`).

**Routes.** `GET /api/browse/roots` and `GET /api/browse?path=…`; see
[CONFIG.md](CONFIG.md#footage-from-any-drive). MCP: `browse_folders`.
Music folders: `POST /api/music/folders` `{"path": "D:\\Music"}` and
`POST /api/music/folders/remove` `{"path": …}`; `GET /api/music` marks
them `"picked": true`.

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
| `/host` | Optional: the host's drives, **read-only**, for Browse your drives (see above). |
| `/projects` | Generated TOMLs, rendered outputs, conversation logs. |
| `/cache` | Persisted visual-analysis cache; speeds up re-renders. |

The servers keep every path inside these mounts. Paths passed to a
tool or endpoint (config, output and manifest paths) must be under
`/projects`. Footage folders may be relative to `/footage`, a folder from
Browse your drives (`D:\GoPro\Alps`), or an absolute path under
`/footage`, `/projects` or a browse root. A config rendered by the server
must write under `/projects` (`[[outputs]].path`, `[project].temp_dir`)
and read from `/footage`, `/projects`, a browse root, the music library
(`VIBECUT_MUSIC_DIRS`) or a watched folder (`VIBECUT_WATCH_DIRS`): that
covers the input folder and files, soundtrack, intro and outro images,
LUT and `models_dir`. Fonts may also come from `/usr/share/fonts`.
Anything else is refused with a 400 naming the setting. Use absolute
paths inside a config: the pipeline opens a relative path against the
server's working directory (`/` in the image), not the config's folder,
so `out/final.mp4` would mean `/out/final.mp4` and is refused. The
`vibecut` CLI is not restricted.

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
| `plan_edit(config_path)` | The edit before the encode, in seconds: the exact output timeline (each clip's source window, output start/end, crossfades, ramps), the beats as they will play, the length, a source time per clip for thumbnails. While a render of the config runs, that render's own plan. (Web: `POST /api/plan`.) See CONFIG.md, "Drafts and the plan". |
| `render(config_path, quality?)` | Run the vibecut pipeline in-process. Blocking; takes 1–10 min. `quality: "draft"` makes a fast 540p preview beside the final (`<stem>.draft.mp4`). |
| `render_async(config_path, quality?)` | Same as `render` but returns a `job_id` immediately. Pair with `render_status`. |
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
