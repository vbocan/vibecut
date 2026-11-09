"""Songs from any drive, and where to get them (Playwright, Chromium):

- an empty library offers "Pick a music folder…" and "Where to get free
  music"; the latter opens the Add music panel on its first site link,
  every site link opens a new tab (https, noopener);
- "Pick a music folder…" opens the drive dialog in music mode: song
  counts, no video wording; it walks to `--path` and "Use this folder"
  adds it: the songs are listed, the folder shows under "Also reading" as
  its host path, and it survives a reload;
- the API refuses a whole drive and folders outside the drives;
- a proposal with one of those songs renders (the render reads the song
  straight from the drive);
- "Remove" takes the folder out again: the library is empty;
- no console errors.

    python test/music_folder_journey.py --url http://localhost:8090 \\
        --path 'C:\\Users\\me\\Music\\Road trip' --footage 'Coastline Bike Ride'

`--path` (display form, drive first) must hold songs; `--footage` is a
trip under /footage for the render. Exits non-zero on any failed check.
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
    else:
        FAILS.append(msg)
        print(f"  FAIL {msg}")


def api(path: str, body=None, method=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + "/api" + path, data=data, method=method or ("POST" if data else "GET"))
    if data:
        req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=600) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def wait_for(page, js: str, secs: float):
    try:
        return page.wait_for_function(js, timeout=secs * 1000)
    except Exception:  # noqa: BLE001
        return None


def main() -> int:
    global BASE
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default=BASE)
    ap.add_argument("--path", required=True)
    ap.add_argument("--footage", required=True)
    args = ap.parse_args()
    BASE = args.url.rstrip("/")
    parts = [p for p in args.path.replace("/", "\\").split("\\") if p]
    drive, below = parts[0], parts[1:]

    print("The API refuses what isn't a music folder on a drive")
    for bad in [drive + "\\", "/etc", "/host", "C:\\Windows\\..\\..\\etc", ""]:
        st, _ = api("/music/folders", {"path": bad})
        check(st in (400, 404), f"POST /music/folders {bad!r} is refused ({st})")

    with sync_playwright() as p:
        browser = p.chromium.launch()
        ctx = browser.new_context(viewport={"width": 1300, "height": 900})
        page = ctx.new_page()
        console: list[str] = []
        page.on("console", lambda m: console.append(m.text) if m.type == "error" else None)
        page.on("pageerror", lambda e: console.append(str(e)))
        page.goto(BASE + "/")

        print("Empty library: both ways in")
        page.wait_for_selector(".act .empty")
        empty = page.locator(".act .empty")
        check(empty.get_by_role("button", name="Pick a music folder…").count() == 1, "the empty state offers Pick a music folder…")
        empty.get_by_role("button", name="Where to get free music").click()
        page.wait_for_timeout(400)
        focused = page.evaluate("() => document.activeElement?.closest('.find') && document.activeElement.tagName")
        check(focused == "A", f"Where to get free music lands on the first site link ({focused})")
        links = page.locator(".find a")
        n = links.count()
        check(n >= 5, f"the panel lists the sites ({n})")
        for i in range(n):
            a = links.nth(i)
            href, target, rel = a.get_attribute("href"), a.get_attribute("target"), a.get_attribute("rel") or ""
            check(href.startswith("https://") and target == "_blank" and "noopener" in rel, f"site link {href} opens safely in a new tab")
        check(page.locator(".find .badge").count() == n, "each site says what its licence asks")

        print(f"Pick {args.path}")
        empty.get_by_role("button", name="Pick a music folder…").click()
        dlg = page.locator("dialog[open]")
        dlg.wait_for()
        check(dlg.get_by_role("heading", name="Pick a music folder").count() == 1, "the dialog is the music folder picker")
        check(dlg.get_by_text("Your music, anywhere").count() == 1, "…with music wording")

        def open_row(name: str):
            row = dlg.locator("[role=option]", has=page.locator(".name", has_text=name)).filter(
                has=page.locator(f".name:text-is({json.dumps(name)})")
            )
            row.first.wait_for(timeout=20000)
            row.first.click()
            wait_for(page, f"document.querySelector('dialog[open] .path')?.textContent.endsWith({json.dumps(name)})", 20)

        open_row(drive)
        for name in below[:-1]:
            open_row(name)
        last = below[-1]
        parent_row = dlg.locator("[role=option]").filter(has=page.locator(f".name:text-is({json.dumps(last)})"))
        meta = parent_row.first.locator(".meta").text_content() or ""
        check("song" in meta and "video" not in meta, f"the folder row counts songs ({meta.strip()!r})")
        open_row(last)
        foot = dlg.locator(".foot .here").text_content() or ""
        check("song" in foot, f"the footer counts the songs here ({foot.strip()!r})")
        check(dlg.locator(".has-thumb").count() == 0, "no video thumbnails in music mode")
        dlg.get_by_role("button", name="Use this folder").click()
        check(wait_for(page, "!document.querySelector('dialog[open]')", 5) is not None, "the dialog closes")
        added = wait_for(page, "[...document.querySelectorAll('[role=status]')].some(e => /^Added .*: \\d+ songs?\\.$/.test(e.textContent))", 120)
        check(added is not None, "the picker says what it added")
        page.wait_for_selector(".act .list [role=radio]", timeout=30000)
        n_songs = page.locator(".act .list [role=radio]").count()
        check(n_songs >= 1, f"the songs are listed ({n_songs})")

        st, lib = api("/music")
        picked = [d for d in lib["dirs"] if d.get("picked")]
        check(len(picked) == 1 and picked[0]["display"].lower() == args.path.lower(), f"the server lists the folder as {args.path} ({picked})")
        song = next((t["path"] for t in lib["tracks"] if t["path"].startswith(picked[0]["path"] if picked else "~")), None)
        check(song is not None, "a library song comes from the picked folder")

        print("After a reload")
        page.reload()
        page.wait_for_selector(".act .list [role=radio]", timeout=60000)
        check(page.locator(".act .list [role=radio]").count() == n_songs, "the folder's songs are still there")

        print("A render with a song from the drive")
        st, prop = api("/propose", {"folder": args.footage, "vibe": "kid_reel", "target_duration": "20s", "soundtrack": song})
        check(st == 200, f"propose with the song ({st}: {str(prop)[:200]})")
        if st == 200:
            st, res = api("/render", {"config_path": prop["config_path"], "quality": "draft"})
            check(st == 200 and res.get("ok") is True, f"the render reads the song from the drive ({st}: {str(res)[:300]})")

        print("Remove it")
        page.locator(".act .import .toggle").click()
        rm = page.get_by_role("button", name=f"Remove {picked[0]['display'] if picked else ''} from your library")
        check(rm.count() == 1, "the picked folder has a Remove button")
        if rm.count():
            rm.click()
            check(wait_for(page, "!!document.querySelector('.act .empty')", 30) is not None, "the library is empty again")
        st, lib = api("/music")
        check(not any(d.get("picked") for d in lib["dirs"]), "the server forgot the folder")

        check(not console, f"no console errors ({console[:3]})")
        browser.close()

    print(f"\n{PASSES} passed, {len(FAILS)} failed")
    for f in FAILS:
        print(f"  - {f}")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
