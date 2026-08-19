# Vibecut: Deterministic Procedural Highlight Reel Pipeline with LLM Agent Integration

[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT_or_Apache--2.0-yellow.svg)](LICENSE)
[![Docker](https://img.shields.io/badge/Docker-Supported-blue?logo=docker)](https://docker.com)
[![Tests](https://img.shields.io/badge/tests-153_passing-brightgreen)](#testing)
[![MCP](https://img.shields.io/badge/MCP-Protocol-purple)](https://modelcontextprotocol.io)
[![NVENC](https://img.shields.io/badge/NVENC-Supported-76b900?logo=nvidia)](https://developer.nvidia.com/video-encode-and-decode-gpu-support-matrix-new)

## Overview

A telemetry-driven procedural pipeline that turns a folder of GoPro / DJI footage into a finished short-form highlight reel — automatically. Vibecut combines per-source telemetry (HiLight tags, accelerometer peaks, GPS speed), audio analysis (beat detection, voice-activity ducking), and visual signals (sharpness, exposure, motion) into a single deterministic score curve, then composes a watchable cut without the user opening a video editor.

The pipeline is wrapped in a Model Context Protocol (MCP) server so an LLM client (Claude Desktop, LM Studio, …) can drive the whole flow conversationally — *"discover the footage I dropped here, propose a vibe, render it, the bit at 0:42 is too dark, drop it, render again."* Same conversation, same output: the system is **deterministic by construction**, because the LLM lives in the *client* and the server is pure tools.

## What it does

The conversation loop, end-to-end:

```
turn 1 (user)  drops folder /footage/summer_trip
turn 1 (LLM)   discover_sources → list_vibes
               "12 sources, 40 min, kid play. Two options: kid_reel or family_chronicle?"

turn 2 (user)  "kid_reel"
turn 2 (LLM)   propose_vibe(kid_reel) → render → inspect_output
               returns mp4 + 12 inline frames + "here's the cut, 95 s."

turn 3 (user)  "less aggressive"
turn 3 (LLM)   apply_feedback(Calmer) → render → inspect_output
               returns updated mp4 + frames + "softer xfades, no flash."

turn 4 (user)  "drop the bit at 0:42"
turn 4 (LLM)   inspect_at(timestamps=[42]) → "yes that's segment 12.
               Drop it?"
turn 4 (user)  "yes"
turn 4 (LLM)   apply_feedback(DropSegment{at_seconds: 42}) → render → done
```

The traditional CLI workflow (`vibecut run my-project.toml` against a hand-authored TOML) still works — the MCP server is layered on top, not in place of, the pipeline.

## Quick Start

### Prerequisites

- Docker 24+ with the NVIDIA Container Toolkit (Linux) **or** Docker Desktop with WSL2 + a recent NVIDIA driver (Windows). CPU-only fallback works without a GPU at ~5× the render time.
- ffmpeg is bundled in the image — no host-side install needed for the Docker workflow.
- Rust 1.80+ if you want to build / iterate on the CLI or the Rust crates directly.
- Node 20+ if you want to iterate on the web UI directly — optional; the Docker build compiles it itself, no host-side Node needed just to run the image.

### Build the all-in-one image

```bash
docker build -t vibecut:latest .
```

The image (~4.7 GB) ships the vibecut binary, ffmpeg with NVENC, ONNX Runtime, the Rust MCP server, the web server, and the built web UI. First build is ~5–10 min (includes an `npm ci` + SvelteKit build stage); subsequent builds are layer-diffed.

### Drop a folder, get a video — through the web UI

No LLM client to configure — open a browser instead.

1. Start the server:

   ```bash
   docker compose up -d
   ```

   (Set `VIBECUT_FOOTAGE` and `VIBECUT_PROJECTS` env vars first to point at your host paths. Set `ANTHROPIC_API_KEY` too if you want the free-text "Ask AI" box to work — everything else works without it.)

2. Open **http://localhost:8090/**. Sources → Vibe → Config → Render → Preview → History, all in one page — see [Architecture](#architecture) below for what each step does.

### Drop a folder, get a video — through Claude Desktop

1. Start the server:

   ```bash
   docker compose up -d
   ```

   (Set `VIBECUT_FOOTAGE` and `VIBECUT_PROJECTS` env vars first to point at your host paths.)

2. Wire up Claude Desktop. Edit `claude_desktop_config.json` (Windows: `%APPDATA%\Claude\claude_desktop_config.json`):

   ```json
   {
     "mcpServers": {
       "vibecut": {
         "url": "http://localhost:8080/"
       }
     }
   }
   ```

3. Restart Claude Desktop and have the conversation above.

LM Studio works the same way: **Settings → Model Context Protocol → Add server →** `http://localhost:8080/`.

### Drop a folder, get a video — CLI only, no LLM

```bash
# Render an example
docker run --rm --gpus all \
    -v "E:/footage:/footage:ro" \
    -v "$PWD/projects:/projects" \
    vibecut:latest \
    /usr/local/bin/vibecut run /projects/my-project.toml
```

Or use the host binary directly via [PowerShell helpers](scripts/):

```powershell
scripts\build.ps1                                    # cargo build --release
scripts\probe.ps1   E:\footage                       # tabular metadata report
scripts\validate.ps1 my-project.toml                 # dry-run schema check
scripts\run-cli.ps1 my-project.toml -RandomSeed
```

See [CONFIG.md](CONFIG.md) for the full TOML schema to hand-author `my-project.toml`.

## Key Features

- **Two ways in, one pipeline.** Hand-author a TOML and run `vibecut run`, **or** drop a folder in front of Claude Desktop and have a conversation. Both paths drive the same Rust pipeline.
- **Telemetry-driven scoring.** GoPro HiLight tags, accelerometer peaks, GPS speed, motion energy, sharpness, exposure, optional Silero VAD. Per-source score curves combine into a multi-source pick set with diversity, energy-arc placement, and seeded variety.
- **Audio-aware composition.** Beat-aligned cuts, smart-loop with natural-intro preservation, VAD-guided ducking around speech, optional rapid-cut beat-burst at the soundtrack's energy peak.
- **GoPro-Quik-style transitions.** Per-pair speed-ramp ("whip-up") effect with brightness warm-up and 67 ms fade-to-white at the seam — visually a kinetic flash.
- **MCP server with 9 tools** (`discover_sources`, `list_vibes`, `propose_vibe`, `render`, `render_async`, `render_status`, `apply_feedback`, `inspect_output`, `inspect_at`). Streamable HTTP transport on port 8080.
- **Six curated vibes.** `kid_reel`, `family_chronicle`, `drone_cinematic`, `action_highlight`, `documentary`, `memory_keeper` — each a hand-tuned TOML template the LLM picks from.
- **Eight feedback variants.** Natural-language reactions ("shorter", "less aggressive", "different music", "drop the bit at 0:42", "more of source X") map to type-checked deterministic config mutations.
- **Multimodal inspection.** `inspect_output` returns 12 evenly-spaced frames inline as JPEG content; `inspect_at` returns frames at user-referenced timestamps. The LLM can see and reason about the actual pixels.
- **Deterministic by construction.** Same `(seed, config)` pair → same picks → same manifest → same output. Validated with a replay harness that hits 100 % bit-identical determinism on multi-turn conversations.
- **Reproducible substrate.** Per-call JSONL session logs at `/projects/sessions/`; replay harness; six-vibe sweep that catches silent pipeline drift in CI.
- **Hardware acceleration.** NVENC inside the container via WSL2 + NVIDIA Container Toolkit. CPU-only fallback (libx264) works without a GPU.

## Performance

| | |
|---|---|
| **Discovery + visual analysis** (12 sources, 40 min) | ~56 s cold, ~0 s warm |
| **Render (warm cache)** | 30–60 s for 90–200 s output, NVENC, 1080p, CRF 20 |
| **Render (cold cache)** | 60–180 s same output |
| **Submit-to-job-ID latency** (`render_async`) | ~0 ms |
| **MCP tool startup** | ~1.5 s (Streamable HTTP, container already up) |
| **Frame extraction** (`inspect_output`, 12 frames @ 480 px) | ~4 s parallel via tokio + ffmpeg |
| **Test suite** | 153 unit tests, ~0.3 s |

Measured on a single workstation with NVIDIA RTX-class GPU + WSL2; numbers scale roughly linearly with source duration.

## Vibe library

Six curated mappings from natural-language intent to a starting TOML. The LLM picks among them via `list_vibes` and `propose_vibe`.

| Key | Aesthetic |
|---|---|
| `kid_reel` | Kinetic family edit, music-driven |
| `family_chronicle` | Chronological, gentle pacing |
| `drone_cinematic` | Long holds, energy-arc, no ramps |
| `action_highlight` | Punchy, every-cut-on-beat |
| `documentary` | Voice-led, ducks on speech |
| `memory_keeper` | Long-form, soundtrack-led |

A compile-time test re-parses every vibe template through `vibecut::project_config::Config` and runs `validate()`, so schema drift breaks the build instead of producing a config the pipeline would reject at runtime.

## Feedback grammar

The user reacts; the LLM picks a `FeedbackAction` variant; the server mutates the config deterministically.

| Variant | Maps to user pattern |
|---|---|
| `ScaleDuration { factor }` | "shorter" / "longer" |
| `Calmer` | "less aggressive" / "calmer" |
| `Punchier` | "more energy" / "punchier" |
| `DifferentMusic { path }` | "different music" |
| `Reroll` | "same but different" |
| `DropSegment { at_seconds }` | "drop the bit at 0:42" — manifest-aware |
| `MoreOf { source_ids }` | "more of [source X]" — boost weight 1.5× |
| `LessOf { source_ids }` | "less of [source X]" — drop weight to 0 |

Each action round-trips through `Config::load → mutate → validate → write`; a cross-cutting test guarantees no action can produce a config the pipeline would reject.

## Architecture

```
┌─────────────────────────────┐  HTTP  ┌────────────────────────────────┐
│ MCP client                  │───────▶│ Docker container               │
│ (Claude Desktop, LM Studio) │  :8080 │   ├── vibecut binary (Rust)    │
│                              │        │   ├── vibecut-mcp (rmcp)      │
│  LLM lives here.             │        │   ├── vibecut-web (axum)      │
│  Server is pure tools.       │        │   ├── vibecut-ui (built, static) │
└─────────────────────────────┘        │   ├── ffmpeg + NVENC           │
                                        │   └── ONNX Runtime             │
┌─────────────────────────────┐  HTTP  │                                │
│ Browser                     │───────▶│                                │
│ (vibecut-ui — no LLM needed) │  :8090 │                                │
└─────────────────────────────┘        └────────────────┬───────────────┘
                                                         │ --gpus all
                                                         ▼
                                         ┌───────────────┐
                                         │ Host GPU      │
                                         │ (WSL2/native) │
                                         └───────────────┘
```

One container, two processes under one `tini` (see [docker/entrypoint.sh](docker/entrypoint.sh)) — not a merged single-port router. MCP and the web UI reach the same pipeline through the same [vibecut-ops/](vibecut-ops/) operations layer either way.

Five crates plus one frontend, all in this repo, all shipped in the Docker image:

1. **Pipeline** ([vibecut/](vibecut/)) — Rust crate. Telemetry → scoring → selection → render → mux → transcode. Library + binary, 89 tests.
2. **Operations layer** ([vibecut-ops/](vibecut-ops/)) — Rust crate. Transport-free async functions (discover, propose, apply_feedback, render, inspect, config read/write, session log/list) that call the pipeline directly — no MCP, no HTTP, no opinion on transport. Every transport below is a thin wrapper around one of these, including the per-call JSONL `SessionLogger` both transports share. 49 tests.
3. **MCP server** ([vibecut-mcp/](vibecut-mcp/)) — Rust crate using `rmcp` 0.10 + axum. 9 tools, Streamable HTTP transport. Ships in the Docker image.
4. **Web server** ([vibecut-web/](vibecut-web/)) — Rust crate using `axum` 0.8. REST + WebSocket over the same operations layer, for direct browser control instead of an LLM conversation — plus a free-text brief endpoint (`POST /api/brief`) that calls the Anthropic API directly, since a browser has no LLM built in the way Claude Desktop does, and it serves the built web UI itself as a static-file fallback. 15 tests. Runs alongside `vibecut-mcp` in the same Docker image and container, on its own port (`WEB_BIND_ADDR`, default `:8090`) — two processes under one `tini`, not a merged single-port router.
5. **Web UI** ([vibecut-ui/](vibecut-ui/)) — SvelteKit 2 + Svelte 5 + TypeScript. Sources → Vibe → Config → Render → Preview → History in one flow, talking to #4 over `fetch`/`WebSocket`. Preview includes a segment-colored, click-to-seek timeline with a per-segment inspector (source, score, reason) and one-click `More of` / `Less of` / `Drop this segment` feedback actions; a free-text brief box (`Ask AI`) stands in for manual vibe-picking or quick-feedback buttons wherever it appears; History has an A/B before/after slider across the session's renders plus a browsable session log. Ships pre-built into the Docker image — open `http://localhost:8090/`, nothing else to run. For frontend iteration, `npm install && npm run dev` against a running container still proxies `/api` to `:8090` (see [DOCKER.md](DOCKER.md)'s dev loop) — set `ANTHROPIC_API_KEY` on the container for the brief box to work.
6. **Client** — outside this repo. Any standard-MCP client (Claude Desktop, LM Studio) for the conversational flow; the web UI for direct browser control.

## Technology Stack

- **Pipeline language**: Rust 1.80+
- **MCP SDK**: [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) 0.10 (Streamable HTTP, server transport)
- **Async runtime**: Tokio 1.x
- **HTTP server**: Axum 0.8
- **Video / audio**: ffmpeg 6.x (NVENC, libx264, libx265, vidstab)
- **ML inference**: ONNX Runtime 2.x with CUDA EP (Silero VAD)
- **Container base**: `nvidia/cuda:12.6.0-runtime-ubuntu24.04`
- **Tests**: 153 (89 pipeline + 49 operations layer + 0 MCP layer + 15 web layer)

## Testing

```bash
cargo test --release            # full workspace
cargo test --release -p vibecut
cargo test --release -p vibecut-ops
cargo test --release -p vibecut-mcp
cargo test --release -p vibecut-web
```

The web UI has no automated tests yet — `npm run check` (TypeScript + Svelte type-checking via `svelte-check`) inside [vibecut-ui/](vibecut-ui/) is the closest thing today.

End-to-end smokes (require Docker):

```bash
python vibecut-mcp/http_smoke.py --call-discover                       # MCP protocol handshake
python vibecut-mcp/run_iteration.py --footage <folder> --gpus          # propose → render → feedback → render
python vibecut-mcp/sweep.py --footage <folder> --gpus                  # all six vibes against one source
python vibecut-mcp/replay.py --log <session.jsonl> --footage <folder>  # determinism replay
python vibecut-mcp/inspect_smoke.py --footage <folder>                 # multimodal frame extraction
python vibecut-mcp/async_smoke.py --footage <folder> --gpus            # render_async / render_status
```

`<folder>` is a path to your own GoPro/DJI footage — none of these scripts ship with a default.

## Citation

If you use Vibecut in your research, please cite:

```bibtex
@software{vibecut2026,
  title  = {Vibecut: Deterministic Procedural Highlight Reel Pipeline with LLM Agent Integration},
  author = {Bocan, Valer},
  year   = {2026},
  url    = {https://github.com/vbocan/vibecut},
  license = {MIT OR Apache-2.0}
}
```

## Documentation

| File | What's there |
|---|---|
| [CONFIG.md](CONFIG.md) | Complete TOML schema reference for hand-authored projects. |
| [DOCKER.md](DOCKER.md) | Docker build / run / wire-up guide. Includes the CLI workflow. |
| [scripts/](scripts/) | PowerShell helpers for CLI, Docker, and web-layer dev workflows. |

## Limitations

- **No user study.** This is a systems contribution; user vocabulary corpus is future work.
- **Vibe library is curated, not learned** — six hand-built entries.
- **Preview's click-to-seek timeline is an approximation** — the manifest doesn't yet record exact per-segment output timing (intro duration, per-seam transition width), so seeking can land a beat off around heavily-transitioned cuts.
- **NVENC determinism is bit-identical within one driver version** — we make no claim across NVIDIA driver upgrades. CPU fallback (libx264) is fully deterministic.

## Roadmap

- Implement the `ExtendSegment` feedback action (pin an exact range) once the picker supports selecting one.
- Persist exact output-timeline layout in the manifest for frame-accurate Preview seeking.
- Verify additional MCP clients beyond Claude Desktop — LM Studio and `mcp-inspector` are next.

## License

This project is dual-licensed under MIT or Apache-2.0; pick whichever fits your downstream. See [LICENSE](LICENSE).

## Author

**Valer Bocan**, PhD, CSSLP

- **Issues**: [GitHub Issues](https://github.com/vbocan/vibecut/issues)
- **Discussions**: [GitHub Discussions](https://github.com/vbocan/vibecut/discussions)
- **Email**: vbocan@gmail.com
