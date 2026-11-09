"""The Share sheet's framing for vertical, square and 4:5 versions
(Playwright, Chromium):

- the platform row has five platforms, Portrait (4:5) among them;
- a Framing radio group offers "Follow the action" (picked by default)
  and "Blur fill", one tab stop, arrows move without starting a version;
- picking Reels makes a smart version (fit "smart", `<stem>.reels-smart.mp4`),
  says so, and shows a before / after pair with the kept window marked
  on the render;
- switching to Blur fill makes the blur version of the same platform
  (its own file), with a before / after and no window;
- picking YouTube (the render's own shape) hides the framing choice;
- Portrait makes a 1080x1350 smart version.

    python test/smart_reframe_share.py --url http://localhost:8090 [--shots DIR]

Needs the demo set (footage folder "Coastline Bike Ride", a song). It
renders one ~30 s video. Exits non-zero on any failed check.
"""

import argparse
import json
import sys
import time
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


def jobs() -> list[dict]:
    return api("/jobs")["jobs"]


def rendering(page) -> bool:
    return page.evaluate("!!document.querySelector('button.stop')")


def wait_render_ends(page, timeout=600):
    t0 = time.time()
    while time.time() - t0 < timeout:
        if not rendering(page) and page.is_visible("text=Download"):
            page.wait_for_timeout(500)
            return True
        page.wait_for_timeout(250)
    return False


def status(page) -> list[str]:
    return page.evaluate(
        "[...document.querySelectorAll('dialog.sheet .formats [role=status]')].map((e) => e.textContent.trim())"
    )


def wait_ready(page, timeout=240000):
    page.wait_for_function(
        "[...document.querySelectorAll('dialog.sheet .formats [role=status]')].some((e) => /version ready/.test(e.textContent))",
        timeout=timeout,
    )


def wait_compare(page, timeout=30000):
    page.wait_for_function(
        "document.querySelectorAll('dialog.sheet .compare img').length === 2",
        timeout=timeout,
    )


def framing_state(page) -> dict:
    return page.evaluate(
        """() => {
          const g = document.querySelector('dialog.sheet .framing [role=radiogroup]');
          if (!g) return null;
          const r = [...g.querySelectorAll('[role=radio]')];
          return {
            names: r.map((b) => b.textContent.trim()),
            checked: r.filter((b) => b.getAttribute('aria-checked') === 'true').map((b) => b.textContent.trim()),
            stops: r.filter((b) => b.tabIndex === 0).length,
          };
        }"""
    )


def main() -> int:
    global BASE
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default=BASE)
    ap.add_argument("--render-timeout", type=int, default=600)
    ap.add_argument("--shots", default=None, help="directory for screenshots of the sheet")
    args = ap.parse_args()
    BASE = args.url.rstrip("/")

    with sync_playwright() as pw:
        browser = pw.chromium.launch()
        ctx = browser.new_context(viewport={"width": 1400, "height": 1000}, reduced_motion="reduce")
        page = ctx.new_page()
        page.on("pageerror", lambda e: FAILS.append(f"page error: {e}"))
        page.goto(BASE + "/")

        print("Make a video")
        page.get_by_role("radio", name="Coastline Bike Ride").first.click()
        page.wait_for_selector(".list [role=radio]")
        page.get_by_role("button", name="Make my video").click()
        page.wait_for_selector(".studio", timeout=args.render_timeout * 1000)
        check(wait_render_ends(page, args.render_timeout), "the video is made")

        print("Share: platforms and framing")
        page.get_by_role("button", name="Share").click()
        page.wait_for_selector("dialog.sheet[open]")
        sheet = page.locator("dialog.sheet")
        radios = page.locator("dialog.sheet .platforms [role=radio]")
        names = [radios.nth(i).inner_text().replace("\n", " ") for i in range(radios.count())]
        check(radios.count() == 5 and any(n.startswith("Portrait") and "4:5" in n for n in names), f"five platforms, Portrait 4:5 among them ({names})")
        f = framing_state(page)
        check(f is not None and f["names"] == ["Follow the action", "Blur fill"], f"a Framing choice is offered ({f})")
        check(f is not None and f["checked"] == ["Follow the action"] and f["stops"] == 1, "…'Follow the action' by default, one tab stop")
        n_reformats = len([j for j in jobs() if j["kind"] == "reformat"])
        page.locator("dialog.sheet .framing [role=radio]").first.focus()
        page.keyboard.press("ArrowRight")
        page.wait_for_timeout(400)
        f = framing_state(page)
        check(f["checked"] == ["Follow the action"], f"arrows move without switching the framing ({f['checked']})")
        check(len([j for j in jobs() if j["kind"] == "reformat"]) == n_reformats, "…or starting a version")

        print("Share: a Reels version that follows the action")
        sheet.get_by_role("radio", name="Reels").first.click()
        page.wait_for_timeout(200)
        st = status(page)
        check(any("following the action" in t for t in st), f"the progress says it follows the action ({st})")
        wait_ready(page)
        st = status(page)
        check(any("version ready, following the action: 1080 by 1920" in t for t in st), f"…and so does its outcome ({st})")
        smart = [j for j in jobs() if j["kind"] == "reformat" and j.get("preset") == "reels" and j.get("fit") == "smart"]
        check(
            len(smart) >= 1 and smart[0]["state"] == "done" and smart[0]["output_path"].endswith(".reels-smart.mp4"),
            f"it is a smart reformat job writing <stem>.reels-smart.mp4 ({[(j['state'], j['output_path']) for j in smart]})",
        )
        save = page.locator("dialog.sheet .v-actions a.btn.primary")
        check((save.get_attribute("download") or "").endswith(".reels-smart.mp4"), f"the Save link keeps the version's name ({save.get_attribute('download')})")
        wait_compare(page)
        shots = page.evaluate(
            """[...document.querySelectorAll('dialog.sheet .compare .shot')].map((s) => {
                 const r = s.getBoundingClientRect();
                 return { w: Math.round(r.width), h: Math.round(r.height), tag: s.querySelector('.tag')?.textContent };
               })"""
        )
        check(
            len(shots) == 2 and shots[0]["tag"] == "Before" and shots[1]["tag"] == "After"
            and shots[0]["w"] > shots[0]["h"] and shots[1]["w"] < shots[1]["h"],
            f"a before (16:9) / after (9:16) pair ({shots})",
        )
        win = page.evaluate(
            """(() => { const w = document.querySelector('dialog.sheet .compare .window');
                        if (!w) return null;
                        const s = w.parentElement.getBoundingClientRect(), r = w.getBoundingClientRect();
                        return { share: r.width / s.width, inside: r.left >= s.left - 1 && r.right <= s.right + 1 }; })()"""
        )
        check(
            win is not None and abs(win["share"] - (9 / 16) / (16 / 9)) < 0.03 and win["inside"],
            f"the kept window is marked on the render, 9:16 of it, inside the frame ({win})",
        )
        cap = page.locator("dialog.sheet .compare figcaption").inner_text()
        check("keeps" in cap or "middle" in cap, f"the caption says what the window is ({cap!r})")
        if args.shots:
            page.locator("dialog.sheet").screenshot(path=f"{args.shots}/share_smart.png")

        print("Share: switch to Blur fill")
        sheet.get_by_role("radio", name="Blur fill").click()
        page.wait_for_timeout(200)
        wait_ready(page)
        st = status(page)
        check(any("version ready, over a blurred fill: 1080 by 1920" in t for t in st), f"the blur version is made ({st})")
        blur = [j for j in jobs() if j["kind"] == "reformat" and j.get("preset") == "reels" and j.get("fit") == "blur"]
        check(len(blur) >= 1 and blur[0]["output_path"].endswith(".reels.mp4"),
              f"…as its own file, <stem>.reels.mp4 ({[j['output_path'] for j in blur]})")
        wait_compare(page)
        check(page.locator("dialog.sheet .compare .window").count() == 0, "a blur version shows no crop window")
        if args.shots:
            page.locator("dialog.sheet").screenshot(path=f"{args.shots}/share_blur.png")

        print("Share: back to Follow the action, then the 16:9 and 4:5 versions")
        sheet.get_by_role("radio", name="Follow the action").click()
        page.wait_for_timeout(300)
        st = status(page)
        check(any("following the action: 1080 by 1920" in t for t in st), f"the smart version is still there, not made again ({st})")
        sheet.get_by_role("radio", name="YouTube").first.click()
        page.wait_for_timeout(300)
        check(page.locator("dialog.sheet .framing").count() == 0, "YouTube (the render's own shape) has no framing choice")
        wait_ready(page)
        sheet.get_by_role("radio", name="Portrait").first.click()
        page.wait_for_timeout(300)
        check(page.locator("dialog.sheet .framing").count() == 1, "…Portrait has it again")
        wait_ready(page)
        st = status(page)
        check(any("Portrait version ready, following the action: 1080 by 1350" in t for t in st), f"a 4:5 smart version ({st})")
        wait_compare(page)
        if args.shots:
            page.locator("dialog.sheet").screenshot(path=f"{args.shots}/share_portrait.png")
        page.set_viewport_size({"width": 390, "height": 844})
        page.wait_for_timeout(400)
        overflow = page.evaluate("(() => { const d = document.querySelector('dialog.sheet'); return d.scrollWidth - d.clientWidth; })()")
        check(overflow <= 1, f"no sideways scroll in the sheet on a phone ({overflow}px)")
        if args.shots:
            page.locator("dialog.sheet").screenshot(path=f"{args.shots}/share_phone.png")

        browser.close()
    return report()


def report() -> int:
    print(f"\n{PASSES} passed, {len(FAILS)} failed")
    for f in FAILS:
        print(f"  - {f}")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
