"""Keyboard / screen-reader pass over the controls the render-jobs,
platform-format and music work added (Playwright, Chromium), and the
mid-render "Stop and apply this instead" offers:

- Share sheet: the platform row is one radio group (one tab stop, arrows
  move without starting a version, Space picks it), the version's outcome
  is read from a status line, a second request for the same version joins
  the running job, and reformat jobs are listed with kind "reformat";
- a keyboard shortcut pressed mid-render offers "Stop and apply" in the
  tweak panel, announced, reachable with Tab, and the focus survives it;
- the cutting room's Stop says what it does in a tooltip, keeps the focus
  while stopping and hands it on (not to the page) once the cutting room
  goes;
- a song picked mid-render shows its offer in the song list; Escape on it
  keeps rendering and the focus goes back to the song;
- the Renders list labels each row's kind and buttons, and its Stop keeps
  the focus; a stopped reformat leaves no file behind;
- the music import view: aria-controls names its panel, "Look again"
  keeps the focus and announces what it found — also when the library was
  empty and the song list replaces the empty state (the library is faked
  empty through page.route) — and every song radio is described by its
  licence note;
- a tweak key pressed while a Stop is on its way is refused with a notice,
  the tweak buttons report themselves unavailable meanwhile (the cancel
  request is held through page.route), and a notice with the focus or the
  pointer on it doesn't time out, and hands the focus back when dismissed;
- Shuffle ('s') starts a re-render on the first press, with no error shown.

    python test/new_controls_keyboard.py --url http://localhost:8090

Needs the demo set (footage folder "Coastline Bike Ride", at least two
songs). It renders one ~30 s video and a few re-renders. Exits non-zero
on any failed check.
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
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read())


def jobs() -> list[dict]:
    return api("/jobs")["jobs"]


ACTIVE = """() => {
  const el = document.activeElement;
  if (!el || el === document.body || el === document.documentElement) return null;
  const name = (el.getAttribute('aria-label') ?? el.textContent ?? '').trim().replace(/\\s+/g, ' ');
  return {
    tag: el.tagName.toLowerCase(),
    role: el.getAttribute('role'),
    name: name.slice(0, 120),
    disabled: el.getAttribute('aria-disabled'),
    checked: el.getAttribute('aria-checked'),
    group: el.closest('[role=radiogroup]')?.getAttribute('aria-label')
      ?? (el.closest('[role=radiogroup]')?.getAttribute('aria-labelledby')
        ? document.getElementById(el.closest('[role=radiogroup]').getAttribute('aria-labelledby'))?.textContent.trim()
        : null),
  };
}"""


def active(page):
    return page.evaluate(ACTIVE)


def press(page, key, settle=150):
    page.keyboard.press(key)
    page.wait_for_timeout(settle)
    return active(page)


def tab_to(page, pred, what, limit=160, back=False):
    for _ in range(limit):
        a = press(page, "Shift+Tab" if back else "Tab", settle=40)
        if a and pred(a):
            return a
    check(False, f"Tab reaches {what}")
    return None


def blur(page):
    page.evaluate("document.activeElement?.blur()")


def rendering(page) -> bool:
    return page.evaluate("!!document.querySelector('button.stop')")


def wait_render_starts(page, timeout=30):
    t0 = time.time()
    while time.time() - t0 < timeout:
        if rendering(page):
            return True
        page.wait_for_timeout(100)
    notes = [t for t in status_texts(page, "body") if t]
    print(f"       (no render started; status lines: {notes}; focus: {active(page)})", flush=True)
    return False


def wait_render_ends(page, timeout=600):
    t0 = time.time()
    while time.time() - t0 < timeout:
        if not rendering(page) and page.is_visible("text=Download"):
            page.wait_for_timeout(500)
            return True
        page.wait_for_timeout(250)
    return False


ERRORS_SHOWN = r"""[...document.querySelectorAll('[role=alert], [role=status], .error')]
  .map((e) => e.textContent.trim())
  .filter((t) => t && /could not|couldn.t|error|failed|HTTP \d{3}/i.test(t))"""


def errors_shown(page) -> list[str]:
    """Error text on the page: alerts, and status lines that carry a server error."""
    return page.evaluate(ERRORS_SHOWN)


def status_texts(page, scope: str) -> list[str]:
    return page.evaluate(
        f"[...document.querySelectorAll({json.dumps(scope + ' [role=status]')})].map((e) => e.textContent.trim())"
    )


def main() -> int:
    global BASE
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default=BASE)
    ap.add_argument("--render-timeout", type=int, default=600)
    args = ap.parse_args()
    BASE = args.url.rstrip("/")

    with sync_playwright() as pw:
        browser = pw.chromium.launch()
        ctx = browser.new_context(viewport={"width": 1400, "height": 1000}, reduced_motion="reduce")
        # Full-quality renders: Fast previews (on by default) makes drafts,
        # and Share on a draft finishes it first (draft_journey.py covers that).
        ctx.add_init_script("try { localStorage.setItem('vibecut.fastPreviews', 'off') } catch {}")
        page = ctx.new_page()
        page.on("pageerror", lambda e: FAILS.append(f"page error: {e}"))

        print("Music: 'Look again' on an empty library")
        # The library answers empty while `empty[0]` is set, else for real.
        empty = [True]
        fake = {"tracks": [], "warnings": [], "import_dir": "/projects/music",
                "dirs": [{"path": "/projects/music", "collection": "user", "exists": True}]}

        def music_route(route):
            if empty[0]:
                route.fulfill(status=200, content_type="application/json", body=json.dumps(fake))
            else:
                route.continue_()

        def is_music_list(url: str) -> bool:
            return urllib.parse.urlparse(url).path == "/api/music"

        page.route(is_music_list, music_route)
        page.goto(BASE + "/")
        page.wait_for_selector(".act .empty")
        toggle = page.locator(".act .import .toggle")
        toggle.click()
        ctl = toggle.get_attribute("aria-controls")
        look = page.locator(f"[id='{ctl}'] button", has_text="Look again")
        look.focus()
        empty[0] = False
        a = press(page, "Enter", settle=50)
        check(a is not None and a["name"] in ("Look again", "Looking…"), f"empty library: 'Look again' keeps the focus while it looks ({a})")
        found = f"""[...document.querySelectorAll("[id='{ctl}'] [role=status]")].some((e) => / songs? in your library/.test(e.textContent))"""
        try:
            page.wait_for_function(found, timeout=60000)
            check(True, "empty library: the import panel announces the songs it found")
        except Exception:  # noqa: BLE001
            check(False, f"empty library: the import panel announces the songs it found ({status_texts(page, '.act')})")
        a = active(page)
        check(a is not None and a["name"] == "Look again", f"empty library: …the focus is still on 'Look again' ({a and a['name']!r})")
        check(toggle.get_attribute("aria-expanded") == "true" and page.locator(f"[id='{ctl}']").count() == 1,
              "empty library: …the panel the user opened is still open")
        check(page.locator(".act .list [role=radio]").count() >= 2, "empty library: …and the songs are listed")
        # Empty again, then the empty state's own "Look again".
        empty[0] = True
        look.focus()
        press(page, "Enter", settle=50)
        page.wait_for_selector(".act .empty", timeout=30000)
        page.wait_for_timeout(300)
        page.locator(".act .empty button").focus()
        empty[0] = False
        a = press(page, "Enter", settle=50)
        # (A fast answer may already have moved it on to the list.)
        check(a is not None and (a["name"] in ("Look again", "Looking…") or a["role"] == "radio"),
              f"empty state: its 'Look again' keeps the focus while it looks ({a})")
        page.wait_for_selector(".act .list [role=radio]", timeout=60000)
        page.wait_for_timeout(300)
        a = active(page)
        check(a is not None and a["role"] == "radio" and a["checked"] == "true",
              f"empty state: once the list replaces it, the focus is on the picked song ({a and a['name'][:40]!r})")
        st = [t for t in status_texts(page, ".act") if "in your library" in t]
        check(any("songs in your library" in t for t in st), f"empty state: …and what it found is announced ({st})")
        toggle.click()
        page.unroute(is_music_list)

        print("Make a first video")
        page.get_by_role("radio", name="Coastline Bike Ride").first.click()
        page.wait_for_selector(".list [role=radio]")
        page.get_by_role("button", name="Make my video").click()
        page.wait_for_selector(".studio", timeout=args.render_timeout * 1000)
        check(wait_render_ends(page, args.render_timeout), "the first video is made")
        render = next(j for j in jobs() if j["kind"] == "render" and j["state"] == "done")
        source = render["output_path"]
        # The Share sheet works from the version on screen (the render's kept copy).
        shown = render.get("version_path") or source
        check(all(j.get("kind") in ("render", "reformat") for j in jobs()), "GET /api/jobs gives every job a kind")

        print("Music: song radios are described by their licence note")
        page.locator(".rail .disclosure").click()
        page.wait_for_selector(".rail .list [role=radio]")
        described = page.evaluate(
            """[...document.querySelectorAll('.rail .list .pick[role=radio]')].map((b) => {
                 const id = b.getAttribute('aria-describedby');
                 return !!id && (document.getElementById(id)?.textContent.trim().length ?? 0) > 0;
               })"""
        )
        check(len(described) >= 2 and all(described), f"every song radio has a licence description ({described})")

        print("Music: the import view")
        toggle = page.locator(".rail .import .toggle")
        toggle.click()
        ctl = toggle.get_attribute("aria-controls")
        check(
            toggle.get_attribute("aria-expanded") == "true" and bool(ctl) and page.locator(f"[id='{ctl}']").count() == 1,
            f"'Add music' expands and aria-controls names its panel ({ctl!r})",
        )
        look = page.locator(f"[id='{ctl}'] button", has_text="Look again")
        look.focus()
        a = press(page, "Enter", settle=50)
        check(a is not None and a["name"] in ("Look again", "Looking…"), f"'Look again' keeps the focus while it looks ({a})")
        page.wait_for_function(
            f"[...document.querySelectorAll(\"[id='{ctl}'] [role=status]\")].some((e) => /in your library/.test(e.textContent))",
            timeout=60000,
        )
        check(True, "'Look again' announces what it found")
        a = active(page)
        check(a is not None and a["name"] == "Look again", f"…and the focus is still on it ({a and a['name']!r})")
        toggle.click()

        print("Share: the platform row")
        # "Share" alone also matches "Share this style".
        page.get_by_role("button", name="Share", exact=True).click()
        page.wait_for_selector("dialog.sheet[open]")
        n_reformats = len([j for j in jobs() if j["kind"] == "reformat"])
        radios = page.locator("dialog.sheet .platforms [role=radio]")
        check(page.get_attribute("dialog.sheet .platforms", "role") == "radiogroup", "the platform row is a radio group")
        check(radios.count() == 5, f"…of five platforms ({radios.count()})")
        stops = page.evaluate("[...document.querySelectorAll('dialog.sheet .platforms [role=radio]')].filter((r) => r.tabIndex === 0).length")
        check(stops == 1, f"…with one tab stop ({stops})")
        radios.nth(0).focus()
        a = press(page, "ArrowRight")
        check(a is not None and a["name"].startswith("Reels"), f"ArrowRight moves to Reels ({a and a['name']!r})")
        page.wait_for_timeout(500)
        none_checked = page.evaluate("![...document.querySelectorAll('dialog.sheet .platforms [role=radio]')].some((r) => r.getAttribute('aria-checked') === 'true')")
        check(none_checked, "…without picking it")
        check(len([j for j in jobs() if j["kind"] == "reformat"]) == n_reformats, "…or starting a version")
        a = press(page, " ", settle=300)
        check(a is not None and a["checked"] == "true" and a["name"].startswith("Reels"), f"Space picks Reels ({a})")
        st = status_texts(page, "dialog.sheet .formats")
        check(any("Reels" in t for t in st), f"the version's progress is announced ({st})")
        page.wait_for_function(
            "[...document.querySelectorAll('dialog.sheet .formats [role=status]')].some((e) => /version ready/.test(e.textContent))",
            timeout=180000,
        )
        st = status_texts(page, "dialog.sheet .formats")
        check(
            any(t.startswith("Reels / TikTok / Shorts version ready") and ": 1080 by 1920" in t for t in st),
            f"…and so is its outcome ({st})",
        )
        reels = [
            j for j in jobs()
            if j["kind"] == "reformat" and j.get("preset") == "reels" and j.get("source_path") in (shown, source)
        ]
        check(
            len(reels) == 1 and reels[0]["state"] == "done" and reels[0]["source_path"] in (shown, source),
            f"the Reels version is a reformat job of this render ({[(j['state'], j.get('source_path')) for j in reels]})",
        )
        page.keyboard.press("Escape")
        page.wait_for_timeout(300)

        print("Reformat jobs: a second request joins the running one")
        first = api("/reformat", {"output_path": source, "preset": "square", "fit": "pad"})
        second = api("/reformat", {"output_path": source, "preset": "square", "fit": "pad"})
        check(not first["reused"] and second["reused"] and second["job_id"] == first["job_id"],
              f"same version, same job ({first['job_id']}, {second['job_id']}, reused={second['reused']})")
        check(second["kind"] == "reformat", "the answer says kind 'reformat'")

        print("Studio: a shortcut mid-render offers 'Stop and apply'")
        blur(page)
        page.keyboard.press("[")  # Shorter
        check(wait_render_starts(page), "'[' starts a re-render")
        page.wait_for_timeout(800)
        blur(page)
        page.keyboard.press("]")  # Longer, mid-render
        page.wait_for_selector(".tweaks .offer", timeout=5000)
        st = status_texts(page, ".tweaks")
        check(any("Longer" in t and "waiting" in t for t in st), f"the offer is announced ({st})")
        a = tab_to(page, lambda a: a["name"].startswith("Stop and apply"), "'Stop and apply “Longer” instead'")
        if a:
            check(a["disabled"] in (None, "false"), "…which is enabled")
            a = press(page, "Enter", settle=60)
            check(a is not None, f"pressing it doesn't drop the focus on the page ({a and a['name']!r})")
            t0 = time.time()
            while time.time() - t0 < 30 and page.locator(".tweaks .offer").count() > 0:
                page.wait_for_timeout(100)
            page.wait_for_timeout(600)
            a = active(page)
            check(a is not None, f"once the offer is gone the focus is somewhere, not on the page ({a and a['name']!r})")
        check(wait_render_starts(page), "the pending change renders")
        check(wait_render_ends(page, args.render_timeout), "…and finishes")

        print("Studio: Stop keeps the focus")
        blur(page)
        page.keyboard.press("s")  # Shuffle
        # One press, no retry: a Shuffle that fails (it used to, about every
        # other time, on a seed a TOML file can't hold) must fail here.
        check(wait_render_starts(page, timeout=30), "'s' starts a re-render on the first press")
        errs = errors_shown(page)
        check(not errs, f"…with no error shown ({errs})")
        page.wait_for_timeout(1500)
        # Hold the cancel request, so the Stop stays on its way for a while.
        held: list = []
        page.route("**/api/jobs/*/cancel", lambda route: held.append(route))
        # What Stop does is shown on hover too, not only to screen readers.
        title = page.get_attribute("button.stop", "title")
        check(title == "Stop this render and go back to the last version", f"Stop's tooltip says where it goes back to ({title!r})")
        page.locator("button.stop").focus()
        a = press(page, "Enter", settle=80)
        check(a is not None and a["name"] in ("Stop", "Stopping…"), f"Stop keeps the focus while stopping ({a})")
        t0 = time.time()
        while not held and time.time() - t0 < 10:
            page.wait_for_timeout(50)
        check(bool(held), "the Stop sent its cancel request")
        busy = page.evaluate(
            "[...document.querySelectorAll('.tweaks button.tweak:not([disabled]), .tweaks .mix')].map((b) => b.getAttribute('aria-disabled'))"
        )
        check(len(busy) > 0 and all(b == "true" for b in busy), f"while the Stop is on its way the tweaks report themselves unavailable ({busy})")
        check(page.get_attribute("button.stop", "aria-busy") == "true", "…and Stop says it is busy")
        page.keyboard.press("c")  # Calmer: refused for now
        try:
            page.wait_for_function(
                "[...document.querySelectorAll('[role=status]')].some((e) => e.textContent.includes('Still stopping'))",
                timeout=3000,
            )
            check(True, "a tweak key pressed meanwhile says 'Still stopping' instead of vanishing")
        except Exception:  # noqa: BLE001
            check(False, f"a tweak key pressed meanwhile says 'Still stopping' ({[t for t in status_texts(page, 'body') if t][:4]})")
        check(page.locator(".tweaks .offer").count() == 0, "…and isn't kept as an offer")
        for r in held:
            r.continue_()
        page.unroute("**/api/jobs/*/cancel")
        t0 = time.time()
        while time.time() - t0 < 30 and rendering(page):
            page.wait_for_timeout(100)
        try:
            page.wait_for_function(
                "[...document.querySelectorAll('[role=status]')].some((e) => e.textContent.includes('Render stopped'))",
                timeout=10000,
            )
            check(True, "the stop is announced")
        except Exception:  # noqa: BLE001
            st = status_texts(page, "body")
            check(False, f"the stop is announced ({[t for t in st if t][:4]})")
        a = active(page)
        check(a is not None, f"after the stop the focus isn't on the page ({a and a['tag']!r} {a and a['name']!r})")

        print("Notice toast: waits while someone is on it")
        notice = page.locator(".toast.notice")
        dismiss = notice.locator("button[aria-label='Dismiss notice']")
        if notice.count() == 1:
            notice.hover()
            page.wait_for_timeout(6500)  # past its 5 s
            check(notice.count() == 1, "a notice under the pointer stays past its time")
            dismiss.focus()
            page.mouse.move(5, 5)
            page.wait_for_timeout(6500)
            a = active(page)
            check(notice.count() == 1 and a is not None and a["name"] == "Dismiss notice",
                  f"a notice with the keyboard focus stays too ({a and a['name']!r})")
            a = press(page, "Enter", settle=300)
            check(notice.count() == 0, "Dismiss removes it")
            check(a is not None, f"…and hands the focus back, not to the page ({a and a['tag']!r} {a and a['name']!r})")
        else:
            check(False, f"the stop leaves a notice toast ({notice.count()})")
        page.wait_for_timeout(1000)

        print("Studio: a song picked mid-render offers 'Stop and apply' in the song list")
        blur(page)
        page.keyboard.press("e")  # More energy
        check(wait_render_starts(page), "'e' starts a re-render")
        page.wait_for_timeout(800)
        song_list = page.locator(".rail .list")
        checked = song_list.locator("[role=radio][aria-checked=true]")
        checked.focus()
        a = press(page, "ArrowDown")
        check(a is not None and a["role"] == "radio" and a["checked"] != "true", f"arrows move to another song ({a and a['name'][:40]!r})")
        press(page, " ", settle=300)
        song_offer = page.locator(".rail section .offer")
        check(song_offer.count() == 1 and page.locator(".tweaks .offer").count() == 0,
              "the offer shows in the song list, not the tweak panel")
        st = [t for t in status_texts(page, ".rail section") if t]
        check(any(t.startswith("Still rendering") and "Song:" in t for t in st), f"…and is announced ({st})")
        a = tab_to(page, lambda a: a["name"] == "Keep rendering", "'Keep rendering' (Shift+Tab from the list)", limit=20, back=True)
        check(a is not None, "Shift+Tab from the list reaches the offer's 'Keep rendering'")
        a = press(page, "Escape", settle=500)
        check(song_offer.count() == 0, "Escape on the offer keeps rendering")
        check(rendering(page), "…the render goes on")
        a = active(page)
        check(a is not None and a["role"] == "radio" and a["checked"] == "true",
              f"…and the focus goes back to the checked song ({a and a['name'][:40]!r})")
        check(wait_render_ends(page, args.render_timeout), "the render finishes")

        print("Renders list")
        blur(page)
        page.keyboard.press("d")
        page.wait_for_selector("dialog.drawer[open]")
        page.locator("details.renders summary").click()
        page.wait_for_selector("details.renders li[data-job]")
        kinds = page.evaluate("[...document.querySelectorAll('details.renders .kind')].map((e) => e.textContent.trim())")
        check("Render" in kinds and "Reformat" in kinds, f"rows say their kind ({sorted(set(kinds))})")
        watch = page.evaluate("[...document.querySelectorAll('details.renders a')].map((a) => a.getAttribute('aria-label'))")
        check(len(watch) > 0 and all(w and w.startswith("Watch ") and w.endswith("(opens in a new tab)") for w in watch),
              f"Watch links say what and where ({watch[:2]})")
        check(any("Reels" in (w or "") and "version of" in (w or "") for w in watch), "a reformat row names its version")
        # A long version to stop: a 9:16 crop of the render.
        job = api("/reformat", {"output_path": source, "preset": "tiktok", "fit": "crop"})
        page.locator("details.renders button", has_text="Refresh").click()
        page.wait_for_timeout(300)
        stop_btn = page.locator(f"details.renders li[data-job='{job['job_id']}'] button", has_text="Stop")
        try:
            stop_btn.wait_for(timeout=5000)
            label = stop_btn.get_attribute("aria-label")
            check(bool(label) and label.startswith("Stop TikTok (crop) version of"), f"its Stop says which job ({label!r})")
            stop_btn.focus()
            a = press(page, "Enter", settle=60)
            # A quick stop may already have handed it on to Refresh.
            check(
                a is not None and (a["name"].startswith("Stop") or a["name"] == "Refresh"),
                f"the list's Stop keeps the focus ({a and a['name']!r})",
            )
            t0 = time.time()
            while time.time() - t0 < 30 and stop_btn.count() > 0:
                page.wait_for_timeout(100)
            check(stop_btn.count() == 0, "the row follows the job to its end without a Refresh")
            page.wait_for_timeout(500)
            a = active(page)
            check(a is not None and not a["name"].startswith("Stop"), f"…and the focus moves on from the gone Stop ({a and a['name']!r})")
            notes = status_texts(page, "details.renders")
            check(any("stopped" in t for t in notes), f"the outcome is announced ({notes})")
            st = api(f"/render/{job['job_id']}")
            check(st["state"] == "cancelled", f"the reformat job is cancelled ({st['state']})")
            row = page.locator(f"details.renders li[data-job='{job['job_id']}'] .job-state").text_content()
            check("Stopped" in (row or ""), f"its row says Stopped ({row!r})")
            check(not api_exists(job["output_path"]), "the stopped version left no file behind")
        except Exception as e:  # noqa: BLE001
            check(False, f"stopping a reformat from the list: {e}")

        browser.close()
    return report()


def api_exists(path: str) -> bool:
    try:
        urllib.request.urlopen(BASE + "/api/output?path=" + urllib.request.quote(path), timeout=10).close()
        return True
    except Exception:  # noqa: BLE001
        return False


def report() -> int:
    print(f"\n{PASSES} passed, {len(FAILS)} failed")
    for f in FAILS:
        print(f"  - {f}")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
