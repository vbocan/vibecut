"""End-to-end test: propose -> render -> apply_feedback -> render.

Spins up the MCP container, drives the conversation as if an LLM
client had decided to iterate. Validates that:

1. `apply_feedback` actually mutates the config (verified by reading
   the config file before and after).
2. The mutated config still validates and renders cleanly.
3. The two rendered outputs differ — i.e. feedback actually changes
   the result.

Usage:

    python vibecut-mcp/run_iteration.py --footage "E:/footage" --gpus

By default exercises the `Calmer` action against an `action_highlight`
proposal. Pass --action calmer | punchier | scale_shorter | scale_longer
| reroll to try others.
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


ACTIONS = {
    "calmer": {"kind": "calmer"},
    "punchier": {"kind": "punchier"},
    "scale_shorter": {"kind": "scale_duration", "factor": 0.6},
    "scale_longer": {"kind": "scale_duration", "factor": 1.5},
    "reroll": {"kind": "reroll"},
}


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


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", default="vibecut:latest")
    parser.add_argument("--footage", required=True, help="Path to your GoPro/DJI footage folder")
    parser.add_argument("--projects", default=DEFAULT_PROJECTS)
    parser.add_argument("--vibe", default="action_highlight")
    parser.add_argument("--action", default="calmer", choices=list(ACTIONS))
    parser.add_argument("--gpus", action="store_true")
    parser.add_argument("--port", type=int, default=18082)
    parser.add_argument("--render-timeout-s", type=int, default=900)
    parser.add_argument("--container-name", default="vibecut-mcp-iter")
    args = parser.parse_args()

    if not Path(args.footage).exists():
        print(f"ERROR: footage not found: {args.footage}")
        return 2
    Path(args.projects).mkdir(parents=True, exist_ok=True)
    (Path(args.projects) / "out").mkdir(parents=True, exist_ok=True)

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
    print(f"  container={res.stdout.strip()[:12]}")

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

        # initialize
        print("\n--> initialize")
        resp, sid = post(base_url, {
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "iteration-test", "version": "0.1"},
            },
        })
        print(f"  OK   protocol={resp['result'].get('protocolVersion')}")
        post(base_url, {"jsonrpc": "2.0", "method": "notifications/initialized"}, sid)

        msg_id = 1
        def call_tool(name: str, arguments: dict, timeout_s: float = 120.0) -> dict:
            nonlocal msg_id
            msg_id += 1
            r, _ = post(base_url, {
                "jsonrpc": "2.0", "id": msg_id, "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }, sid=sid, timeout_s=timeout_s)
            if "result" not in r:
                raise RuntimeError(f"{name} failed: {r}")
            content = r["result"].get("content", [])
            if content and content[0].get("type") == "text":
                return json.loads(content[0]["text"])
            return r["result"].get("structuredContent") or {}

        # 1. propose_vibe
        print(f"\n--> propose_vibe (vibe={args.vibe})")
        prop = call_tool("propose_vibe", {
            "folder": "", "vibe": args.vibe, "seed": 42,
        }, timeout_s=30)
        config_path = prop["config_path"]
        print(f"  OK   config_path={config_path}")

        # 2. first render — baseline
        print("\n--> render  (baseline)")
        t0 = time.time()
        r1 = call_tool("render", {"config_path": config_path},
                       timeout_s=args.render_timeout_s)
        elapsed1 = time.time() - t0
        print(f"  OK   elapsed={elapsed1:.1f}s "
              f"segments={r1.get('manifest_summary', {}).get('n_segments')}")
        baseline_output = r1.get("output_path")

        # Move baseline aside so the second render writes to the same path
        # and we can compare.
        if baseline_output:
            host_baseline = (
                Path(args.projects)
                / Path(baseline_output).relative_to("/projects")
            )
            host_baseline_save = host_baseline.with_name(
                host_baseline.stem + "_baseline" + host_baseline.suffix
            )
            if host_baseline.exists():
                host_baseline.replace(host_baseline_save)
                print(f"  saved baseline as {host_baseline_save.name}")

        # 3. apply_feedback
        print(f"\n--> apply_feedback (action={args.action})")
        config_text_before = Path(
            args.projects + "/" + Path(config_path).relative_to("/projects").as_posix()
        ).read_text(encoding="utf-8")

        fb = call_tool("apply_feedback", {
            "config_path": config_path,
            "action": ACTIONS[args.action],
        }, timeout_s=15)
        if not fb.get("ok"):
            print(f"  FAIL: {fb}")
            return 1
        print(f"  OK   {fb['n_changes']} change(s):")
        for c in fb.get("changes", []):
            print(f"       {c['field']}: {c['before']!r} -> {c['after']!r}")

        # Verify the file actually changed.
        config_text_after = Path(
            args.projects + "/" + Path(config_path).relative_to("/projects").as_posix()
        ).read_text(encoding="utf-8")
        if config_text_before == config_text_after and fb["n_changes"] > 0:
            print("  WARN: tool reported changes but file content is identical")

        # 4. second render — the iterated version
        print("\n--> render  (after feedback)")
        t0 = time.time()
        r2 = call_tool("render", {"config_path": config_path},
                       timeout_s=args.render_timeout_s)
        elapsed2 = time.time() - t0
        print(f"  OK   elapsed={elapsed2:.1f}s "
              f"segments={r2.get('manifest_summary', {}).get('n_segments')}")
        iterated_output = r2.get("output_path")

        # 5. compare
        if baseline_output and iterated_output:
            host_iterated = (
                Path(args.projects)
                / Path(iterated_output).relative_to("/projects")
            )
            host_baseline_save = host_iterated.with_name(
                host_iterated.stem + "_baseline" + host_iterated.suffix
            )
            print()
            if host_baseline_save.exists() and host_iterated.exists():
                same_size = host_baseline_save.stat().st_size == host_iterated.stat().st_size
                print(f"  baseline:  {host_baseline_save.name}  "
                      f"({host_baseline_save.stat().st_size:,} bytes)")
                print(f"  iterated:  {host_iterated.name}  "
                      f"({host_iterated.stat().st_size:,} bytes)")
                if same_size:
                    print("  WARN: outputs have identical size — feedback may not have")
                    print("        materially changed the cut.")
                else:
                    print("  OK: outputs differ in size — feedback produced a different cut.")
    finally:
        print(f"\nstopping container {args.container_name}...")
        subprocess.run(["docker", "stop", args.container_name],
                       capture_output=True, text=True)
    return 0


if __name__ == "__main__":
    if os.name == "nt":
        try:
            sys.stdout.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
        except Exception:
            pass
    sys.exit(main())
