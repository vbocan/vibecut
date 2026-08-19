"""Vibe sweep / case-study driver.

Runs every vibe in the library (or a chosen subset) against the same
footage folder and writes the rendered outputs side by side under
`/projects/sweep/<vibe>.mp4`. Useful for eyeballing all six aesthetics
against the same source in one pass — same seed, so it's reproducible.

Usage:

    python src/vibecut-mcp/sweep.py --footage "E:/footage" --gpus
    python src/vibecut-mcp/sweep.py --footage "E:/footage" --vibes kid_reel,drone_cinematic --gpus

The summary lands at `/projects/sweep/sweep_summary.json` (mounted
back to the host's `out/mcp/sweep/`).
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

PROTOCOL_VERSION = "2025-03-26"

DEFAULT_PROJECTS = "C:/Users/vboca/OneDrive/Playground/Vibecut/out/mcp"

ALL_VIBES = [
    "kid_reel",
    "family_chronicle",
    "drone_cinematic",
    "action_highlight",
    "documentary",
    "memory_keeper",
]


def post(base_url: str, body: dict, sid: str | None = None,
         timeout_s: float = 60.0) -> tuple[dict, str | None]:
    headers = {
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream",
    }
    if sid:
        headers["Mcp-Session-Id"] = sid
    req = urllib.request.Request(
        base_url, data=json.dumps(body).encode("utf-8"),
        method="POST", headers=headers,
    )
    with urllib.request.urlopen(req, timeout=timeout_s) as resp:
        new_sid = resp.headers.get("Mcp-Session-Id") or sid
        ctype = resp.headers.get("Content-Type", "")
        raw = resp.read().decode("utf-8")
    if not raw.strip():
        return {}, new_sid
    if "text/event-stream" in ctype:
        for line in raw.splitlines():
            if line.startswith("data:"):
                payload = line[len("data:"):].strip()
                if payload:
                    return json.loads(payload), new_sid
    return json.loads(raw), new_sid


def initialize(base_url: str) -> str | None:
    resp, sid = post(base_url, {
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": PROTOCOL_VERSION,
                   "capabilities": {},
                   "clientInfo": {"name": "vibe-sweep", "version": "0.1"}},
    })
    if "result" not in resp:
        raise RuntimeError(f"initialize failed: {resp}")
    post(base_url, {"jsonrpc": "2.0", "method": "notifications/initialized"}, sid)
    return sid


def call_tool(base_url: str, sid: str | None, tool: str,
              arguments: dict, timeout_s: float = 900.0) -> dict:
    msg_id = int(time.time() * 1000) & 0xFFFFFF
    r, _ = post(base_url, {
        "jsonrpc": "2.0", "id": msg_id, "method": "tools/call",
        "params": {"name": tool, "arguments": arguments},
    }, sid=sid, timeout_s=timeout_s)
    if "result" not in r:
        raise RuntimeError(f"{tool} failed: {r}")
    content = r["result"].get("content", [])
    text_items = [c for c in content if c.get("type") == "text"]
    if text_items:
        return json.loads(text_items[0]["text"])
    return r["result"].get("structuredContent") or {}


def rewrite_output_path(toml_text: str, new_output_path: str) -> str:
    """Surgically replace the first `path = "..."` line in the TOML —
    that's the `[[outputs]]` entry the vibe templates emit. Surgical
    edit beats round-trip-through-a-TOML-library because we don't
    want to perturb comments / formatting / order in the proposal
    file (helps debugging when something looks off)."""
    return re.sub(
        r'(?m)^(\s*path\s*=\s*)"[^"]+"',
        f'\\1"{new_output_path}"',
        toml_text,
        count=1,
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", default="vibecut:latest")
    parser.add_argument("--footage", required=True, help="Path to your GoPro/DJI footage folder")
    parser.add_argument("--projects", default=DEFAULT_PROJECTS)
    parser.add_argument("--vibes",
                        default=",".join(ALL_VIBES),
                        help="Comma-separated vibe keys. Defaults to all six.")
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--gpus", action="store_true")
    parser.add_argument("--port", type=int, default=18085)
    parser.add_argument("--render-timeout-s", type=int, default=900)
    parser.add_argument("--container-name", default="vibecut-mcp-sweep")
    args = parser.parse_args()

    vibes = [v.strip() for v in args.vibes.split(",") if v.strip()]
    unknown = [v for v in vibes if v not in ALL_VIBES]
    if unknown:
        print(f"WARN: unknown vibe(s) {unknown}; will surface as errors")

    if not Path(args.footage).exists():
        print(f"ERROR: footage path not found: {args.footage}")
        return 2
    Path(args.projects).mkdir(parents=True, exist_ok=True)
    sweep_dir = Path(args.projects) / "sweep"
    sweep_dir.mkdir(parents=True, exist_ok=True)

    base_url = f"http://127.0.0.1:{args.port}/"
    subprocess.run(["docker", "rm", "-f", args.container_name],
                   capture_output=True, text=True)

    docker_cmd = [
        "docker", "run", "-d", "--rm",
        "--name", args.container_name,
        "-p", f"127.0.0.1:{args.port}:8080",
        "-v", f"{args.footage}:/footage:ro",
        "-v", f"{args.projects}:/projects",
        "-v", "vibecut-cache:/cache",
    ]
    if args.gpus:
        docker_cmd[3:3] = ["--gpus", "all"]
    docker_cmd.append(args.image)

    print(f"$ {' '.join(repr(a) if ' ' in a else a for a in docker_cmd)}")
    res = subprocess.run(docker_cmd, capture_output=True, text=True)
    if res.returncode != 0:
        print(f"ERROR: docker run failed: {res.stderr}")
        return 1

    try:
        # Wait for the listener.
        deadline = time.time() + 30
        while time.time() < deadline:
            try:
                urllib.request.urlopen(base_url, timeout=2).read()
                break
            except urllib.error.HTTPError:
                break
            except (urllib.error.URLError, ConnectionError):
                time.sleep(0.5)

        sid = initialize(base_url)
        print(f"server up at {base_url}")
        print()

        results = []
        for vibe in vibes:
            print(f"==== {vibe} ====")
            t_total = time.time()
            try:
                # 1. propose
                t0 = time.time()
                prop = call_tool(base_url, sid, "propose_vibe",
                                 {"folder": "", "vibe": vibe, "seed": args.seed},
                                 timeout_s=30)
                t_propose = time.time() - t0
                if "error" in prop:
                    print(f"  FAIL: {prop}")
                    results.append({"vibe": vibe, "error": prop["error"]})
                    continue
                config_path = prop["config_path"]
                container_target = f"/projects/sweep/{vibe}.mp4"
                # Rewrite the output path so different vibes don't
                # clobber each other's mp4.
                host_cfg = (
                    Path(args.projects)
                    / Path(config_path).relative_to("/projects").as_posix()
                )
                host_cfg.write_text(
                    rewrite_output_path(host_cfg.read_text(encoding="utf-8"),
                                        container_target),
                    encoding="utf-8",
                )

                # 2. render
                t0 = time.time()
                rend = call_tool(base_url, sid, "render",
                                 {"config_path": config_path},
                                 timeout_s=args.render_timeout_s)
                t_render = time.time() - t0
                if not rend.get("ok"):
                    print(f"  FAIL: render: {rend}")
                    results.append({"vibe": vibe, "error": "render failed",
                                    "render_excerpt": rend})
                    continue
                ms = rend.get("manifest_summary", {})
                host_out = sweep_dir / f"{vibe}.mp4"
                size = host_out.stat().st_size if host_out.exists() else 0
                print(f"  OK  segments={ms.get('n_segments')}  "
                      f"actual={ms.get('actual_duration_secs'):.1f}s  "
                      f"target={ms.get('target_duration_secs'):.1f}s  "
                      f"size={size:,} bytes  "
                      f"propose={t_propose:.1f}s  render={t_render:.1f}s")
                results.append({
                    "vibe": vibe,
                    "config_path": config_path,
                    "output_path": container_target,
                    "host_output_path": str(host_out),
                    "size_bytes": size,
                    "propose_secs": round(t_propose, 2),
                    "render_secs": round(t_render, 2),
                    "total_secs": round(time.time() - t_total, 2),
                    "manifest_summary": ms,
                })
            except Exception as ex:
                print(f"  EXCEPTION: {ex}")
                results.append({"vibe": vibe, "error": str(ex)})

        # Write the summary.
        summary_path = sweep_dir / "sweep_summary.json"
        summary_path.write_text(
            json.dumps({
                "footage": args.footage,
                "seed": args.seed,
                "results": results,
            }, indent=2, ensure_ascii=False),
            encoding="utf-8",
        )
        print(f"\nsummary -> {summary_path}")

        # Compact comparison table.
        print()
        print(f"{'vibe':18s}  {'segments':>9}  {'actual':>8}  {'target':>8}  {'size MB':>8}  {'render s':>9}")
        for r in results:
            if "error" in r:
                print(f"{r['vibe']:18s}  ERROR: {r['error']}")
                continue
            ms = r.get("manifest_summary", {})
            print(
                f"{r['vibe']:18s}  "
                f"{ms.get('n_segments', 0):9d}  "
                f"{ms.get('actual_duration_secs', 0.0):8.1f}  "
                f"{ms.get('target_duration_secs', 0.0):8.1f}  "
                f"{r.get('size_bytes', 0) / 1_048_576:8.1f}  "
                f"{r.get('render_secs', 0.0):9.1f}"
            )

        n_ok = sum(1 for r in results if "error" not in r)
        return 0 if n_ok == len(vibes) else 1
    finally:
        print(f"\nstopping {args.container_name}...")
        subprocess.run(["docker", "stop", args.container_name],
                       capture_output=True, text=True)


if __name__ == "__main__":
    if os.name == "nt":
        try:
            sys.stdout.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
        except Exception:
            pass
    sys.exit(main())
