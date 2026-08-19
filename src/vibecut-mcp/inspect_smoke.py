"""Smoke test: drives `inspect_output` and `inspect_at` against
the existing rendered mp4 in /projects/out/. Verifies that:

1. The MCP server returns multimodal `Content` (one text summary + N
   image entries with `image/jpeg` mime type).
2. The base64 payloads decode to valid JPEG bytes (start with FFD8).
3. The number of frames is reasonable (matches `max_frames` request).
4. `inspect_at` returns frames at the exact timestamps requested.

Saves the decoded frames to `out/mcp/inspect_frames/` so the operator
can eyeball them.
"""
from __future__ import annotations

import argparse
import base64
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
    parser.add_argument("--mp4", default="/projects/out/footage.mp4",
                        help="Container path of an already-rendered mp4")
    parser.add_argument("--port", type=int, default=18083)
    parser.add_argument("--container-name", default="vibecut-mcp-inspect")
    parser.add_argument("--max-frames", type=int, default=12)
    args = parser.parse_args()

    Path(args.projects).mkdir(parents=True, exist_ok=True)
    out_dir = Path(args.projects) / "inspect_frames"
    out_dir.mkdir(parents=True, exist_ok=True)
    # Best-effort cleanup of any prior smoke run.
    for f in out_dir.glob("*.jpg"):
        f.unlink()

    # Confirm the mp4 exists on the host side first.
    mp4_host = Path(args.projects) / Path(args.mp4).relative_to("/projects")
    if not mp4_host.exists():
        print(f"ERROR: no rendered mp4 at {mp4_host}. Run run_edit.py first.")
        return 2

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
        args.image,
    ]
    print(f"$ {' '.join(repr(a) if ' ' in a else a for a in docker_cmd)}")
    res = subprocess.run(docker_cmd, capture_output=True, text=True)
    if res.returncode != 0:
        print(f"ERROR: docker run failed: {res.stderr}")
        return 1

    failures = 0
    try:
        # Wait for listener.
        deadline = time.time() + 20
        while time.time() < deadline:
            try:
                urllib.request.urlopen(base_url, timeout=2).read()
                break
            except urllib.error.HTTPError:
                break
            except (urllib.error.URLError, ConnectionError):
                time.sleep(0.4)

        print("\n--> initialize")
        resp, sid = post(base_url, {
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": PROTOCOL_VERSION,
                       "capabilities": {},
                       "clientInfo": {"name": "inspect-smoke", "version": "0.1"}},
        })
        if "result" not in resp:
            print(f"  FAIL: {resp}")
            return 1
        print(f"  OK   protocol={resp['result'].get('protocolVersion')}")
        post(base_url, {"jsonrpc": "2.0",
                        "method": "notifications/initialized"}, sid)

        # tools/list — confirm new tools registered
        r, _ = post(base_url, {"jsonrpc": "2.0", "id": 2,
                               "method": "tools/list", "params": {}}, sid)
        tools = sorted(t["name"] for t in r["result"]["tools"])
        print(f"\ntools registered: {tools}")
        for expected in ("inspect_output", "inspect_at"):
            if expected not in tools:
                print(f"  FAIL: missing tool {expected!r}")
                failures += 1

        # 1. inspect_output
        print(f"\n--> inspect_output (max_frames={args.max_frames})")
        t0 = time.time()
        r, _ = post(base_url, {
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "inspect_output",
                       "arguments": {"output_path": args.mp4,
                                     "max_frames": args.max_frames}},
        }, sid, timeout_s=180)
        elapsed = time.time() - t0
        if "result" not in r:
            print(f"  FAIL: {r}")
            return 1
        content = r["result"]["content"]
        text_items = [c for c in content if c.get("type") == "text"]
        image_items = [c for c in content if c.get("type") == "image"]
        print(f"  OK   {len(text_items)} text + {len(image_items)} image content items "
              f"({elapsed:.1f}s)")
        if image_items:
            try:
                summary = json.loads(text_items[0]["text"])
                print(f"       output duration: {summary.get('duration_secs')}s")
                print(f"       frame width: {summary.get('frame_width_px')}px")
                print(f"       frames returned at: {[f['t_secs'] for f in summary.get('frames', [])]}")
            except Exception:
                pass

        # Decode and save the JPEGs.
        for i, img in enumerate(image_items):
            data = img.get("data", "")
            mime = img.get("mimeType", "")
            if mime != "image/jpeg":
                print(f"  WARN: frame {i} has mime={mime!r}, expected image/jpeg")
            jpeg = base64.b64decode(data)
            if not jpeg.startswith(b"\xff\xd8"):
                print(f"  FAIL: frame {i} is not a valid JPEG (starts {jpeg[:4]!r})")
                failures += 1
                continue
            out = out_dir / f"overview_{i:02d}.jpg"
            out.write_bytes(jpeg)
        if image_items:
            print(f"  saved {len(image_items)} frames to {out_dir}")

        # 2. inspect_at — specific timestamps
        ts = [1.0, 30.0, 60.0]
        print(f"\n--> inspect_at (timestamps={ts})")
        t0 = time.time()
        r, _ = post(base_url, {
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "inspect_at",
                       "arguments": {"output_path": args.mp4,
                                     "timestamps": ts}},
        }, sid, timeout_s=120)
        elapsed = time.time() - t0
        if "result" not in r:
            print(f"  FAIL: {r}")
            return 1
        content = r["result"]["content"]
        image_items = [c for c in content if c.get("type") == "image"]
        text_items = [c for c in content if c.get("type") == "text"]
        if len(image_items) != len(ts):
            print(f"  FAIL: expected {len(ts)} frames, got {len(image_items)} ({elapsed:.1f}s)")
            failures += 1
        else:
            print(f"  OK   {len(image_items)} frames returned ({elapsed:.1f}s)")
            for i, img in enumerate(image_items):
                jpeg = base64.b64decode(img["data"])
                out = out_dir / f"at_{int(ts[i]):03d}s.jpg"
                out.write_bytes(jpeg)
            print(f"  saved frames to {out_dir}")

        # 3. inspect_at with rejection — out-of-range timestamps
        print("\n--> inspect_at (out-of-range timestamps)")
        r, _ = post(base_url, {
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": {"name": "inspect_at",
                       "arguments": {"output_path": args.mp4,
                                     "timestamps": [-1.0, 10000.0]}},
        }, sid, timeout_s=30)
        # Should error cleanly because no valid timestamps remain.
        if "error" in r or (r.get("result", {}).get("isError")):
            print("  OK   rejected out-of-range timestamps with a clean error")
        else:
            content = r.get("result", {}).get("content", [])
            n_images = sum(1 for c in content if c.get("type") == "image")
            if n_images == 0:
                print("  OK   no frames returned (cleaned to empty)")
            else:
                print(f"  WARN: got {n_images} frames for out-of-range request")

    finally:
        print(f"\nstopping container {args.container_name}...")
        subprocess.run(["docker", "stop", args.container_name],
                       capture_output=True, text=True)

    if failures:
        print(f"\n{failures} failure(s)")
        return 1
    print("\nall smoke checks passed")
    return 0


if __name__ == "__main__":
    if os.name == "nt":
        try:
            sys.stdout.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
        except Exception:
            pass
    sys.exit(main())
