"""Style codes, end to end (Playwright, Chromium): make a video, copy its
style code in the Studio, use the code on another trip in Create, render.

- the Studio's "Share this style" makes a short VC1- code, copies it and
  saves it by name;
- Create's "Use a style code" previews what a pasted code sets and which
  song it gets, explains a code from a newer Vibecut, and lists saved
  styles;
- "Make my video" with the code in use makes a proposal on the other
  trip whose config differs from the first only in footage-specific keys,
  renders it with every cut on the beat, and sharing that project's style
  gives back the same code.

    python test/style_codes_journey.py --url http://localhost:8090

Needs the demo set (footage folders "Coastline Bike Ride" and "Alps Ski
Day", songs in the music library). Renders two ~30 s videos. Exits
non-zero on any failed check.
"""

import argparse
import json
import sys
import time
import urllib.parse
import urllib.request

from playwright.sync_api import sync_playwright

FAILS: list[str] = []
PASSES = 0
BASE = "http://localhost:8090"


def check(cond: bool, msg: str) -> None:
    global PASSES
    if cond:
        PASSES += 1
        print(f"  ok   {msg}", flush=True)
    else:
        FAILS.append(msg)
        print(f"  FAIL {msg}", flush=True)


def api(path: str, body: dict | None = None):
    req = urllib.request.Request(BASE + "/api" + path)
    if body is not None:
        req.data = json.dumps(body).encode()
        req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.loads(r.read())


def session(page) -> dict:
    return page.evaluate("JSON.parse(localStorage.getItem('vibecut.session.v1') ?? '{}')")


def wait_studio(page, timeout: int) -> bool:
    t0 = time.time()
    page.wait_for_selector(".studio", timeout=timeout * 1000)
    while time.time() - t0 < timeout:
        if not page.query_selector("button.stop") and page.is_visible("text=Download"):
            page.wait_for_timeout(500)
            return True
        page.wait_for_timeout(250)
    return False


def flatten(v, path=""):
    if isinstance(v, dict):
        for k, x in v.items():
            yield from flatten(x, f"{path}.{k}" if path else k)
    elif isinstance(v, list):
        yield (path + ".#", len(v))
        for i, x in enumerate(v):
            yield from flatten(x, f"{path}.{i}")
    else:
        yield (path, v)


def footage_specific(p: str) -> bool:
    return (
        p in ("project.name", "input.folder")
        or (p.startswith("outputs.") and p.endswith(".path"))
        or ((p.startswith("intro.layers.") or p.startswith("outro.layers.")) and p.endswith(".content"))
    )


def main() -> int:
    global BASE
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default=BASE)
    ap.add_argument("--render-timeout", type=int, default=900)
    args = ap.parse_args()
    BASE = args.url.rstrip("/")

    with sync_playwright() as pw:
        browser = pw.chromium.launch()
        ctx = browser.new_context(viewport={"width": 1400, "height": 1000}, reduced_motion="reduce")
        page = ctx.new_page()
        page.on("pageerror", lambda e: FAILS.append(f"page error: {e}"))
        page.goto(BASE + "/")

        print("Make a video on the coastline")
        page.get_by_role("radio", name="Coastline Bike Ride").first.click()
        page.get_by_role("radio", name="Action highlight").click()
        page.get_by_role("radio", name="Reel 30 s").click()
        page.get_by_role("button", name="Make my video").click()
        check(wait_studio(page, args.render_timeout), "the first video is made")
        first_cfg_path = session(page)["proposeResult"]["config_path"]

        print("Studio: share this style")
        page.locator(".style-share .disclosure").click()
        page.wait_for_function("document.querySelector('#style-code-out')?.value.startsWith('VC1-')", timeout=60000)
        code = page.input_value("#style-code-out")
        print(f"       code ({len(code)} chars): {code}", flush=True)
        check(len(code) < 120, f"the code is short enough for a chat ({len(code)} chars)")
        chips = page.locator(".style-share .chips li").all_inner_texts()
        check(any(c.startswith("Length") and "30 s" in c for c in chips), f"its chips say what it sets ({chips})")
        page.get_by_role("button", name="Copy", exact=True).click()
        page.wait_for_selector(".style-share .ok", timeout=5000)
        check("Copied" in page.inner_text(".style-share .ok"), "Copy copies it (and says so)")
        page.fill("#style-name", "Coastline reel")
        page.get_by_role("button", name="Save", exact=True).click()
        page.wait_for_selector(".style-share .ok:has-text('Saved as')", timeout=10000)
        saved = api("/recipes")["styles"]
        check(any(s["name"] == "Coastline reel" and s["code"] == code for s in saved), "it is saved by name")

        print("Create: use the code on the Alps")
        # "Start a new video?" — yes: the first one stays in the output folder.
        page.once("dialog", lambda d: d.accept())
        page.get_by_role("button", name="New video", exact=True).click()
        page.wait_for_selector("#style-code-in")
        page.get_by_role("radio", name="Alps Ski Day").first.click()
        newer = "VC2-" + code[4:]
        page.fill("#style-code-in", newer)
        page.wait_for_selector(".style-code .error", timeout=10000)
        err = page.inner_text(".style-code .error")
        check("newer Vibecut" in err, f"a code from a newer Vibecut is explained ({err!r})")
        page.fill("#style-code-in", f"try my style: {code} 🔥")
        page.wait_for_selector(".style-code .preview", timeout=20000)
        chips = page.locator(".style-code .chips li").all_inner_texts()
        check(any(c.startswith("Length") and "30 s" in c for c in chips), f"the preview says what it sets ({chips})")
        check(any(c.startswith("Cuts") for c in chips) and any(c.startswith("Song") for c in chips), "…cuts and song included")
        music = page.inner_text(".style-code .music")
        check("Same song as the original" in music, f"it finds the same song here ({music.strip()!r})")
        check(page.locator(".saved-style", has_text="Coastline reel").count() == 1, "the saved style is offered")
        check("style code" in page.inner_text(".make-dock .eyebrow").lower(), "the dock says the code is in use")
        check("Action highlight" in page.inner_text(".make-dock .line"), "…with the style's vibe")
        page.get_by_role("button", name="Make my video").click()
        check(wait_studio(page, args.render_timeout), "the video in that style is made")

        print("The second project is the first one's style on other footage")
        second = session(page)["proposeResult"]
        check(second["config_path"] != first_cfg_path, "a new proposal was written")
        check(second["config_toml"].startswith(f"# Made from a shared style code: {code}\n"), "its config says which code made it")
        q = lambda p: "/config/structured?config_path=" + urllib.parse.quote(p)
        a, b = api(q(first_cfg_path)), api(q(second["config_path"]))
        check(b["input"]["folder"].endswith("Alps Ski Day"), f"it uses the Alps footage ({b['input']['folder']})")
        fa, fb = dict(flatten(a)), dict(flatten(b))
        differ = sorted(p for p in set(fa) | set(fb) if fa.get(p) != fb.get(p))
        check(
            all(footage_specific(p) for p in differ),
            f"the two configs differ only in footage-specific keys ({differ})",
        )

        render = next(j for j in api("/jobs")["jobs"] if j["kind"] == "render" and j["state"] == "done")
        manifest = api("/output?path=" + urllib.parse.quote(render.get("version_manifest_path") or render["manifest_path"]))
        sync = manifest.get("beat_sync") or {}
        check(
            sync.get("n_cuts", 0) > 0 and sync.get("within_tolerance") == sync.get("n_cuts"),
            f"every cut is on the beat ({sync.get('within_tolerance')}/{sync.get('n_cuts')}, mean {sync.get('mean_abs_error_ms')} ms)",
        )
        check(bool(manifest.get("timeline", {}).get("clips")), "the manifest has its exact timeline")
        check("Alps Ski Day" in json.dumps(manifest["sources"]), "the render cut the Alps footage")

        print("Sharing the new project's style gives the same code")
        page.locator(".style-share .disclosure").click()
        page.wait_for_function("document.querySelector('#style-code-out')?.value.startsWith('VC1-')", timeout=60000)
        again = page.input_value("#style-code-out")
        check(again == code, f"re-shared code is identical ({again})")

        browser.close()

    print(f"\n{PASSES} passed, {len(FAILS)} failed")
    for f in FAILS:
        print(f"  - {f}")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
