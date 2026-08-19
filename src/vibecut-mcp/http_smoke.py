"""HTTP smoke test for the Rust MCP server.

Drives the Streamable HTTP transport (MCP 2025-06-18 spec) by hand:
sends `initialize`, captures the session ID, sends `tools/list`,
calls `discover_sources` with an empty mount. Validates that the
all-Rust container speaks MCP correctly without needing Claude
Desktop or LM Studio in the loop.

Defaults to http://127.0.0.1:18080 (matches the smoke compose
invocation). Override with --base-url.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request

PROTOCOL_VERSION = "2025-03-26"


def post(base_url: str, body: dict, session_id: str | None = None) -> tuple[dict, str | None]:
    """POST a JSON-RPC payload to the server's `/` endpoint.

    Returns (response_json, session_id_or_None). The Streamable HTTP
    spec returns either application/json or text/event-stream — we
    accept both and parse the first SSE `data:` event when streamed.
    """
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
    with urllib.request.urlopen(req, timeout=120) as resp:
        sid = resp.headers.get("Mcp-Session-Id") or session_id
        ctype = resp.headers.get("Content-Type", "")
        raw = resp.read().decode("utf-8")
    # Notifications return 202 Accepted with empty body — that's spec-correct.
    if not raw.strip():
        return {}, sid
    if "text/event-stream" in ctype:
        # Parse first SSE `data:` line.
        for line in raw.splitlines():
            if line.startswith("data:"):
                payload = line[len("data:"):].strip()
                if payload:
                    return json.loads(payload), sid
        raise RuntimeError(f"no SSE data in response: {raw!r}")
    return json.loads(raw), sid


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:18080/")
    parser.add_argument("--call-discover", action="store_true")
    args = parser.parse_args()

    failures = 0

    # 1. initialize
    print("--> initialize")
    try:
        resp, sid = post(args.base_url, {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "http-smoke", "version": "0.1"},
            },
        })
    except (urllib.error.URLError, ConnectionError) as e:
        print(f"  FAIL: cannot reach {args.base_url}: {e}")
        return 1
    if "result" not in resp:
        print(f"  FAIL: {resp}")
        failures += 1
    else:
        info = resp["result"]
        print(
            f"  OK   server={info.get('serverInfo', {}).get('name')!r} "
            f"protocol={info.get('protocolVersion')} session={sid!r}"
        )

    # 1b. notifications/initialized (no response)
    post(args.base_url, {"jsonrpc": "2.0", "method": "notifications/initialized"}, session_id=sid)

    # 2. tools/list
    print("\n--> tools/list")
    resp, sid = post(args.base_url, {
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {},
    }, session_id=sid)
    if "result" not in resp:
        print(f"  FAIL: {resp}")
        failures += 1
    else:
        names = sorted(t["name"] for t in resp["result"].get("tools", []))
        print(f"  OK   tools registered: {names}")
        expected = {"discover_sources", "propose_vibe", "render"}
        missing = expected - set(names)
        if missing:
            print(f"  FAIL: missing: {sorted(missing)}")
            failures += 1

    # 3. (optional) discover_sources call
    if args.call_discover:
        print("\n--> tools/call discover_sources({})")
        resp, sid = post(args.base_url, {
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "discover_sources", "arguments": {}},
        }, session_id=sid)
        if "result" not in resp:
            print(f"  FAIL: {resp}")
            failures += 1
        else:
            content = resp["result"].get("content", [])
            if content and content[0].get("type") == "text":
                body = content[0].get("text", "")
                print(f"  OK   response: {body[:300]}")
            else:
                print(f"  OK   structured: {json.dumps(content)[:300]}")

    if failures:
        print(f"\n{failures} failure(s)")
        return 1
    print("\nall HTTP MCP smoke checks passed")
    return 0


if __name__ == "__main__":
    if os.name == "nt":
        try:
            sys.stdout.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
        except Exception:
            pass
    sys.exit(main())
