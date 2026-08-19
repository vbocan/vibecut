"""Replay-determinism harness.

Reads a JSONL session log produced by the MCP server (path:
`/projects/sessions/<container_iso>.jsonl`) and replays every tool
call against a live server, comparing the new outputs to what was
recorded. The point: prove the system is deterministic.

For a deterministic LLM-driven creative pipeline this is the central
claim — same conversation, same output. No system that calls into a
non-deterministic LLM at runtime can claim this; we can because the
LLM is in the *client* and our server is pure tools.

Usage:

    # Replay against a fresh container (script spins one up):
    python src/vibecut-mcp/replay.py --log out/mcp/sessions/<file>.jsonl \\
        --footage "E:/footage" --gpus

    # Replay against an existing running server:
    python src/vibecut-mcp/replay.py --log <file>.jsonl \\
        --base-url http://localhost:8080/

Determinism is checked PER TOOL on the fields that should be
reproducible (see `STABLE_FIELDS`). Fields that legitimately vary
(timestamps, file paths with embedded `YYYYMMDD_HHMMSS`, byte sizes)
are excluded so they don't flag false positives.
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


# Per-tool list of `result_excerpt` JSON pointers to compare. A
# divergence on any of these fields is a determinism failure; fields
# not listed are tolerated (timestamps, paths with timestamps, etc.).
STABLE_FIELDS: dict[str, list[tuple[str, ...]]] = {
    "discover_sources": [
        ("n_sources",),
        ("n_with_audio",),
        ("total_secs",),
        ("summary",),
    ],
    "list_vibes": [
        ("n_vibes",),
    ],
    "propose_vibe": [
        ("vibe",),
        # config_path / output_path embed a timestamp — skip.
    ],
    "render": [
        ("ok",),
        ("manifest_summary", "n_segments"),
        ("manifest_summary", "actual_duration_secs"),
        ("manifest_summary", "target_duration_secs"),
        ("manifest_summary", "n_sources"),
    ],
    "apply_feedback": [
        ("ok",),
        ("n_changes",),
        ("action",),
        ("changes",),
    ],
    # The inspect_* tools return image bytes that may vary very
    # slightly between runs (encoder pseudo-random); their existence
    # in the log is mostly informational.
    "inspect_output": [
        ("n_frames",),
    ],
    "inspect_at": [
        ("n_frames",),
    ],
}


def get_path(d: dict, path: tuple[str, ...]):
    cur = d
    for k in path:
        if not isinstance(cur, dict) or k not in cur:
            return None
        cur = cur[k]
    return cur


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
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "replay-harness", "version": "0.1"},
        },
    })
    if "result" not in resp:
        raise RuntimeError(f"initialize failed: {resp}")
    post(base_url, {"jsonrpc": "2.0", "method": "notifications/initialized"}, sid)
    return sid


def call_tool(base_url: str, sid: str | None, tool: str,
              arguments: dict, timeout_s: float = 900.0) -> dict:
    """Invoke a tool. Returns the parsed JSON body of the response
    (the first `text` content block parsed as JSON), or for
    multimodal tools a synthesised dict that mirrors what the
    logger would record."""
    msg_id = int(time.time() * 1000) & 0xFFFFFF
    r, _ = post(base_url, {
        "jsonrpc": "2.0", "id": msg_id, "method": "tools/call",
        "params": {"name": tool, "arguments": arguments},
    }, sid=sid, timeout_s=timeout_s)
    if "result" not in r:
        return {"error": r.get("error", "unknown error")}
    content = r["result"].get("content", [])
    text_items = [c for c in content if c.get("type") == "text"]
    image_items = [c for c in content if c.get("type") == "image"]
    if text_items:
        try:
            body = json.loads(text_items[0]["text"])
        except json.JSONDecodeError:
            body = {"raw_text_preview": text_items[0]["text"][:200]}
    else:
        body = {}
    # Mirror the server's logger excerpt for inspect_* tools.
    if tool in ("inspect_output", "inspect_at"):
        return {"n_frames": len(image_items), "summary": body}
    return body


def excerpt_for(tool: str, body: dict) -> dict:
    """Re-create the same excerpt shape the server's logger emits.
    Mirrors `excerpt_for` in lib.rs so comparisons are like-for-like.
    """
    if tool == "discover_sources":
        sources = body.get("sources") or []
        n_with_audio = sum(1 for s in sources if s.get("has_audio"))
        return {
            "n_sources": body.get("n_sources"),
            "total_secs": body.get("total_secs"),
            "n_with_audio": n_with_audio,
            "summary": body.get("summary"),
        }
    if tool == "list_vibes":
        vibes = body.get("vibes") or []
        return {"n_vibes": len(vibes)}
    if tool == "propose_vibe":
        return {
            "vibe": body.get("vibe"),
            "config_path": body.get("config_path"),
            "output_path": body.get("output_path"),
        }
    if tool == "apply_feedback":
        return {
            "ok": body.get("ok", False),
            "n_changes": body.get("n_changes", 0),
            "changes": body.get("changes", []),
            "action": body.get("action"),
        }
    if tool == "render":
        return {
            "ok": body.get("ok", False),
            "output_path": body.get("output_path"),
            "manifest_summary": body.get("manifest_summary"),
        }
    return body


def compare(tool: str, recorded: dict, live: dict) -> list[str]:
    """Return a list of human-readable mismatch descriptions for
    `tool`, comparing the recorded excerpt to a freshly extracted one.
    Empty list = deterministic."""
    fields = STABLE_FIELDS.get(tool, [])
    diffs: list[str] = []
    for path in fields:
        a = get_path(recorded, path)
        b = get_path(live, path)
        # Float comparison with epsilon — avoids bit-level wobble that
        # doesn't matter (e.g. ffmpeg rounding to the next 30-fps frame).
        if isinstance(a, (int, float)) and isinstance(b, (int, float)):
            if abs(float(a) - float(b)) > 0.05:
                diffs.append(f"{'.'.join(path)}: {a!r} -> {b!r}")
        else:
            if a != b:
                diffs.append(f"{'.'.join(path)}: {a!r} -> {b!r}")
    return diffs


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--log", required=True, help="JSONL session log to replay")
    parser.add_argument("--base-url",
                        help="MCP server URL. If omitted, the script spins up a temporary container.")
    parser.add_argument("--image", default="vibecut:latest")
    parser.add_argument("--footage",
                        default=None,
                        help="Footage bind-mount source (required only when spawning a container, i.e. when --base-url is omitted)")
    parser.add_argument("--projects",
                        default="C:/Users/vboca/OneDrive/Playground/Vibecut/out/mcp")
    parser.add_argument("--port", type=int, default=18084)
    parser.add_argument("--gpus", action="store_true")
    parser.add_argument("--render-timeout-s", type=int, default=900)
    args = parser.parse_args()

    log_path = Path(args.log)
    if not log_path.exists():
        print(f"ERROR: log file not found: {log_path}")
        return 2

    # Read the log.
    entries = []
    for line in log_path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            entries.append(json.loads(line))
        except json.JSONDecodeError as e:
            print(f"WARN: skipping malformed log line: {e}")
    print(f"loaded {len(entries)} entries from {log_path}")

    tool_calls = [e for e in entries if e.get("event") == "tool_call"]
    if not tool_calls:
        print("ERROR: no tool_call entries in log")
        return 1
    print(f"{len(tool_calls)} tool calls to replay")

    # Spin up a container if no --base-url was given.
    container_id = None
    if args.base_url:
        base_url = args.base_url
    else:
        if not args.footage:
            print("ERROR: --footage is required when --base-url is not given")
            return 2
        if not Path(args.footage).exists():
            print(f"ERROR: footage path not found: {args.footage}")
            return 2
        base_url = f"http://127.0.0.1:{args.port}/"
        # Best-effort cleanup.
        subprocess.run(["docker", "rm", "-f", "vibecut-mcp-replay"],
                       capture_output=True, text=True)
        cmd = [
            "docker", "run", "-d", "--rm",
            "--name", "vibecut-mcp-replay",
            "-p", f"127.0.0.1:{args.port}:8080",
            "-v", f"{args.footage}:/footage:ro",
            "-v", f"{args.projects}:/projects",
            "-v", "vibecut-cache:/cache",
        ]
        if args.gpus:
            cmd[3:3] = ["--gpus", "all"]
        cmd.append(args.image)
        print(f"$ {' '.join(repr(a) if ' ' in a else a for a in cmd)}")
        res = subprocess.run(cmd, capture_output=True, text=True)
        if res.returncode != 0:
            print(f"ERROR: docker run failed: {res.stderr}")
            return 1
        container_id = res.stdout.strip()[:12]
        # Wait for HTTP listener.
        deadline = time.time() + 30
        while time.time() < deadline:
            try:
                urllib.request.urlopen(base_url, timeout=2).read()
                break
            except urllib.error.HTTPError:
                break
            except (urllib.error.URLError, ConnectionError):
                time.sleep(0.5)

    try:
        sid = initialize(base_url)
        print(f"\nreplaying against {base_url}")
        print("=" * 70)

        deterministic = 0
        drifted = 0
        errors = 0
        per_call = []

        # Path remap: every successful propose_vibe creates a NEW
        # /projects/proposal_<slug>_<timestamp>.toml. Subsequent render
        # / apply_feedback calls in the recorded log reference the
        # OLD path, but during replay we want them to point at the
        # newly-created file (otherwise we render a stale TOML left
        # over from a previous session and get drift that's a test
        # artifact, not a real determinism failure).
        path_remap: dict[str, str] = {}

        def remap(args_json: dict) -> dict:
            """Substitute remapped paths in tool arguments."""
            out = {}
            for k, v in args_json.items():
                if isinstance(v, str) and v in path_remap:
                    out[k] = path_remap[v]
                else:
                    out[k] = v
            return out

        for i, e in enumerate(tool_calls, start=1):
            tool = e["tool"]
            args_json = remap(e.get("args") or {})
            recorded = e.get("result_excerpt") or {}

            t0 = time.time()
            try:
                body = call_tool(base_url, sid, tool, args_json,
                                 timeout_s=args.render_timeout_s)
            except Exception as ex:
                print(f"[{i}/{len(tool_calls)}] {tool:18s} ERROR: {ex}")
                errors += 1
                per_call.append((tool, "error", str(ex)))
                continue
            elapsed = time.time() - t0

            # Register path remappings from this call's output.
            if tool == "propose_vibe":
                old_cfg = recorded.get("config_path")
                new_cfg = body.get("config_path")
                if old_cfg and new_cfg and old_cfg != new_cfg:
                    path_remap[old_cfg] = new_cfg
                old_out = recorded.get("output_path")
                new_out = body.get("output_path")
                if old_out and new_out and old_out != new_out:
                    path_remap[old_out] = new_out

            live_excerpt = excerpt_for(tool, body)
            diffs = compare(tool, recorded, live_excerpt)
            if diffs:
                drifted += 1
                print(f"[{i}/{len(tool_calls)}] {tool:18s} DRIFT  ({elapsed:.1f}s)")
                for d in diffs:
                    print(f"     {d}")
                per_call.append((tool, "drift", diffs))
            else:
                deterministic += 1
                print(f"[{i}/{len(tool_calls)}] {tool:18s} OK     ({elapsed:.1f}s)")
                per_call.append((tool, "ok", None))

        print("=" * 70)
        total = len(tool_calls)
        rate = (deterministic / total * 100.0) if total else 0.0
        print(f"determinism: {deterministic}/{total} ({rate:.1f}%)  "
              f"drifted: {drifted}  errors: {errors}")
        return 0 if drifted == 0 and errors == 0 else 1
    finally:
        if container_id:
            print(f"\nstopping replay container...")
            subprocess.run(["docker", "stop", "vibecut-mcp-replay"],
                           capture_output=True, text=True)


if __name__ == "__main__":
    if os.name == "nt":
        try:
            sys.stdout.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
        except Exception:
            pass
    sys.exit(main())
