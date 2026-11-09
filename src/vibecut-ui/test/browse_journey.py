"""Footage from any drive (Playwright, Chromium), keyboard only up to Make:

- Create → "Browse your drives…" opens a modal dialog whose folder list
  has the focus; the drive list shows the host's drives;
- arrows + Enter walk from the drive to a folder, Backspace goes up (and
  lands back on the folder it came from), the breadcrumb follows, folders
  with videos get a count and a thumbnail;
- "Use this folder" picks it: the dialog closes, the focus is back on the
  opener, the folder is selected as a Recent trip card and its clips are
  read like any trip; Make my video renders it, beat-locked;
- after a reload the Recent card is still there;
- the API refuses traversal (`..`, NUL, outside the roots, hidden
  folders) with 400 and a rebinding Host with 403;
- no console errors.

    python test/browse_journey.py --url http://localhost:8090 \
        --path 'C:\\Users\\me\\Videos\\Coastline Bike Ride'

`--path` is the host folder to walk to (display form, a drive root
first). Exits non-zero on any failed check.
"""

import argparse
import json
import sys
import time
import urllib.error
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


def status_of(path: str, headers: dict | None = None) -> int:
    req = urllib.request.Request(BASE + "/api" + path, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code


def wait_for(page, js: str, timeout_s: float, poll_ms: int = 100):
    t0 = time.time()
    while time.time() - t0 < timeout_s:
        if page.evaluate(js):
            return time.time() - t0
        page.wait_for_timeout(poll_ms)
    return None


ACTIVE = "() => document.querySelector('dialog[open] .row.active .name')?.textContent.trim() ?? null"
CURRENT = "() => document.querySelector('dialog[open] .crumb.current')?.textContent.trim() ?? null"
IDLE = "() => document.querySelector('dialog[open] [role=listbox]')?.getAttribute('aria-busy') === 'false'"


def wait_listing(page, name: str) -> bool:
    js = f"(() => {{ const l = document.querySelector('dialog[open] [role=listbox]'); const c = document.querySelector('dialog[open] .crumb.current'); return !!l && l.getAttribute('aria-busy') === 'false' && c?.textContent.trim() === {json.dumps(name)}; }})()"
    return wait_for(page, js, 30) is not None


def highlight(page, name: str) -> bool:
    """Arrow down from the top until `name` is the highlighted folder."""
    page.keyboard.press("Home")
    n = page.locator("dialog[open] [role=option]").count()
    for _ in range(n + 1):
        if page.evaluate(ACTIVE) == name:
            return True
        page.keyboard.press("ArrowDown")
    return page.evaluate(ACTIVE) == name


def main() -> int:
    global BASE
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default=BASE)
    ap.add_argument("--path", required=True, help=r"host folder, display form: C:\Users\…\Trip")
    ap.add_argument("--render-timeout", type=int, default=600)
    args = ap.parse_args()
    BASE = args.url.rstrip("/")
    T = args.render_timeout
    parts = [p for p in args.path.replace("/", "\\").split("\\") if p]
    drive, segs = parts[0].upper(), parts[1:]
    trip = segs[-1]

    print("The API refuses escapes")
    q = lambda p: "/browse?path=" + urllib.parse.quote(p, safe="")
    for bad in [f"{drive}\\..", f"{drive}\\Users\\..\\..\\etc", "/host/c/../../etc", "/etc/passwd", "/proc/self",
                f"{drive}\\$RECYCLE.BIN", "relative\\folder", f"{drive}\\x\x00y", ""]:
        check(status_of(q(bad)) == 400, f"400 for {bad!r}")
    check(status_of(q(f"{drive}\\no-such-folder-here-42")) == 404, "404 for a missing folder")
    check(status_of("/browse/roots", {"Host": "evil.example.com"}) == 403, "403 for a rebinding Host")
    check(status_of(q(f"{drive}\\")) == 200, f"200 for {drive}\\")
    check(status_of(q(f"{drive}\\")) == 200 and not api(q(f"{drive}\\"))["can_use"], "a whole drive can't be used")

    with sync_playwright() as pw:
        browser = pw.chromium.launch()
        ctx = browser.new_context(viewport={"width": 1400, "height": 1000})
        page = ctx.new_page()
        errors: list[str] = []
        page.on("pageerror", lambda e: errors.append(f"page error: {e}"))
        page.on("console", lambda m: errors.append(f"console {m.type}: {m.text}") if m.type == "error" else None)
        page.goto(BASE + "/")
        page.wait_for_selector("h2#trip-h")

        print("Open the dialog from the keyboard")
        opener = page.get_by_role("button", name="Browse your drives…")
        opener.focus()
        page.keyboard.press("Enter")
        page.wait_for_selector("dialog[open]")
        check(page.get_by_role("dialog", name="Browse your drives").is_visible(), "a dialog named 'Browse your drives' opens")
        check(wait_for(page, "(" + IDLE + ")()", 20) is not None, "…and lists the drives")
        focus = page.evaluate("() => document.activeElement?.getAttribute('role')")
        check(focus == "listbox", f"the folder list has the focus ({focus})")
        names = page.evaluate("() => [...document.querySelectorAll('dialog[open] .row .name')].map(e => e.textContent.trim())")
        check(drive in names and not any(n.lower() in ("wsl", "wslg") for n in names), f"the drives: {names}")
        check(page.evaluate(CURRENT) == "Drives", "the breadcrumb starts at Drives")
        check(page.get_by_role("button", name="Use this folder").is_disabled(), "nothing to use at the drive list")

        print(f"Walk to {args.path}")
        check(highlight(page, drive), f"arrows reach {drive}")
        page.keyboard.press("Enter")
        check(wait_listing(page, drive), f"Enter opens {drive}")
        check(page.get_by_role("button", name="Use this folder").is_disabled(), "a whole drive can't be used")
        for i, seg in enumerate(segs):
            ok = highlight(page, seg)
            check(ok, f"arrows reach {seg}")
            if not ok:
                break
            page.keyboard.press("Enter")
            check(wait_listing(page, seg), f"Enter opens {seg}")
            if i == len(segs) - 2:
                # One level above the trip: Backspace goes up and comes back.
                page.keyboard.press("Backspace")
                check(wait_listing(page, segs[i - 1] if i else drive), "Backspace goes up a level")
                check(page.evaluate(ACTIVE) == seg, f"…with {seg} highlighted again")
                page.keyboard.press("Enter")
                check(wait_listing(page, seg), f"Enter goes back into {seg}")
                count = page.evaluate(
                    f"() => [...document.querySelectorAll('dialog[open] .row')].find(r => r.querySelector('.name')?.textContent.trim() === {json.dumps(trip)})?.querySelector('.videos')?.textContent.trim() ?? null"
                )
                check(bool(count) and "video" in count, f"{trip} shows its video count ({count})")
                thumb = wait_for(
                    page,
                    f"(() => {{ const r = [...document.querySelectorAll('dialog[open] .row')].find(r => r.querySelector('.name')?.textContent.trim() === {json.dumps(trip)}); return !!r?.querySelector('img[src^=\"data:image\"]'); }})()",
                    60,
                )
                check(thumb is not None, f"…and a thumbnail")
        crumbs = page.evaluate("() => [...document.querySelectorAll('dialog[open] .crumbs li')].map(e => e.textContent.replace('›','').trim())")
        check(crumbs == ["Drives", drive] + segs, f"the breadcrumb is the whole path ({len(crumbs)} parts)")
        shown = page.locator("dialog[open] .path").text_content()
        check(shown.lower() == args.path.lower().rstrip("\\"), f"the path is shown as on the host ({shown})")
        here = page.locator("dialog[open] .foot .here").text_content().strip()
        check("video" in here, f"the footer counts the videos here ({here!r})")

        print("Use this folder (keyboard)")
        use = page.get_by_role("button", name="Use this folder")
        check(use.is_enabled(), "Use this folder is enabled")
        for _ in range(6):
            page.keyboard.press("Tab")
            if page.evaluate("() => document.activeElement?.textContent.trim()") == "Use this folder":
                break
        check(page.evaluate("() => document.activeElement?.textContent.trim()") == "Use this folder", "Tab reaches Use this folder")
        page.keyboard.press("Enter")
        check(wait_for(page, "!document.querySelector('dialog[open]')", 5) is not None, "the dialog closes")
        back = page.evaluate("() => document.activeElement?.textContent.trim()")
        check(back == "Browse your drives…", f"the focus is back on the opener ({back!r})")
        card = page.get_by_role("radio", name=trip)
        check(card.count() >= 1 and card.first.get_attribute("aria-checked") == "true", f"{trip} is the selected trip")
        check(card.first.locator(".tag").text_content() == "Recent", "…as a Recent card")
        clips = wait_for(page, "document.querySelectorAll('.sources .strip .clip').length > 0", 120)
        check(clips is not None, "its clips are read like any trip's")

        print("Make my video, from the keyboard")
        page.wait_for_selector("button.make:not([disabled])", timeout=60_000)
        page.locator("button.make").focus()
        page.keyboard.press("Enter")
        page.wait_for_selector(".studio", timeout=T * 1000)
        done = wait_for(page, "!!document.querySelector('#main .player-wrap video')?.currentSrc", T)
        check(done is not None, "the video plays in the Studio")
        jobs = [j for j in api("/jobs")["jobs"] if j["kind"] == "render"]
        job = jobs[0] if jobs else {}
        check(job.get("state") == "done", f"the render is done ({job.get('state')}, {job.get('quality')})")
        if job.get("manifest_path"):
            m = api("/output?path=" + urllib.parse.quote(job["manifest_path"]))
            b = m.get("beat_sync") or {}
            check(b.get("n_cuts", 0) > 0 and b.get("within_tolerance") == b.get("n_cuts"),
                  f"every cut on the beat ({b.get('within_tolerance')}/{b.get('n_cuts')})")
            srcs = {c.get("source_path") or c.get("path") or "" for c in (m.get("timeline") or {}).get("clips", [])} if isinstance(m.get("timeline"), dict) else set()
            print(f"  info manifest {job['manifest_path']}, {len(srcs)} source paths seen")
        cfg_path = (job.get("config_path") or "")
        if cfg_path:
            toml = api("/config?config_path=" + urllib.parse.quote(cfg_path)).get("config_toml", "")
            check("/host/" in toml and trip in toml, "the config reads the footage straight from the drive")

        print("After a reload")
        page.reload()
        page.wait_for_selector(".studio, h2#trip-h")
        if page.locator("h2#trip-h").count() == 0:
            page.once("dialog", lambda d: d.accept())  # "Start a new video?"
            page.get_by_role("button", name="New video", exact=True).click()
        page.wait_for_selector("h2#trip-h")
        check(wait_for(page, f"[...document.querySelectorAll('[role=radio]')].some(r => r.textContent.includes({json.dumps(trip)}) && r.querySelector('.tag'))", 20) is not None,
              "the Recent card is still there")
        page.wait_for_timeout(1500)

        check(not errors, f"no console errors ({errors[:5]})")
        browser.close()

    print(f"\n{PASSES} passed, {len(FAILS)} failed")
    for f in FAILS:
        print(f"  - {f}")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
