"""End-to-end driver: spawn the vibecut-mcp container (HTTP transport),
drive the three Phase-1 tools (discover_sources -> propose_vibe -> render),
and surface the final mp4 path. Equivalent to what the Python-era
`mcp_server.python/run_edit.py` did, retargeted to the all-Rust server's
Streamable HTTP transport.

    python src/vibecut-mcp/run_edit.py --gpus --target 1m30s --footage "E:/footage"
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

PROTOCOL_VERSION = "2025-03-26"

DEFAULT_PROJECTS = "C:/Users/vboca/OneDrive/Playground/Vibecut/out/mcp"


def post(base_url: str, body: dict, session_id: str | None = None, timeout_s: float = 60.0) -> tuple[dict, str | None]:
    headers = {
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream",
    }
    if session_id:
        headers["Mcp-Session-Id"] = session_id
    req = urllib.request.Request(
        base_url,
        data=json.dumps(body).encode("utf-8"),
        method="POST",
        headers=headers,
    )
    with urllib.request.urlopen(req, timeout=timeout_s) as resp:
        sid = resp.headers.get("Mcp-Session-Id") or session_id
        ctype = resp.headers.get("Content-Type", "")
        raw = resp.read().decode("utf-8")
    if not raw.strip():
        return {}, sid
    if "text/event-stream" in ctype:
        for line in raw.splitlines():
            if line.startswith("data:"):
                payload = line[len("data:"):].strip()
                if payload:
                    return json.loads(payload), sid
        raise RuntimeError(f"no SSE data: {raw!r}")
    return json.loads(raw), sid


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", default="vibecut:latest")
    parser.add_argument("--footage", required=True, help="Path to your GoPro/DJI footage folder")
    parser.add_argument("--projects", default=DEFAULT_PROJECTS)
    parser.add_argument("--target", default="3m")
    parser.add_argument("--vibe", default="kid_reel")
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--gpus", action="store_true")
    parser.add_argument("--port", type=int, default=18080)
    parser.add_argument("--render-timeout-s", type=int, default=900)
    parser.add_argument("--container-name", default="vibecut-mcp-runedit")
    args = parser.parse_args()

    if not Path(args.footage).exists():
        print(f"ERROR: footage path not found: {args.footage}")
        return 2
    Path(args.projects).mkdir(parents=True, exist_ok=True)
    (Path(args.projects) / "out").mkdir(parents=True, exist_ok=True)

    base_url = f"http://127.0.0.1:{args.port}/"

    # Best-effort: kill any leftover container with the same name from a
    # prior aborted run.
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
    container_id = res.stdout.strip()[:12]
    print(f"  container={container_id}")

    try:
        # Wait for the HTTP endpoint.
        deadline = time.time() + 30
        while time.time() < deadline:
            try:
                urllib.request.urlopen(base_url, timeout=2).read()
                break  # Any response means the listener is up.
            except urllib.error.HTTPError:
                break  # 405 / similar — listener is up, just doesn't accept GET-without-init
            except (urllib.error.URLError, ConnectionError):
                time.sleep(0.5)
        else:
            print("ERROR: server did not become reachable in 30s")
            print(subprocess.run(["docker", "logs", args.container_name],
                                 capture_output=True, text=True).stderr)
            return 1

        # 1. initialize
        print("\n--> initialize")
        resp, sid = post(base_url, {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "run-edit", "version": "0.1"},
            },
        })
        if "result" not in resp:
            print(f"  FAIL: {resp}")
            return 1
        print(f"  OK   protocol={resp['result'].get('protocolVersion')} session={sid}")
        post(base_url, {"jsonrpc": "2.0", "method": "notifications/initialized"}, session_id=sid)

        msg_id = 1
        def call_tool(name: str, arguments: dict, timeout_s: float = 120.0) -> dict:
            nonlocal msg_id
            msg_id += 1
            r, _ = post(base_url, {
                "jsonrpc": "2.0",
                "id": msg_id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }, session_id=sid, timeout_s=timeout_s)
            if "result" not in r:
                raise RuntimeError(f"{name} failed: {r}")
            content = r["result"].get("content", [])
            if content and content[0].get("type") == "text":
                return json.loads(content[0]["text"])
            return r["result"].get("structuredContent") or {}

        # 2. discover_sources
        print("\n--> discover_sources")
        t0 = time.time()
        disc = call_tool("discover_sources", {"folder": ""}, timeout_s=180)
        elapsed = time.time() - t0
        if "error" in disc:
            print(f"  FAIL: {disc}")
            return 1
        print(f"  OK   {disc.get('summary', '?')} ({elapsed:.1f}s)")
        for s in disc.get("sources", []):
            print(
                f"       source {s['source_id']}: "
                f"{s['n_chapters']} chapter(s), "
                f"{s['total_duration_secs']:.1f}s, "
                f"audio={s['has_audio']}"
            )

        # 3. propose_vibe
        print("\n--> propose_vibe")
        prop = call_tool("propose_vibe", {
            "folder": "",
            "vibe": args.vibe,
            "target_duration": args.target,
            "seed": args.seed,
        }, timeout_s=30)
        if "error" in prop:
            print(f"  FAIL: {prop}")
            return 1
        config_path = prop["config_path"]
        print(f"  OK   vibe={prop['vibe_name']!r}")
        print(f"       config_path={config_path}")
        print(f"       output_path={prop['output_path']}")

        # 4. render
        print(f"\n--> render  (timeout={args.render_timeout_s}s)")
        t0 = time.time()
        rend = call_tool("render", {"config_path": config_path},
                         timeout_s=args.render_timeout_s)
        elapsed = time.time() - t0
        ok = rend.get("ok")
        print(f"  {'OK' if ok else 'FAIL'}   exit={rend.get('exit_code')}  elapsed={elapsed:.1f}s")
        if "manifest_summary" in rend:
            ms = rend["manifest_summary"]
            print(
                f"       segments={ms.get('n_segments')}  "
                f"actual={ms.get('actual_duration_secs'):.1f}s  "
                f"target={ms.get('target_duration_secs'):.1f}s  "
                f"sources={ms.get('n_sources')}"
            )
        if rend.get("output_exists"):
            host_out = (
                Path(args.projects)
                / Path(rend["output_path"]).relative_to("/projects")
            )
            print(f"\n  output mp4 (host path): {host_out}")
        if not ok:
            print("\n  log tail:")
            for line in rend.get("log_tail", "").splitlines()[-15:]:
                print(f"    {line}")
            return 1
    finally:
        print(f"\nstopping container {args.container_name}...")
        subprocess.run(["docker", "stop", args.container_name],
                       capture_output=True, text=True)
    return 0


if __name__ == "__main__":
    if os.name == "nt":
        try:
            sys.stdout.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
            sys.stderr.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
        except Exception:
            pass
    sys.exit(main())
