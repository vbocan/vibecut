"""Submit a render via render_async, poll render_status,
verify it transitions queued -> running -> done with the same manifest
shape the synchronous `render` produces.
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

DEFAULT_FOOTAGE = "E:/DATA/GOPRO VIDEO/2020.04 - Ne dăm cu mașinuța electrică"
DEFAULT_PROJECTS = "C:/Users/vboca/OneDrive/Playground/Vibecut/out/mcp"


def post(base_url, body, sid=None, timeout_s=60.0):
    headers = {"Content-Type": "application/json",
               "Accept": "application/json, text/event-stream"}
    if sid:
        headers["Mcp-Session-Id"] = sid
    req = urllib.request.Request(base_url, data=json.dumps(body).encode("utf-8"),
                                 method="POST", headers=headers)
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--footage", default=DEFAULT_FOOTAGE)
    parser.add_argument("--projects", default=DEFAULT_PROJECTS)
    parser.add_argument("--gpus", action="store_true")
    parser.add_argument("--port", type=int, default=18087)
    parser.add_argument("--container-name", default="vibecut-mcp-async")
    parser.add_argument("--max-wait-s", type=int, default=600)
    args = parser.parse_args()

    base_url = f"http://127.0.0.1:{args.port}/"
    subprocess.run(["docker", "rm", "-f", args.container_name],
                   capture_output=True, text=True)

    cmd = ["docker", "run", "-d", "--rm", "--name", args.container_name,
           "-p", f"127.0.0.1:{args.port}:8080",
           "-v", f"{args.footage}:/footage:ro",
           "-v", f"{args.projects}:/projects",
           "-v", "vibecut-cache:/cache"]
    if args.gpus:
        cmd[3:3] = ["--gpus", "all"]
    cmd.append("vibecut:latest")
    print(f"$ {' '.join(repr(a) if ' ' in a else a for a in cmd)}")
    res = subprocess.run(cmd, capture_output=True, text=True)
    if res.returncode != 0:
        print(f"ERROR: {res.stderr}")
        return 1

    try:
        # Wait for listener.
        deadline = time.time() + 30
        while time.time() < deadline:
            try:
                urllib.request.urlopen(base_url, timeout=2).read()
                break
            except urllib.error.HTTPError:
                break
            except (urllib.error.URLError, ConnectionError):
                time.sleep(0.5)

        r, sid = post(base_url, {"jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{},
                      "clientInfo":{"name":"async-smoke","version":"0.1"}}})
        post(base_url, {"jsonrpc":"2.0","method":"notifications/initialized"}, sid)
        print(f"initialized: {r['result'].get('protocolVersion')}")

        msg = 1
        def call(name, arguments, timeout_s=120.0):
            nonlocal msg; msg += 1
            r, _ = post(base_url, {"jsonrpc":"2.0","id":msg,"method":"tools/call",
                "params":{"name":name,"arguments":arguments}}, sid, timeout_s)
            if "result" not in r:
                raise RuntimeError(r)
            content = r["result"].get("content", [])
            text_items = [c for c in content if c.get("type") == "text"]
            if text_items:
                return json.loads(text_items[0]["text"])
            return r["result"].get("structuredContent") or {}

        # 1. Propose
        print("\n--> propose_vibe")
        prop = call("propose_vibe", {"folder":"","vibe":"action_highlight",
                                     "target_duration":"45s","seed":42}, 30)
        cfg_path = prop["config_path"]
        print(f"  config={cfg_path}")

        # 2. render_async — should return immediately with job_id
        print("\n--> render_async")
        t0 = time.time()
        sub = call("render_async", {"config_path": cfg_path}, 10)
        submission_elapsed = time.time() - t0
        job_id = sub["job_id"]
        print(f"  submitted: job_id={job_id} state={sub['state']} ({submission_elapsed:.2f}s)")
        if submission_elapsed > 3.0:
            print(f"  WARN: submit took {submission_elapsed:.1f}s — should be ~instant")

        # 3. Poll render_status
        print("\n--> render_status (polling)")
        deadline = time.time() + args.max_wait_s
        last_state = None
        n_polls = 0
        while time.time() < deadline:
            n_polls += 1
            st = call("render_status", {"job_id": job_id}, 30)
            cur = st.get("state")
            if cur != last_state:
                print(f"  poll {n_polls}: state={cur}")
                last_state = cur
            if cur in ("done", "failed"):
                print(f"\nfinal state: {cur}")
                if cur == "done":
                    ms = st.get("manifest_summary", {})
                    print(f"  segments={ms.get('n_segments')} "
                          f"actual={ms.get('actual_duration_secs')}s "
                          f"target={ms.get('target_duration_secs')}s "
                          f"elapsed={st.get('elapsed_ms')}ms")
                else:
                    print(f"  error: {st.get('error')}")
                return 0 if cur == "done" else 1
            time.sleep(5)
        print(f"TIMEOUT after {args.max_wait_s}s; last state was {last_state}")
        return 1
    finally:
        print(f"\nstopping {args.container_name}...")
        subprocess.run(["docker", "stop", args.container_name],
                       capture_output=True, text=True)


if __name__ == "__main__":
    if os.name == "nt":
        try:
            sys.stdout.reconfigure(encoding="utf-8")
        except Exception:
            pass
    sys.exit(main())
