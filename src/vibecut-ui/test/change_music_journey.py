"""Changing the music after the video is made (Playwright, Chromium):

- the Studio has a song bar under the player: the song, its tempo and
  beat score, "Change the music" and "Get more music";
- "Change the music" opens a sheet: songs with the one in the video
  marked, fit badges for the picked one, "Play the song", "Hear it over my
  video" (the video plays muted with the song), and "Use …", which closes
  the sheet and re-renders with that song; afterwards the song bar names
  it and offers "Back to" the previous one;
- "Next song" in the tweak panel offers a song first (Use it / Another /
  ✕) and renders nothing until "Use it";
- the change list names songs, not files;
- "Get more music": the download sites (new tab, noopener), a folder
  typed in is read and its songs come back marked New; a song saved into
  it while away shows up when the window gets the focus back;
- no console errors.

    python test/change_music_journey.py --url http://localhost:8090 \\
        --extra 'C:\\Users\\me\\Music\\New songs' --extra-dir /mnt/new-songs \\
        --sample /mnt/demo/music/a.mp3

`--extra` is a folder (display path) the server can read; `--extra-dir`
is the same folder as this script sees it, where it copies `--sample`
under new names. Exits non-zero on any failed check.
"""

import argparse
import json
import shutil
import sys
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


def jobs():
    with urllib.request.urlopen(BASE + "/api/jobs", timeout=30) as r:
        return json.loads(r.read())["jobs"]


def wait_for(page, js: str, secs: float):
    try:
        return page.wait_for_function(js, timeout=secs * 1000)
    except Exception:  # noqa: BLE001
        return None


def wait_idle(page, secs=600):
    """Until no render is queued or running and the Studio shows a video."""
    return wait_for(
        page,
        "() => !document.querySelector('.studio .overlay') && !!document.querySelector('.studio video')",
        secs,
    )


def song_title(page) -> str:
    return (page.locator(".songbar .sb-title").text_content() or "").replace("♪", "").strip()


def main() -> int:
    global BASE
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default=BASE)
    ap.add_argument("--extra", required=True)
    ap.add_argument("--extra-dir", required=True)
    ap.add_argument("--sample", required=True)
    args = ap.parse_args()
    BASE = args.url.rstrip("/")

    with sync_playwright() as p:
        browser = p.chromium.launch(args=["--autoplay-policy=no-user-gesture-required"])
        ctx = browser.new_context(viewport={"width": 1300, "height": 900})
        page = ctx.new_page()
        console: list[str] = []
        page.on("console", lambda m: console.append(m.text) if m.type == "error" else None)
        page.on("pageerror", lambda e: console.append(str(e)))
        page.goto(BASE + "/")

        print("Make a first video")
        page.wait_for_selector("h2#trip-h")
        check(page.get_by_role("button", name="Free music…").count() == 1, "Create's song step links to free music")
        page.get_by_role("radio", name="Coastline Bike Ride").first.click()
        page.wait_for_selector(".strip .clip", timeout=60000)
        page.get_by_role("button", name="Make my video").click()
        page.wait_for_selector(".studio", timeout=600000)
        check(wait_idle(page) is not None, "the first video is made")

        print("The song bar")
        bar = page.locator(".songbar")
        first = song_title(page)
        check(bar.count() == 1 and first != "", f"a song bar under the player names the song ({first!r})")
        check("BPM" in (bar.text_content() or "") and "on the beat" in (bar.text_content() or ""), "…with its tempo and beat score")
        check(bar.get_by_role("button", name="Back to", exact=False).count() == 0, "no 'Back to' before a song change")

        print("Change the music")
        bar.get_by_role("button", name="Change the music").click()
        dlg = page.locator("dialog[open]")
        dlg.wait_for()
        check(dlg.get_by_role("heading", name="Change the music").count() == 1, "the sheet opens")
        radios = dlg.get_by_role("radiogroup", name="Songs").get_by_role("radio")
        n = radios.count()
        check(n >= 2, f"it lists the songs ({n})")
        now = dlg.locator("[role=radio][aria-checked=true]")
        check("In the video" in (now.text_content() or ""), "the song in the video is marked and chosen")
        focused = page.evaluate("() => document.activeElement?.getAttribute('aria-checked')")
        check(focused == "true", "the focus starts on it")
        page.keyboard.press("ArrowDown")
        target = (dlg.locator("[role=radio][aria-checked=true] .t").text_content() or "").strip()
        target = target.replace("In the video", "").replace("New", "").strip()
        check(target != first, f"arrows choose another song ({target!r})")
        check(dlg.locator(".fit li").count() >= 1, f"fit badges for it ({dlg.locator('.fit').text_content()!r})")
        dlg.get_by_role("button", name="▶ Hear it over my video").click()
        playing = wait_for(page, "() => { const v = document.querySelector('dialog[open] video.preview'); return v && !v.paused && v.muted; }", 10)
        check(playing is not None, "Hear it over my video plays the video, muted")
        dlg.get_by_role("button", name="❚❚ Stop").first.click()
        check(page.evaluate("() => document.querySelector('dialog[open] video.preview')?.paused") is True, "…and Stop stops it")
        before = len([j for j in jobs() if j["kind"] == "render"])
        dlg.get_by_role("button", name=f"Use {target}").click()
        check(wait_for(page, "() => !document.querySelector('dialog[open]')", 5) is not None, "Use closes the sheet")
        check(wait_for(page, f"() => document.querySelectorAll('.studio .overlay').length > 0", 20) is not None
              or len([j for j in jobs() if j["kind"] == "render"]) > before, "…and re-renders")
        check(wait_idle(page) is not None, "the new version is made")
        page.wait_for_timeout(800)
        check(song_title(page) == target, f"the song bar names the new song ({song_title(page)!r})")
        back = bar.get_by_role("button", name=f"Back to {first}")
        check(back.count() == 1, f"…and offers 'Back to {first}'")
        changes = page.locator(".tweaks .changes").text_content() or ""
        check(".mp3" not in changes and ".wav" not in changes, f"the change list names songs, not files ({changes.strip()!r})")

        print("Next song asks first")
        renders = len([j for j in jobs() if j["kind"] == "render"])
        page.locator(".tweaks .tweak.song").click()
        offer = page.locator(".song-offer")
        offer.wait_for()
        t1 = offer.locator("strong").text_content()
        check(page.evaluate("() => document.activeElement?.textContent.trim()") == "Use it", f"it offers a song first ({t1!r}), focus on Use it")
        offer.get_by_role("button", name="Another").click()
        t2 = offer.locator("strong").text_content()
        check(t2 != t1 or len(radios.all()) <= 2, f"'Another' offers a different song ({t2!r})")
        offer.get_by_role("button", name="Keep the current song").click()
        check(offer.count() == 0, "✕ leaves the song as it is")
        page.wait_for_timeout(1500)
        check(len([j for j in jobs() if j["kind"] == "render"]) == renders, "…and nothing was rendered")

        print("Get more music")
        bar.get_by_role("button", name="Get more music").click()
        dlg = page.locator("dialog[open]")
        dlg.wait_for()
        links = dlg.locator(".sources a")
        k = links.count()
        check(k >= 5, f"the download sites are listed ({k})")
        check(all((links.nth(i).get_attribute("target") == "_blank" and "noopener" in (links.nth(i).get_attribute("rel") or "")) for i in range(k)),
              "…each opens in a new tab, safely")
        shutil.copy(args.sample, f"{args.extra_dir}/Fresh Download One.mp3")
        dlg.locator("input[placeholder^='Or type a folder']").fill(args.extra)
        dlg.get_by_role("button", name="Add", exact=True).click()
        added = wait_for(page, "() => /^Added .*: 1 song\\.$/.test(document.querySelector('dialog[open] .status')?.textContent || '')", 120)
        check(added is not None, "a typed folder is read")
        check(dlg.get_by_role("tab", name="Your songs", exact=False).get_attribute("aria-selected") == "true", "…and the sheet shows the songs")
        new_rows = dlg.locator("[role=radio]", has=page.locator(".tag.new"))
        check(new_rows.count() == 1 and "Fresh Download One" in (new_rows.first.text_content() or ""), "the new song is marked New")
        check(dlg.locator("[role=radio][aria-checked=true]").first.text_content().find("Fresh Download One") >= 0, "…and chosen")

        print("Back from a download tab")
        shutil.copy(args.sample, f"{args.extra_dir}/Fresh Download Two.mp3")
        page.wait_for_timeout(3200)  # the rescan waits 3 s between looks
        page.evaluate("() => window.dispatchEvent(new Event('focus'))")
        found = wait_for(page, "() => [...document.querySelectorAll('dialog[open] [role=radio]')].some(r => r.textContent.includes('Fresh Download Two') && r.querySelector('.tag.new'))", 60)
        check(found is not None, "a song saved while away shows up, marked New, when the window gets the focus back")
        dlg.get_by_role("button", name="Close").click()
        page.wait_for_timeout(300)
        bar.get_by_role("button", name="Change the music").click()
        dlg = page.locator("dialog[open]")
        dlg.wait_for()
        page.wait_for_timeout(500)
        check(dlg.locator(".tag.new").count() == 0, "once seen, songs aren't New any more")
        dlg.get_by_role("button", name="Close").click()

        check(not console, f"no console errors ({console[:3]})")
        browser.close()

    print(f"\n{PASSES} passed, {len(FAILS)} failed")
    for f in FAILS:
        print(f"  - {f}")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
