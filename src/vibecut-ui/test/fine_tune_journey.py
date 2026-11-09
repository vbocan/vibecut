"""Fine-tune: the advanced page built on the project's TOML (Playwright):

- the top bar has "Fast previews" (a switch) and "Fine-tune";
- Fine-tune opens a full page: the video, a plan of the edit, the
  changes from the vibe, and controls grouped by goal (Story, Rhythm,
  Music, Look, Titles, What matters, Export) with the vibe's setting
  shown for each;
- a slider change is written into the TOML in place: the comments the
  proposal wrote stay, numbers are written short, the change is listed
  ("Longest shot … (vibe: 7 s)") with a Reset, its group gets a dot and
  the plan is made again without rendering;
- a choice (Beat burst → Never) and a Reset on one control work;
- the TOML view edits the same file: a hand-written comment and value
  survive, show up as a change, and stay through later control changes;
- a change that breaks the config is refused with a message and nothing
  is written;
- "Render draft" closes the page and renders the edit; the new version
  plays in the Studio;
- "Reset all" goes back to the vibe as it comes;
- no console errors.

    python test/fine_tune_journey.py --url http://localhost:8090

Exits non-zero on any failed check.
"""

import argparse
import json
import re
import sys
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


def api(path: str):
    with urllib.request.urlopen(BASE + "/api" + path, timeout=60) as r:
        return json.loads(r.read())


def jobs():
    return api("/jobs")["jobs"]


def wait_for(page, js: str, secs: float):
    try:
        return page.wait_for_function(js, timeout=secs * 1000)
    except Exception:  # noqa: BLE001
        return None


def toml(cfg_path: str) -> str:
    return api("/config?config_path=" + urllib.parse.quote(cfg_path))["config_toml"]


def based(page) -> str:
    return page.locator("dialog.drawer .based").text_content() or ""


def main() -> int:
    global BASE
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default=BASE)
    args = ap.parse_args()
    BASE = args.url.rstrip("/")

    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_context(viewport={"width": 1400, "height": 900}).new_page()
        console: list[str] = []
        page.on("console", lambda m: console.append(m.text) if m.type == "error" else None)
        page.on("pageerror", lambda e: console.append(str(e)))
        page.goto(BASE + "/")

        print("Make a first video")
        page.wait_for_selector("h2#trip-h")
        sw = page.get_by_role("switch", name="Fast previews")
        check(sw.count() == 1 and sw.get_attribute("aria-checked") == "true", "the top bar has Fast previews, on")
        page.get_by_role("radio", name="Coastline Bike Ride").first.click()
        page.wait_for_selector(".strip .clip", timeout=60000)
        page.get_by_role("button", name="Make my video").click()
        page.wait_for_selector(".studio", timeout=600000)
        check(wait_for(page, "() => !document.querySelector('.studio .overlay') && !!document.querySelector('.studio video')", 600) is not None,
              "the first video is made")
        cfg_path = next(j for j in jobs() if j["kind"] == "render")["config_path"]
        original = toml(cfg_path)

        print("Open Fine-tune")
        page.get_by_role("button", name="Fine-tune").click()
        dlg = page.locator("dialog.drawer[open]")
        dlg.wait_for()
        check(dlg.get_by_role("heading", name="Coastline Bike Ride", exact=False).count() >= 1, "it names the video")
        check(wait_for(page, "() => /Based on .+ · /.test(document.querySelector('dialog.drawer .based')?.textContent || '')", 30) is not None,
              f"it says which vibe it's based on ({based(page).strip()!r})")
        check(dlg.locator("video.ft-video").count() == 1, "the video is on the page")
        planned = wait_for(page, "() => /\\d+ shots/.test(document.querySelector('dialog.drawer .plan')?.textContent || '')", 120)
        check(planned is not None, "the edit is planned without rendering")
        groups = [t.strip().split(",")[0] for t in dlg.get_by_role("tablist", name="Settings groups").get_by_role("tab").all_text_contents()]
        check(groups == ["Story", "Rhythm", "Music", "Look", "Titles", "What matters", "Export"], f"settings are grouped by goal ({groups})")
        n0 = len(api("/config/defaults?config_path=" + urllib.parse.quote(cfg_path))["changes"])

        print("A slider")
        before_plans = page.evaluate("() => performance.getEntriesByType('resource').filter(e => e.name.endsWith('/api/plan')).length")
        dlg.get_by_role("tab", name="Rhythm").click()
        sl = dlg.locator("#ctl-selection\\.max_clip")
        check("Family chronicle: 7 s." in (dlg.locator("#help-selection\\.max_clip").text_content() or ""), "the vibe's setting is shown")
        sl.focus()
        for _ in range(4):
            page.keyboard.press("ArrowLeft")
        sl.dispatch_event("change")
        check(wait_for(page, f"() => /{n0 + 1} change/.test(document.querySelector('dialog.drawer .based')?.textContent || '')", 30) is not None,
              f"the change is counted ({based(page).strip()!r})")
        text = toml(cfg_path)
        check('max_clip = "5s"' in text.replace("  ", " ") or 'max_clip     = "5s"' in text or "\"5s\"" in text, "it is written into the TOML")
        comments = [l for l in original.splitlines() if l.strip().startswith("#")]
        check(all(c in text for c in comments), f"the proposal's comments stay ({len(comments)})")
        check("0000000" not in text, "numbers are written short")
        row = dlg.locator(".changes li", has_text="Longest shot")
        check(row.count() == 1 and "vibe: 7 s" in (row.text_content() or ""), f"the change is listed with the vibe's value ({row.text_content() if row.count() else ''!r})")
        rhythm = dlg.get_by_role("tablist", name="Settings groups").get_by_role("tab", name="Rhythm", exact=False)
        check(rhythm.locator(".dot").count() == 1 and "1 changed" in (rhythm.text_content() or ""),
              f"the group shows it has a change ({rhythm.text_content()!r})")
        check(dlg.get_by_role("button", name="Reset Longest shot to 7 s").count() == 1, "the control offers a Reset")
        replanned = wait_for(page, f"() => performance.getEntriesByType('resource').filter(e => e.name.endsWith('/api/plan')).length > {before_plans}", 20)
        check(replanned is not None, "the edit is planned again")

        print("A choice and a Reset")
        dlg.get_by_role("tab", name="Music").click()
        dlg.get_by_role("radiogroup", name="Beat burst").get_by_role("radio", name="Never").click()
        check(wait_for(page, "() => document.querySelector('dialog.drawer [role=radiogroup][aria-label=\"Beat burst\"] [aria-checked=true]')?.textContent.trim() === 'Never'", 30) is not None,
              "Beat burst → Never is chosen")
        check('beat_burst = "off"' in toml(cfg_path).replace("  ", " ").replace("  ", " "), "…and written")
        dlg.get_by_role("button", name="Reset Beat burst to When it suits").click()
        check(wait_for(page, "() => document.querySelector('dialog.drawer [role=radiogroup][aria-label=\"Beat burst\"] [aria-checked=true]')?.textContent.trim() === 'When it suits'", 30) is not None,
              "Reset puts the vibe's setting back")

        print("The TOML view")
        dlg.get_by_role("tab", name="TOML").click()
        area = dlg.locator("#raw-toml")
        t = area.input_value()
        area.fill(re.sub(r"variety\s*=\s*[0-9.]+", "variety = 0.3", t.replace("[selection]", "[selection]\n# shots for the long ride", 1), count=1))
        check(dlg.get_by_text("Unsaved changes").count() == 1, "an edit is unsaved until saved")
        dlg.get_by_role("button", name="Save TOML").click()
        check(wait_for(page, "() => /Saved/.test(document.querySelector('dialog.drawer .toml-foot')?.textContent || '')", 30) is not None, "Save TOML saves it")
        check(wait_for(page, "() => [...document.querySelectorAll('dialog.drawer .changes li')].some(l => l.textContent.includes('Shake it up'))", 30) is not None,
              "a hand edit shows up as a change")
        dlg.get_by_role("tab", name="Controls").click()
        dlg.get_by_role("tab", name="Look").click()
        dlg.get_by_role("radiogroup", name="Between shots").get_by_role("radio", name="Dissolve").click()
        wait_for(page, "() => document.querySelector('dialog.drawer [role=radiogroup][aria-label=\"Between shots\"] [aria-checked=true]')?.textContent.trim() === 'Dissolve'", 30)
        text = toml(cfg_path)
        check("# shots for the long ride" in text and "variety = 0.3" in text and '"dissolve"' in text,
              "the hand-written comment and value stay through later control changes")

        print("A broken change is refused")
        n_console = len(console)
        dlg.get_by_role("tab", name="TOML").click()
        area = dlg.locator("#raw-toml")
        before = toml(cfg_path)
        area.fill(area.input_value().replace("[selection]", "[selection]\nmax_clip_typo = 3", 1))
        dlg.get_by_role("button", name="Save TOML").click()
        err = wait_for(page, "() => !!document.querySelector('dialog.drawer [role=alert]')", 20)
        check(err is not None, "a key the config doesn't know is refused with a message")
        check(toml(cfg_path) == before, "…and nothing is written")
        # The refusal is a 400 the browser logs; it's the expected answer.
        del console[n_console:]
        area.fill(before)
        dlg.get_by_role("button", name="Save TOML").click()
        page.wait_for_timeout(800)
        dlg.get_by_role("tab", name="Controls").click()

        print("Render the tuned edit")
        renders = len([j for j in jobs() if j["kind"] == "render"])
        dlg.get_by_role("button", name="Render draft").click()
        check(wait_for(page, "() => !document.querySelector('dialog.drawer[open]')", 10) is not None, "Render draft closes the page")
        check(wait_for(page, "() => !document.querySelector('.studio .overlay') && !!document.querySelector('.studio video')", 600) is not None
              and len([j for j in jobs() if j["kind"] == "render"]) > renders, "the tuned edit is rendered")

        print("Reset all")
        page.get_by_role("button", name="Fine-tune").click()
        dlg = page.locator("dialog.drawer[open]")
        dlg.wait_for()
        wait_for(page, "() => /change/.test(document.querySelector('dialog.drawer .based')?.textContent || '')", 30)
        dlg.get_by_role("button", name="Reset all").click()
        check(wait_for(page, "() => /no changes/.test(document.querySelector('dialog.drawer .based')?.textContent || '')", 30) is not None,
              f"Reset all goes back to the vibe as it comes ({based(page).strip()!r})")
        check("# shots for the long ride" in toml(cfg_path), "…and keeps comments")
        page.keyboard.press("Escape")

        check(not console, f"no console errors ({console[:3]})")
        browser.close()

    print(f"\n{PASSES} passed, {len(FAILS)} failed")
    for f in FAILS:
        print(f"  - {f}")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
