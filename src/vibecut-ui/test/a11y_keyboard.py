"""Keyboard / screen-reader pass over the web UI (Playwright, Chromium).

Drives a running vibecut-web with the keyboard only: tabs through the
Create screen, checks every radio group has one tab stop and that the
arrow keys, Home and End move the selection (with wrap-around), that
arrowing across the footage scans only the folder it stops on, makes a
video (focus must land at the top of each new screen, not on the page,
and a click on text must not restart the Tab order there), then in the
Studio checks the moments radio group, the song list's manual
selection, the shortcuts dialog, the Share and Pro settings dialogs
(focus moves in, stays in, Escape closes, focus returns to the opener),
the Pro tablists, the Undo toast, the versions radio group (manual
selection: Tab still leaves it in one step), Compare A/B, and that a
tweak or the moment panel's Drop keeps the keyboard focus through the
re-render. A rough contrast pass over the small text of both screens
runs too.

    python test/a11y_keyboard.py --url http://localhost:8090

Needs footage folders "Alps Ski Day" (3 clips), "Coastline Bike Ride"
(4 clips) and "trip" (6 clips) and at least two songs — the demo set.
It renders one ~30 s video and three re-renders. Exits non-zero on any
failed check.
"""

import argparse
import sys
import time

from playwright.sync_api import sync_playwright

FAILS: list[str] = []
PASSES = 0


def check(cond: bool, msg: str) -> None:
    global PASSES
    if cond:
        PASSES += 1
        print(f"  ok   {msg}")
    else:
        FAILS.append(msg)
        print(f"  FAIL {msg}")


# Delays chosen fetches so tests can force responses to arrive out of order.
DELAY_FETCH = """
(() => {
  const orig = window.fetch;
  window.__delay = {};
  window.__aborted = [];
  window.fetch = async (input, init) => {
    const url = typeof input === 'string' ? input : input.url;
    init?.signal?.addEventListener('abort', () => window.__aborted.push(url));
    for (const [k, ms] of Object.entries(window.__delay))
      if (url.includes(k)) await new Promise((r) => setTimeout(r, ms));
    return orig(input, init);
  };
})();
"""

HELPERS = """
(() => {
  const text = (el) => (el?.textContent ?? '').trim().replace(/\\s+/g, ' ');
  const nameOf = (el) => {
    if (!el) return '';
    const lb = el.getAttribute('aria-labelledby');
    if (lb) return lb.split(/\\s+/).map((id) => text(document.getElementById(id))).join(' ').trim();
    return el.getAttribute('aria-label') ?? text(el).slice(0, 80);
  };
  const groupOf = (el) => el?.closest('[role=radiogroup],[role=tablist]');
  window.__a11y = {
    active() {
      const el = document.activeElement;
      if (!el || el === document.body || el === document.documentElement) return null;
      const g = groupOf(el);
      return {
        tag: el.tagName.toLowerCase(),
        role: el.getAttribute('role'),
        name: nameOf(el),
        checked: el.getAttribute('aria-checked') ?? el.getAttribute('aria-selected'),
        group: g ? nameOf(g) : null,
        dialog: el.closest('dialog[open],[role=dialog]') ? nameOf(el.closest('dialog[open],[role=dialog]')) : null,
      };
    },
    group(name) {
      const g = [...document.querySelectorAll('[role=radiogroup],[role=tablist]')].find((x) => nameOf(x) === name);
      if (!g) return null;
      const role = g.getAttribute('role');
      const sel = role === 'tablist' ? '[role=tab]' : '[role=radio]';
      return [...g.querySelectorAll(sel)]
        .filter((el) => el.parentElement.closest('[role=' + role + ']') === g)
        .map((el) => ({
          name: nameOf(el),
          checked: (el.getAttribute('aria-checked') ?? el.getAttribute('aria-selected')) === 'true',
          tabbable: el.tabIndex >= 0,
        }));
    },
    // Rough WCAG 1.4.3 pass over visible text: the text colour (times
    // the opacity of its ancestors) over the solid background colours
    // behind it, on the page ink. Text over gradients or images can't be
    // measured this way and is skipped; disabled controls are exempt.
    contrast() {
      const parse = (c) => {
        const m = /rgba?\\(([^)]+)\\)/.exec(c || '');
        if (!m) return null;
        const p = m[1].split(/[\\s,\\/]+/).filter(Boolean).map(Number);
        return [p[0], p[1], p[2], p.length > 3 ? p[3] : 1];
      };
      const lin = (v) => ((v /= 255) <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
      const lum = (c) => 0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2]);
      const over = (t, b) => [0, 1, 2].map((i) => t[i] * t[3] + b[i] * (1 - t[3])).concat(1);
      const out = [];
      const seen = new Set();
      const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
      while (walk.nextNode()) {
        const el = walk.currentNode.parentElement;
        if (!el || seen.has(el) || !walk.currentNode.textContent.trim()) continue;
        seen.add(el);
        if (el.closest('[aria-hidden=true], .sr-only, :disabled, [aria-disabled=true], option, canvas, svg')) continue;
        const r = el.getBoundingClientRect();
        const cs = getComputedStyle(el);
        if (r.width < 1 || r.height < 1 || cs.visibility === 'hidden') continue;
        let opacity = 1, unknown = false;
        const layers = [];
        for (let e = el; e; e = e.parentElement) {
          const s = getComputedStyle(e);
          opacity *= Number(s.opacity);
          if (s.backgroundImage !== 'none') { unknown = true; break; }
          const bg = parse(s.backgroundColor);
          if (bg && bg[3] > 0) { layers.push(bg); if (bg[3] >= 1) break; }
        }
        const fg0 = parse(cs.color);
        if (unknown || !fg0 || fg0[3] === 0 || cs.webkitTextFillColor === 'rgba(0, 0, 0, 0)') continue;
        let bg = [7, 8, 12, 1];
        for (let i = layers.length - 1; i >= 0; i--) bg = over(layers[i], bg);
        const fg = over([fg0[0], fg0[1], fg0[2], fg0[3] * opacity], bg);
        const [hi, lo] = [lum(fg), lum(bg)].sort((a, b) => b - a);
        const ratio = (hi + 0.05) / (lo + 0.05);
        const px = parseFloat(cs.fontSize);
        const large = px >= 24 || (px >= 18.66 && Number(cs.fontWeight) >= 700);
        const need = large ? 3 : 4.5;
        if (ratio < need)
          out.push(`${ratio.toFixed(2)}:1 < ${need} (${px}px) '${walk.currentNode.textContent.trim().slice(0, 40)}' <${el.tagName.toLowerCase()} class="${el.className}">`);
      }
      return out;
    },
  };
})();
"""


def active(page):
    return page.evaluate("window.__a11y.active()")


def group(page, name):
    return page.evaluate("(n) => window.__a11y.group(n)", name)


def checked_name(page, name):
    items = group(page, name) or []
    on = [i["name"] for i in items if i["checked"]]
    return on[0] if len(on) == 1 else (None if not on else on)


def press(page, key, settle=120):
    page.keyboard.press(key)
    page.wait_for_timeout(settle)
    return active(page)


def focus_top(page):
    page.evaluate("document.querySelector('a.skip')?.focus()")


def tab_to(page, pred, what, limit=150, back=False):
    a = active(page)
    if a and pred(a):
        return a
    for _ in range(limit):
        a = press(page, "Shift+Tab" if back else "Tab", settle=30)
        if a and pred(a):
            return a
    raise AssertionError(f"could not Tab to {what}")


def one_tab_stop(page, name, label):
    items = group(page, name)
    if not items:
        check(False, f"{label}: group '{name}' exists")
        return
    stops = [i for i in items if i["tabbable"]]
    on = [i for i in items if i["checked"]]
    want = on[0] if on else items[0]
    check(
        len(stops) == 1 and stops[0]["name"] == want["name"],
        f"{label}: exactly one tab stop ({len(stops)} of {len(items)}), on the "
        + ("checked item" if on else "first item"),
    )


def contrast(page, label):
    bad = page.evaluate("window.__a11y.contrast()")
    check(not bad, f"{label}: small text has at least 4.5:1 contrast ({len(bad)} below)")
    for b in bad[:12]:
        print(f"         {b}")


def focus_tag(page):
    return page.evaluate("document.activeElement?.tagName.toLowerCase() ?? null")


def main_tabindex(page):
    return page.evaluate("document.getElementById('main').getAttribute('tabindex')")


def arrows_cycle(page, name, label, vertical=True):
    """From the focused item of `name`: Right/Down, Left/Up, End, wrap, Home."""
    items = group(page, name)
    n = len(items)
    names = [i["name"] for i in items]
    a = active(page)
    i0 = names.index(a["name"])
    check(a["group"] == name, f"{label}: focus is in the group")

    def at(idx, key, how):
        a = press(page, key)
        exp = names[idx % n]
        check(
            a is not None and a["name"] == exp and checked_name(page, name) == exp,
            f"{label}: {how} → '{exp}' focused and checked (got {a and a['name']!r}, checked {checked_name(page, name)!r})",
        )

    at(i0 + 1, "ArrowRight", "ArrowRight moves on")
    at(i0, "ArrowLeft", "ArrowLeft moves back")
    if vertical and n > 2:
        at(i0 + 1, "ArrowDown", "ArrowDown moves on")
        at(i0, "ArrowUp", "ArrowUp moves back")
    at(n - 1, "End", "End")
    at(0, "ArrowRight", "ArrowRight wraps from the last")
    at(n - 1, "ArrowLeft", "ArrowLeft wraps from the first")
    at(0, "Home", "Home")
    one_tab_stop(page, name, label)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://localhost:8090")
    ap.add_argument("--render-timeout", type=int, default=600)
    ap.add_argument("--create-only", action="store_true", help="stop before making a video")
    ap.add_argument("--full-motion", action="store_true", help="don't ask for reduced motion")
    args = ap.parse_args()

    with sync_playwright() as pw:
        browser = pw.chromium.launch()
        # Reduced motion by default: also checks the app honours it (no
        # reveal autoplay).
        motion = "no-preference" if args.full_motion else "reduce"
        ctx = browser.new_context(viewport={"width": 1400, "height": 1000}, reduced_motion=motion)
        ctx.add_init_script(DELAY_FETCH)
        ctx.add_init_script(HELPERS)
        # Full-quality renders: Fast previews (on by default) makes drafts,
        # and Share on a draft finishes it first instead of opening the
        # sheet (draft_journey.py covers that).
        ctx.add_init_script("try { localStorage.setItem('vibecut.fastPreviews', 'off') } catch {}")
        page = ctx.new_page()
        page.on("pageerror", lambda e: FAILS.append(f"page error: {e}"))
        page.goto(args.url)
        page.wait_for_selector("[role=radiogroup] [role=radio]")
        page.wait_for_function("document.querySelector('.sources .hint strong')?.textContent.includes('4 clips')")
        page.wait_for_selector(".list [role=radio]")

        print("Create: one tab stop per group")
        groups = ["Pick your footage", "Choose a vibe", "Length", "Filter by mood", "Song"]
        for g in groups:
            one_tab_stop(page, g, g)
        focus_top(page)
        seen: dict[str, set[str]] = {}
        for _ in range(200):
            a = press(page, "Tab", settle=20)
            if a and a["role"] == "radio":
                seen.setdefault(a["group"], set()).add(a["name"])
            if a and a["name"].startswith("Make my video"):
                break
        else:
            check(False, "Tab reaches 'Make my video'")
        for g in groups:
            check(len(seen.get(g, ())) == 1, f"Tab passes '{g}' once (radios focused: {sorted(seen.get(g, ()))})")
        contrast(page, "Create")

        print("Create: skip link, and clicks on text keep the Tab order")
        check(main_tabindex(page) is None, "main: no lasting tabindex")
        focus_top(page)
        press(page, "Enter")
        check(focus_tag(page) == "main", f"skip link: focus moves to main ({focus_tag(page)!r})")
        a = press(page, "Tab")
        check(a is not None and a["name"] == "Browse your drives…", f"skip link: the next Tab reaches main's first control ({a and a['name']!r})")
        check(main_tabindex(page) is None, "skip link: main is focusable only while it has the focus")
        page.click("#vibe-h")
        check(focus_tag(page) != "main", f"click on the 'Choose a vibe' heading doesn't focus main ({focus_tag(page)!r})")
        a = press(page, "Tab")
        check(
            a is not None and a["group"] == "Choose a vibe",
            f"click on the 'Choose a vibe' heading, then Tab: the vibe group, not main's first control ({a and a['name']!r})",
        )
        check(
            page.get_attribute("#cmd-input", "aria-keyshortcuts") is None and page.locator(".cmd kbd.hint").count() == 0,
            "command bar: Create doesn't advertise Ctrl+K (only the Studio handles it)",
        )

        print("Create: footage radios (deferred and stale scans)")
        scans: list[str] = []
        page.on("request", lambda r: scans.append(r.url.split("folder=", 1)[1]) if "/api/sources?folder=" in r.url else None)
        clips = "document.querySelector('.sources .hint strong')?.textContent.includes('%s')"
        focus_top(page)
        tab_to(page, lambda a: a["group"] == "Pick your footage", "the footage group")
        check(active(page)["name"].startswith("Coastline"), "footage: Tab lands on the checked folder")
        # Arrowing across the folders scans only the one it stops on.
        scans.clear()
        for _ in range(4):  # trip, Alps, Coastline, trip
            page.keyboard.press("ArrowRight")
            page.wait_for_timeout(60)
        page.wait_for_function(clips % "6 clips")
        page.wait_for_timeout(500)
        check(str(checked_name(page, "Pick your footage")).startswith("trip"), "footage: four quick ArrowRights land on 'trip'")
        check(scans == ["trip"], f"footage: ...and scan only that folder ({scans})")
        # Enter (or a click) on a folder the arrows only passed scans it at once.
        scans.clear()
        page.keyboard.press("ArrowLeft")  # Coastline: the scan waits
        page.keyboard.press("Enter")
        page.wait_for_timeout(150)
        check(len(scans) == 1 and scans[0].startswith("Coastline"), f"footage: Enter scans the folder straight away ({scans})")
        page.wait_for_function(clips % "4 clips")
        page.wait_for_timeout(500)
        check(len(scans) == 1, f"footage: ...once ({scans})")
        # A slow scan of Alps already under way must not win over a later pick.
        page.evaluate("window.__delay = {'folder=Alps': 2500}")
        press(page, "ArrowLeft")  # Alps (slow)
        page.wait_for_timeout(500)  # its scan has started
        press(page, "ArrowRight")  # back to Coastline (already scanned)
        page.wait_for_timeout(3000)
        a = active(page)
        check(
            a["name"].startswith("Coastline") and str(checked_name(page, "Pick your footage")).startswith("Coastline"),
            "footage: A → B → A keeps A checked",
        )
        check(
            "4 clips" in page.inner_text(".sources .hint"),
            "footage: the clips shown are A's after B's slow scan returns",
        )
        aborted = page.evaluate("window.__aborted")
        check(any("folder=Alps" in u for u in aborted), f"footage: B's superseded scan is aborted ({aborted})")
        # Two folders not scanned yet, the first one slow: the later wins.
        page.evaluate("window.__delay = {'folder=trip': 2500}")
        press(page, "ArrowRight")  # trip (slow)
        page.wait_for_timeout(500)
        press(page, "ArrowRight")  # wraps to Alps (fast)
        page.wait_for_function(clips % "3 clips")
        page.wait_for_timeout(3000)
        check(str(checked_name(page, "Pick your footage")).startswith("Alps"), "footage: ArrowRight wraps to the first folder")
        check(
            "3 clips" in page.inner_text(".sources .hint"),
            "footage: a slow earlier scan doesn't replace the later folder's clips",
        )
        page.evaluate("window.__delay = {}")
        press(page, "ArrowRight")  # Coastline for the render
        page.wait_for_function(clips % "4 clips")
        one_tab_stop(page, "Pick your footage", "footage")

        print("Create: vibe radios")
        tab_to(page, lambda a: a["group"] == "Choose a vibe", "the vibe group")
        a = active(page)
        check(a["checked"] == "true", f"vibe: Tab lands on the checked vibe ({a['name']})")
        arrows_cycle(page, "Choose a vibe", "vibe")
        a = press(page, " ")
        check(a["checked"] == "true", "vibe: Space keeps the focused vibe checked")
        a = press(page, "Tab")
        check(a["group"] == "Length", f"vibe: Tab leaves the group in one step (now on {a['name']!r})")

        print("Create: length radios")
        arrows_cycle(page, "Length", "length")
        press(page, "ArrowRight")  # Reel, 30 s: a quick render
        check(str(checked_name(page, "Length")).startswith("Reel"), "length: Reel checked")

        print("Create: mood and song radios")
        tab_to(page, lambda a: a["group"] == "Filter by mood", "the mood group")
        arrows_cycle(page, "Filter by mood", "mood")
        press(page, "ArrowRight")  # Chill
        page.wait_for_timeout(300)
        n_chill = len(group(page, "Song"))
        press(page, "Home")
        page.wait_for_timeout(300)
        check(n_chill < len(group(page, "Song")), f"mood: the filter changes the song list ({n_chill} chill songs)")
        tab_to(page, lambda a: a["group"] == "Song" and a["role"] == "radio", "the song group")
        one_tab_stop(page, "Song", "song")
        a = active(page)
        check(
            a["name"].startswith("Let Vibecut pick") and a["checked"] == "true",
            f"song: 'Let Vibecut pick' is the group's checked radio by default ({a['name'][:30]!r})",
        )
        arrows_cycle(page, "Song", "song")
        on = [i["name"] for i in group(page, "Song") if i["checked"]]
        check(len(on) == 1 and on[0].startswith("Let Vibecut pick"), f"song: Home goes back to 'Let Vibecut pick', the one checked choice ({on})")
        press(page, "ArrowDown")  # the first song, for the render
        on = [i["name"] for i in group(page, "Song") if i["checked"]]
        check(len(on) == 1 and not on[0].startswith("Let Vibecut pick"), f"song: picking a song unchecks 'Let Vibecut pick' ({on})")

        if args.create_only:
            browser.close()
            return report()

        print("Create → Studio")
        tab_to(page, lambda a: a["name"].startswith("Make my video"), "'Make my video'")
        press(page, "Enter")
        page.wait_for_selector(".room", timeout=30000)
        page.wait_for_timeout(300)
        tag = focus_tag(page)
        check(tag == "main", f"cutting room: focus moves to the top of the new screen, not the page ({tag!r})")
        page.wait_for_selector(".studio", timeout=args.render_timeout * 1000)
        page.wait_for_selector("[role=radiogroup][aria-label='Moments in your video'] [role=radio]", timeout=60000)
        page.wait_for_timeout(1500)
        tag = focus_tag(page)
        check(tag == "main", f"studio: focus is at the top of the new screen ({tag!r})")
        a = press(page, "Tab")
        check(
            a is not None and a["name"].startswith("Shortcuts"),
            f"studio: Tab from there reaches the screen's first control ({a and a['name']!r})",
        )
        check(main_tabindex(page) is None, "studio: main is focusable only while it has the focus")
        check(
            page.get_attribute("#cmd-input", "aria-keyshortcuts") == "Control+K Meta+K",
            "command bar: the Studio advertises Ctrl / Cmd+K",
        )
        a = press(page, "Control+k")
        check(a is not None and a["tag"] == "input", f"command bar: Ctrl+K focuses it in the Studio ({a and a['tag']!r})")
        contrast(page, "Studio")
        paused = page.evaluate("document.querySelector('.player-wrap video')?.paused")
        if args.full_motion:
            check(paused is False, "full motion: the finished reel plays by itself (the reveal)")
        else:
            check(paused is True, "reduced motion: the finished reel doesn't start playing by itself")

        print("Studio: moments radio group and inspector")
        moments = "Moments in your video"
        one_tab_stop(page, moments, "moments")
        focus_top(page)
        tab_to(page, lambda a: a["group"] == moments, "the moments group")
        a = press(page, "ArrowRight")
        check(
            checked_name(page, moments) == a["name"] and page.is_visible("aside[aria-label='Selected moment']"),
            "moments: ArrowRight selects the next moment and opens the inspector",
        )
        arrows_cycle(page, moments, "moments")
        a = press(page, "Tab")
        check(a["group"] != moments, f"moments: Tab leaves the group in one step (now on {a['name']!r})")
        sel = checked_name(page, moments)
        tab_to(page, lambda a: a["name"] == "Close moment details", "the inspector's Close")
        a = press(page, "Enter")
        check(
            a is not None and a["group"] == moments and a["name"] == sel,
            f"inspector: closing hands focus back to the moment ({a and a['name']!r})",
        )

        print("Studio: shortcuts dialog")
        a = press(page, "?")
        check(a is not None and a["dialog"] == "Keyboard shortcuts", "help: '?' opens it and moves focus inside")
        a = press(page, "Escape")
        check(not page.is_visible("[role=dialog][aria-label='Keyboard shortcuts']"), "help: Escape closes it")
        check(a is not None and a["name"] == sel, f"help: focus returns to where '?' was pressed ({a and a['name']!r})")
        btn = tab_to(page, lambda a: a["name"].startswith("Shortcuts"), "the Shortcuts button")
        a = press(page, "Enter")
        check(a is not None and a["dialog"] == "Keyboard shortcuts", "help: the button opens it with focus inside")
        check(
            page.get_by_role("button", name="Shortcuts").get_attribute("aria-expanded") == "true",
            "help: the button reports aria-expanded",
        )
        a = press(page, "Tab")
        check(a is not None and a["tag"] == "input", "help: Tab reaches the shortcuts checkbox")
        a = press(page, "Escape")
        check(not page.is_visible("[role=dialog][aria-label='Keyboard shortcuts']"), "help: Escape closes it from the checkbox")
        check(a is not None and a["name"].startswith("Shortcuts"), f"help: focus returns to the button ({a and a['name']!r})")

        def modal(opener_name, dialog_sel, label):
            tab_to(page, lambda a: a["name"].strip().startswith(opener_name) and a["dialog"] is None, f"'{opener_name}'")
            a = press(page, "Enter", settle=400)
            check(page.is_visible(dialog_sel), f"{label}: Enter opens it")
            check(a is not None and a["dialog"] is not None, f"{label}: focus moves inside ({a and a['name']!r})")
            n_out = 0
            for _ in range(25):
                a = press(page, "Tab", settle=20)
                if a is not None and a["dialog"] is None:
                    n_out += 1
            check(n_out == 0, f"{label}: Tab never reaches the page behind ({n_out} escapes)")
            return a

        def close_modal(opener_name, dialog_sel, label):
            a = press(page, "Escape", settle=400)
            check(not page.is_visible(dialog_sel), f"{label}: Escape closes it")
            check(
                a is not None and a["name"].strip().startswith(opener_name),
                f"{label}: focus returns to '{opener_name}' ({a and a['name']!r})",
            )

        print("Studio: share sheet")
        modal("Share", "dialog.sheet[open]", "share")
        close_modal("Share", "dialog.sheet[open]", "share")

        print("Studio: Pro settings drawer and its tablists")
        focus_top(page)
        modal("Pro settings", "dialog.drawer[open]", "pro")
        view = "Settings view"
        page.wait_for_selector("[role=tablist][aria-label='Settings steps']")
        tab_to(page, lambda a: a["group"] == view, "the Guided / Raw tabs")
        one_tab_stop(page, view, "pro view tabs")
        a = press(page, "ArrowRight")
        check(
            checked_name(page, view) == "Raw TOML" and page.is_visible("#raw-toml"),
            "pro view tabs: ArrowRight selects Raw TOML and shows its panel",
        )
        check(
            page.get_attribute("[role=tabpanel]#pro-panel", "aria-labelledby") == "pro-tab-raw",
            "pro view tabs: the panel is labelled by the selected tab",
        )
        press(page, "ArrowRight")
        check(checked_name(page, view) == "Guided", "pro view tabs: ArrowRight wraps to Guided")
        a = press(page, "ArrowDown")
        check(checked_name(page, view) == "Guided", "pro view tabs: up / down arrows are left alone in a horizontal tablist")
        steps = "Settings steps"
        tab_to(page, lambda a: a["group"] == steps, "the wizard steps")
        arrows_cycle(page, steps, "wizard steps", vertical=False)
        close_modal("Pro settings", "dialog.drawer[open]", "pro")

        print("Studio: song list (manual selection) and the global shortcuts")
        tab_to(page, lambda a: a["name"].startswith("Song"), "the Song disclosure")
        press(page, "Enter", settle=600)
        page.wait_for_selector(".rail [role=radiogroup][aria-label='Song'] [role=radio]")
        tab_to(page, lambda a: a["group"] == "Song" and a["role"] == "radio", "the studio song group")
        one_tab_stop(page, "Song", "studio song")
        before = checked_name(page, "Song")
        moment_before = checked_name(page, moments)
        a0 = active(page)
        a = press(page, "ArrowDown")
        check(a["group"] == "Song" and a["name"] != a0["name"], "studio song: ArrowDown moves focus")
        check(checked_name(page, "Song") == before, "studio song: …but doesn't re-render (selection unchanged)")
        a = press(page, "ArrowLeft")
        check(a["group"] == "Song", "studio song: ArrowLeft stays in the group")
        check(
            checked_name(page, moments) == moment_before,
            "global shortcuts: arrows in a radio group don't move the selected moment",
        )
        a = press(page, "ArrowDown")
        target = a["name"]
        press(page, " ")
        page.wait_for_selector(".overlay", timeout=30000)
        page.wait_for_selector(".overlay", state="detached", timeout=args.render_timeout * 1000)
        page.wait_for_timeout(1000)
        check(checked_name(page, "Song") == target, f"studio song: Space picks the focused song ({target[:30]!r})")

        print("Studio: undo toast")
        page.wait_for_selector(".toast", timeout=30000)
        status = page.inner_text("p.sr-only[role=status]:has-text('You can undo this')")
        check("Song:" in status, f"undo: one status line announces the change ({status!r})")
        check(
            page.evaluate("document.querySelectorAll('.toasts [aria-live], .toasts [role=status]').length") == 0,
            "undo: the toast isn't a second live region",
        )
        tab_to(page, lambda a: a["name"] == "Dismiss", "the toast's Dismiss")
        page.wait_for_timeout(13000)
        check(page.is_visible(".toast"), "undo: the toast doesn't time out while it has focus")
        press(page, "Shift+Tab")  # Undo
        press(page, "Shift+Tab")  # out of the toast
        page.wait_for_timeout(300)
        opener = active(page)
        tab_to(page, lambda a: a["name"] == "Dismiss", "the toast's Dismiss")
        a = press(page, "Enter", settle=300)
        check(not page.is_visible(".toast"), "undo: Dismiss closes the toast")
        check(
            a is not None and opener is not None and a["name"] == opener["name"],
            f"undo: focus goes back to where it came from ({a and a['name']!r})",
        )

        print("Studio: versions radio group (opening one reloads the player)")
        vers = "Versions"
        player = ".player-wrap video"
        items = group(page, vers) or []
        n = len(items)
        check(n >= 2, f"versions: the song change kept a second version ({n} listed)")
        one_tab_stop(page, vers, "versions")
        latest = checked_name(page, vers)
        check(str(latest).startswith(f"Version {n}"), f"versions: the latest is checked ({latest!r})")
        src0 = page.get_attribute(player, "src")
        focus_top(page)
        tab_to(page, lambda a: a["group"] == vers, "the versions group")
        a = press(page, "ArrowUp")
        check(a["group"] == vers and a["name"].startswith(f"Version {n - 1}"), "versions: ArrowUp moves focus to the previous one")
        check(
            checked_name(page, vers) == latest and page.get_attribute(player, "src") == src0,
            "versions: …without opening it",
        )
        a = press(page, "Tab")
        check(a is None or a["group"] != vers, f"versions: after ArrowUp, Tab leaves the group in one step ({a and a['name']!r})")
        one_tab_stop(page, vers, "versions, focus gone")
        a = press(page, "Shift+Tab")
        check(
            a is not None and a["group"] == vers and a["name"] == latest,
            f"versions: Shift+Tab comes back to the checked version ({a and a['name']!r})",
        )
        a = press(page, "ArrowUp")
        a = press(page, " ", settle=1500)
        now = checked_name(page, vers)
        check(
            str(now).startswith(f"Version {n - 1}") and page.get_attribute(player, "src") != src0,
            f"versions: Space opens the focused version ({now!r})",
        )
        check(a is not None and a["group"] == vers, "versions: focus stays on the version")
        one_tab_stop(page, vers, "versions")
        press(page, "End")
        press(page, "Enter", settle=1500)
        check(
            checked_name(page, vers) == latest and page.get_attribute(player, "src") == src0,
            "versions: End, Enter goes back to the latest",
        )

        print("Studio: compare A/B")
        tab_to(page, lambda a: a["name"].startswith("Compare A/B"), "'Compare A/B'")
        btn = page.locator(".versions .head button")
        check(
            btn.get_attribute("aria-expanded") == "false" and btn.get_attribute("aria-controls") is None,
            "compare: collapsed, and aria-controls doesn't point at a missing panel",
        )
        press(page, "Enter", settle=500)
        ctl = btn.get_attribute("aria-controls")
        check(
            btn.get_attribute("aria-expanded") == "true" and bool(ctl) and page.is_visible(f"#{ctl}"),
            f"compare: Enter expands it and aria-controls names the shown panel ({ctl!r})",
        )
        a = press(page, "Enter", settle=300)
        check(
            a is not None and a["name"].startswith("Compare A/B") and not page.is_visible("#compare-ab"),
            "compare: Enter again collapses it, focus stays on the button",
        )

        print("Studio: a click on text keeps the Tab order")
        page.click(".versions .eyebrow")
        check(focus_tag(page) != "main", f"click on the 'Versions' label doesn't focus main ({focus_tag(page)!r})")
        a = press(page, "Tab")
        check(
            a is not None and a["name"].startswith("Compare A/B"),
            f"click on 'Versions', then Tab: the next control there, not the top of the Studio ({a and a['name']!r})",
        )

        def rerender_done():
            page.wait_for_selector(".overlay", timeout=30000)
            page.wait_for_selector(".overlay", state="detached", timeout=args.render_timeout * 1000)
            page.wait_for_selector("[role=radiogroup][aria-label='Moments in your video'] [role=radio]", timeout=60000)
            page.wait_for_timeout(1500)

        print("Studio: a tweak keeps the keyboard focus through its re-render")
        focus_top(page)
        tab_to(page, lambda a: a["tag"] == "button" and a["name"].endswith("Calmer"), "'Calmer'")
        # Hold the change's request, so it is still being written for a while.
        held = []
        page.route("**/api/feedback", lambda route: held.append(route))
        a = press(page, "Enter", settle=300)
        check(a is not None and a["name"].endswith("Calmer"), f"tweak: focus stays on 'Calmer' while it works ({a and a['name']!r})")
        t0 = time.time()
        while not held and time.time() - t0 < 10:
            page.wait_for_timeout(50)
        busy = page.evaluate("[...document.querySelectorAll('.tweaks .mix, .tweaks button.tweak:not([disabled])')].map((b) => b.getAttribute('aria-disabled'))")
        check(
            bool(held) and len(busy) > 0 and all(b == "true" for b in busy),
            f"tweak: while its change is being written, every tweak reports itself unavailable ({busy})",
        )
        press(page, "Enter", settle=400)
        check(len(held) == 1, f"tweak: ...and pressing it again doesn't send the change twice ({len(held)} requests)")
        for r in held:
            r.continue_()
        page.unroute("**/api/feedback")
        # Mid-render the tweaks stay live: a press offers "Stop and apply
        # this instead" (see test/new_controls_keyboard.py), so the button
        # doesn't report itself unavailable once the re-render runs.
        page.wait_for_selector(".overlay", timeout=30000)
        page.wait_for_timeout(500)
        busy = page.evaluate("document.activeElement.getAttribute('aria-disabled')")
        check(busy != "true", f"tweak: ...which stays available during the re-render (aria-disabled {busy!r})")
        rerender_done()
        a = active(page)
        check(a is not None and a["name"].endswith("Calmer"), f"tweak: ...and after the re-render ({a and a['name']!r})")

        print("Studio: the moment panel's Drop keeps the keyboard on the page")
        focus_top(page)
        tab_to(page, lambda a: a["group"] == moments, "the moments group")
        press(page, "ArrowRight")
        items = group(page, moments)
        idx = [i["checked"] for i in items].index(True)
        tab_to(page, lambda a: a["name"] == "Drop this moment", "'Drop this moment'")
        press(page, "Enter", settle=200)
        page.wait_for_selector(".overlay", timeout=30000)
        page.wait_for_timeout(300)
        tag = focus_tag(page)
        check(tag == "main", f"drop: while it re-renders, focus waits at the top of the screen, not on the page ({tag!r})")
        rerender_done()
        a = active(page)
        items = group(page, moments)
        want = items[min(idx, len(items) - 1)]["name"] if items else None
        check(
            a is not None and a["group"] == moments and a["name"] == want,
            f"drop: after the re-render, focus is back on the moment in the same place ({a and a['name']!r}, want {want!r})",
        )

        browser.close()
    return report()


def report() -> int:
    print(f"\n{PASSES} passed, {len(FAILS)} failed")
    for f in FAILS:
        print(f"  - {f}")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
