"""The "see the edit before the encode" journey (Playwright, Chromium):

- Make → the storyboard (the planned shots on the beat grid, with
  thumbnails) shows while the render runs, before there is any video;
- the first video is a quick draft: it plays, at 960x540, with a "Draft"
  badge, and the versions strip marks it;
- "Finish in full quality" renders the same edit in full quality, with
  progress, and the Studio then shows a 1920x1080 final of the same
  timeline (the draft badge gone);
- a tweak renders a draft again; Download on it finishes it in full
  quality first, then downloads the final;
- the top bar has the "Fast previews" switch, on by default.

    python test/draft_journey.py --url http://localhost:8090

Needs the demo set (footage folder "Coastline Bike Ride", a song). Prints
the timings it measured. Exits non-zero on any failed check.
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


def api(path: str):
    with urllib.request.urlopen(BASE + "/api" + path, timeout=60) as r:
        return json.loads(r.read())


def manifest(path: str) -> dict:
    return api("/output?path=" + urllib.parse.quote(path))


VIDEO = """() => {
  const v = document.querySelector('#main .player-wrap video');
  if (!v) return null;
  return { src: decodeURIComponent(v.currentSrc || v.src || ''), w: v.videoWidth, h: v.videoHeight,
           ready: v.readyState, t: v.currentTime, paused: v.paused };
}"""


def video(page):
    return page.evaluate(VIDEO)


def is_draft(src: str) -> bool:
    """A draft's file: `<stem>.draft.mp4`, or its kept version `<stem>.draft__<tag>.mp4`."""
    return ".draft." in src or ".draft__" in src


def wait_for(page, js: str, timeout_s: float, poll_ms: int = 100):
    t0 = time.time()
    while time.time() - t0 < timeout_s:
        if page.evaluate(js):
            return time.time() - t0
        page.wait_for_timeout(poll_ms)
    return None


def wait_video(page, pred, timeout_s: float):
    t0 = time.time()
    while time.time() - t0 < timeout_s:
        v = video(page)
        if v and v["ready"] >= 1 and v["w"] > 0 and pred(v):
            return v
        page.wait_for_timeout(150)
    return video(page)


def main() -> int:
    global BASE
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default=BASE)
    ap.add_argument("--render-timeout", type=int, default=600)
    args = ap.parse_args()
    BASE = args.url.rstrip("/")
    T = args.render_timeout
    timings: dict[str, float] = {}

    with sync_playwright() as pw:
        browser = pw.chromium.launch()
        ctx = browser.new_context(viewport={"width": 1400, "height": 1000}, accept_downloads=True)
        page = ctx.new_page()
        page.on("pageerror", lambda e: FAILS.append(f"page error: {e}"))
        page.goto(BASE + "/")

        print("Make: the storyboard comes before the video")
        page.get_by_role("radio", name="Coastline Bike Ride").first.click()
        page.wait_for_selector(".list [role=radio]")
        t0 = time.time()
        page.get_by_role("button", name="Make my video").click()
        took = wait_for(page, "!!document.querySelector('.storyboard .strip .tile')", T)
        check(took is not None, "the storyboard appears while the render runs")
        timings["storyboard"] = time.time() - t0
        state = page.evaluate(
            """() => ({ video: !!document.querySelector('#main .player-wrap video'),
                        stop: !!document.querySelector('button.stop'),
                        eyebrow: document.querySelector('.room .eyebrow')?.textContent.trim(),
                        tiles: document.querySelectorAll('.storyboard .tile.clip').length,
                        ticks: document.querySelectorAll('.storyboard .beats i').length,
                        caption: document.querySelector('.storyboard .facts')?.textContent.trim(),
                        label: document.querySelector('.storyboard .strip')?.getAttribute('aria-label') })"""
        )
        check(not state["video"] and state["stop"], f"…before there is any video, with the render still running ({state})")
        check(state["tiles"] >= 4 and state["ticks"] > 10, f"…its shots sit on a beat grid ({state['tiles']} shots, {state['ticks']} beat ticks)")
        check(state["eyebrow"] == "Cutting a quick preview", f"the first cut is a quick preview ({state['eyebrow']!r})")
        check(bool(state["label"]) and "shots" in state["label"], f"the strip is described for screen readers ({state['label']!r})")
        thumbs = wait_for(page, "document.querySelectorAll('.storyboard .tile img').length >= 3", 60)
        if thumbs is not None:
            timings["thumbnails"] = time.time() - t0
        check(thumbs is not None, "the shots get thumbnails")

        print("The draft plays")
        page.wait_for_selector(".studio", timeout=T * 1000)
        v = wait_video(page, lambda v: True, T)
        timings["draft"] = time.time() - t0
        check(v is not None and is_draft(v["src"]), f"the Studio shows the draft ({v and v['src']})")
        check(v is not None and (v["w"], v["h"]) == (960, 540), f"…at 960x540 ({v and (v['w'], v['h'])})")
        check(page.locator(".head .draft-pill").is_visible(), "a Draft badge says so")
        chip = page.locator(".draft-chip").text_content() or ""
        check("540p" in chip, f"…and the player is labelled ({chip!r})")
        page.evaluate("() => { const v = document.querySelector('#main .player-wrap video'); v.muted = true; return v.play(); }")
        page.wait_for_timeout(1500)
        v = video(page)
        check(v is not None and v["t"] > 0.5 and not v["paused"], f"the draft plays (t={v and round(v['t'], 2)})")
        page.evaluate("() => document.querySelector('#main .player-wrap video').pause()")
        check(
            page.locator(".versions .ver .draft").count() == 1,
            "the versions strip remembers it is a draft",
        )
        draft_job = next(j for j in api("/jobs")["jobs"] if j["kind"] == "render" and j["state"] == "done")
        check(draft_job.get("quality") == "draft", f"the job says draft ({draft_job.get('quality')})")
        draft_manifest = manifest(draft_job["manifest_path"])

        print("Finish in full quality")
        t1 = time.time()
        page.get_by_role("button", name="Finish in full quality").click()
        eyebrow = wait_for(
            page,
            "document.querySelector('.overlay .room .eyebrow')?.textContent.trim() === 'Finishing in full quality'",
            30,
        )
        check(eyebrow is not None, "the player shows it finishing, with progress")
        check(page.locator(".overlay [role=progressbar]").count() == 1, "…a progress bar")
        v = wait_video(page, lambda v: not is_draft(v["src"]), T)
        timings["final"] = time.time() - t1
        check(v is not None and not is_draft(v["src"]), f"the Studio shows the final ({v and v['src']})")
        check(v is not None and (v["w"], v["h"]) == (1920, 1080), f"…a 1920x1080 file ({v and (v['w'], v['h'])})")
        check(page.locator(".head .draft-pill").count() == 0, "the Draft badge is gone")
        check(page.locator(".versions .ver .draft").count() == 0, "the version is now full quality, not a new one")
        check(page.locator(".versions .ver").count() == 1, "…still one version")
        final_job = next(j for j in api("/jobs")["jobs"] if j["kind"] == "render" and j["state"] == "done")
        fm = manifest(final_job["manifest_path"])
        check(final_job.get("quality") == "final" and fm["resolution"] == "1920x1080", f"the final's manifest: {fm['resolution']}")
        check(fm["timeline"] == draft_manifest["timeline"], "the final is the draft's edit, cut for cut")
        b = fm.get("beat_sync") or {}
        check(b.get("within_tolerance") == b.get("n_cuts"), f"every cut on the beat ({b.get('within_tolerance')}/{b.get('n_cuts')})")

        print("A tweak is a draft again; Download finishes it first")
        page.locator("#main").click(position={"x": 5, "y": 5})
        page.keyboard.press("[")  # Shorter
        started = wait_for(page, "!!document.querySelector('.overlay .room')", 30)
        check(started is not None, "Shorter re-renders")
        sb = wait_for(page, "!!document.querySelector('.overlay .storyboard .tile')", T)
        check(sb is not None, "the re-cut shows its storyboard over the player")
        v = wait_video(page, lambda v: is_draft(v["src"]), T)
        check(v is not None and (v["w"], v["h"]) == (960, 540), f"the tweak is a 960x540 draft ({v and v['src']})")
        check(page.locator(".head .draft-pill").is_visible(), "…with its Draft badge")
        t2 = time.time()
        with page.expect_download(timeout=T * 1000) as dl:
            page.locator(".head-actions a.btn", has_text="Download").click()
            fin = wait_for(
                page,
                "document.querySelector('.overlay .room .eyebrow')?.textContent.trim() === 'Finishing in full quality'",
                30,
            )
            check(fin is not None, "Download on a draft finishes it in full quality first")
        timings["download_after_finish"] = time.time() - t2
        d = dl.value
        check(d.suggested_filename.endswith(".mp4"), f"…then downloads the final ({d.suggested_filename})")
        v = video(page)
        v = wait_video(page, lambda v: not is_draft(v["src"]), 30)
        check(v is not None and not is_draft(v["src"]) and (v["w"], v["h"]) == (1920, 1080), "…which is what the Studio shows")

        print("Top bar: Fast previews")
        sw = page.get_by_role("switch", name="Fast previews")
        check(sw.count() == 1 and sw.is_checked(), "a Fast previews switch, on by default")
        sw.click()
        stored = page.evaluate("localStorage.getItem('vibecut.fastPreviews')")
        check(not sw.is_checked() and stored == "off", f"switching it off is remembered ({stored})")
        sw.click()

        browser.close()

    print("\ntimings (s):", json.dumps({k: round(v, 1) for k, v in timings.items()}))
    print(f"\n{PASSES} passed, {len(FAILS)} failed")
    for f in FAILS:
        print("  FAIL", f)
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
