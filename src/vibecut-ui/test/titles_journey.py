"""Opening and closing cards (Playwright, Chromium):

- the Studio's "Titles" opens Fine-tune on the Titles group;
- the opening card: its preview shows its text; text, size, colour,
  position (the anchor grid; a new anchor drops old nudges), a nudge,
  background, length and "Over the first shot" are each written into the
  TOML in place;
- lines: "Add a line of text" adds a `[[intro.layers]]` entry under the
  last one; appear / disappear times, a box behind it and Remove work;
- the closing card: "Add a closing card" writes `[outro]` with its
  `[[outro.layers]]`; its text is edited; a render then has the closing
  card on its timeline; "Remove the closing card" takes it out;
- no console errors.

    python test/titles_journey.py --url http://localhost:8090
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


def toml(cfg_path: str) -> str:
    return api("/config?config_path=" + urllib.parse.quote(cfg_path))["config_toml"]


def flat(text: str) -> str:
    """TOML with runs of spaces squeezed, for alignment-proof checks."""
    return re.sub(r"[ \t]+", " ", text)


def wait_for(page, js: str, secs: float):
    try:
        return page.wait_for_function(js, timeout=secs * 1000)
    except Exception:  # noqa: BLE001
        return None


def wait_toml(cfg_path: str, page, pred, secs=20) -> bool:
    for _ in range(int(secs * 4)):
        if pred(flat(toml(cfg_path))):
            return True
        page.wait_for_timeout(250)
    return False


def set_range(loc, value):
    loc.evaluate("(el, v) => { el.value = String(v); el.dispatchEvent(new Event('input', {bubbles: true})); el.dispatchEvent(new Event('change', {bubbles: true})); }", value)


def set_color(loc, value):
    loc.evaluate("(el, v) => { el.value = v; el.dispatchEvent(new Event('input', {bubbles: true})); el.dispatchEvent(new Event('change', {bubbles: true})); }", value)


def main() -> int:
    global BASE
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default=BASE)
    args = ap.parse_args()
    BASE = args.url.rstrip("/")

    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_context(viewport={"width": 1400, "height": 950}).new_page()
        console: list[str] = []
        page.on("console", lambda m: console.append(m.text) if m.type == "error" else None)
        page.on("pageerror", lambda e: console.append(str(e)))
        page.goto(BASE + "/")

        print("Make a first video")
        page.wait_for_selector("h2#trip-h")
        page.get_by_role("radio", name="Coastline Bike Ride").first.click()
        page.wait_for_selector(".strip .clip", timeout=60000)
        page.get_by_role("button", name="Make my video").click()
        page.wait_for_selector(".studio", timeout=600000)
        check(wait_for(page, "() => !document.querySelector('.studio .overlay') && !!document.querySelector('.studio video')", 600) is not None, "the first video is made")
        cfg = next(j for j in jobs() if j["kind"] == "render")["config_path"]

        print("Titles from the Studio")
        page.get_by_role("button", name="Titles", exact=True).click()
        dlg = page.locator("dialog.drawer[open]")
        dlg.wait_for()
        check(dlg.get_by_role("tab", name="Titles").get_attribute("aria-selected") == "true", "Fine-tune opens on Titles")
        ed = dlg.locator(".titles")
        ed.locator(".preview").wait_for()
        first_text = ed.locator(".preview .tl").first.text_content() or ""
        check("Coastline Bike Ride" in first_text, f"the preview shows the opening text ({first_text!r})")

        print("The opening line")
        txt = ed.locator("input.text")
        txt.fill("Ride along the coast")
        txt.dispatch_event("change")
        check(wait_toml(cfg, page, lambda t: 'content = "Ride along the coast"' in t), "the text is written")
        check("Ride along the coast" in (ed.locator(".preview .tl").first.text_content() or ""), "…and previewed")
        set_range(ed.get_by_label("Size", exact=True), 120)
        check(wait_toml(cfg, page, lambda t: "size = 120" in t), "the size is written")
        set_color(ed.get_by_label("Colour", exact=True), "#ff3d81")
        check(wait_toml(cfg, page, lambda t: 'color = "#ff3d81"' in t), "the colour is written")
        ed.locator(".grid").get_by_role("radio", name="Top left").click()
        check(wait_toml(cfg, page, lambda t: 'position = "top-left"' in t), "the position is written")
        style = ed.locator(".preview .tl").first.get_attribute("style") or ""
        check("left:" in style and "top:" in style and "50%" not in style, f"…and the preview moves there ({style[:80]!r})")
        set_range(ed.locator(".nudge input[type=range]").nth(1), 60)
        check(wait_toml(cfg, page, lambda t: "y_offset = 60" in t), "a nudge is written")
        ed.locator(".grid").get_by_role("radio", name="Centre", exact=True).click()
        check(wait_toml(cfg, page, lambda t: 'position = "center"' in t and "y_offset = 60" not in t), "a new anchor drops the old nudge")

        print("The card")
        set_color(ed.get_by_label("Background"), "#203040")
        check(wait_toml(cfg, page, lambda t: 'bg_color = "#203040"' in t), "the background is written")
        set_range(ed.get_by_label("Length", exact=True), 3.5)
        check(wait_toml(cfg, page, lambda t: 'duration = "3s 500ms"' in t or 'duration = "3500ms"' in t), "the length is written")
        ed.get_by_role("radio", name="Over the first shot").click()
        check(wait_toml(cfg, page, lambda t: 'mode = "overlay"' in t), "Over the first shot is written")
        check(ed.locator(".preview.overlay").count() == 1, "…and the preview shows it over footage")
        ed.get_by_role("radio", name="A card").click()
        check(wait_toml(cfg, page, lambda t: 'mode = "card"' in t), "…and back to a card")

        print("Lines")
        ed.get_by_role("button", name="Add a line of text").click()
        check(wait_toml(cfg, page, lambda t: t.count("[[intro.layers]]") == 2 and 'content = "New line"' in t), "a line is added as [[intro.layers]]")
        check(wait_for(page, "() => document.querySelectorAll('dialog.drawer .titles .preview .tl').length === 2", 20) is not None, "…and previewed")
        check(ed.get_by_role("tablist", name="Lines").get_by_role("tab", name="New line").get_attribute("aria-selected") == "true", "…and chosen for editing")
        set_range(ed.get_by_label("Appears at", exact=True), 1.5)
        check(wait_toml(cfg, page, lambda t: 'appear_at = "1s 500ms"' in t or 'appear_at = "1500ms"' in t), "its appear time is written")
        ed.get_by_label("Box behind").check()
        check(wait_toml(cfg, page, lambda t: "background" in t and "#00000099" in t), "a box behind it is written")
        ed.get_by_role("button", name="Remove this line").click()
        check(wait_toml(cfg, page, lambda t: t.count("[[intro.layers]]") == 1 and "New line" not in t), "Remove takes the line out")

        print("The closing card")
        ed.get_by_role("tab", name="Closing (none)").click()
        ed.get_by_role("button", name="Add a closing card").click()
        check(wait_toml(cfg, page, lambda t: "[outro]" in t and "[[outro.layers]]" in t and "outro = {" not in t), "a closing card is written as [outro] tables")
        txt = ed.locator("input.text")
        txt.fill("See you next summer")
        txt.dispatch_event("change")
        check(wait_toml(cfg, page, lambda t: 'content = "See you next summer"' in t), "its text is written")
        renders = len([j for j in jobs() if j["kind"] == "render"])
        dlg.get_by_role("button", name="Render draft").click()
        new = None
        for _ in range(600):
            rs = [j for j in jobs() if j["kind"] == "render"]
            if len(rs) > renders and rs[0]["state"] in ("done", "failed", "cancelled"):
                new = rs[0]
                break
            page.wait_for_timeout(1000)
        check(new is not None and new["state"] == "done", f"the edit renders ({new and new['state']})")
        wait_for(page, "() => !document.querySelector('.studio .overlay') && !!document.querySelector('.studio video')", 60)
        done = new or next(j for j in jobs() if j["kind"] == "render" and j["state"] == "done")
        manifest = done.get("version_manifest_path") or done.get("manifest_path")
        with urllib.request.urlopen(BASE + "/api/output?path=" + urllib.parse.quote(manifest), timeout=60) as r:
            m = json.loads(r.read())
        check(bool((m.get("timeline") or {}).get("outro")), "the render has the closing card on its timeline")

        page.get_by_role("button", name="Titles", exact=True).click()
        dlg = page.locator("dialog.drawer[open]")
        dlg.wait_for()
        ed = dlg.locator(".titles")
        ed.get_by_role("tab", name="Closing").click()
        ed.get_by_role("button", name="Remove the closing card").click()
        check(wait_toml(cfg, page, lambda t: "[outro]" not in t), "Remove takes the closing card out")
        check("Ride along the coast" in toml(cfg), "…and leaves the opening card")
        page.keyboard.press("Escape")

        check(not console, f"no console errors ({console[:3]})")
        browser.close()

    print(f"\n{PASSES} passed, {len(FAILS)} failed")
    for f in FAILS:
        print(f"  - {f}")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
