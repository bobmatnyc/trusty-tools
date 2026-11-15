"""Exercise the injected page functions of pages.js on a static page in Chromium (#11239, #11294)."""

import asyncio
import base64
import json
from pathlib import Path

import pytest
from file_io_fixture import DOWNLOAD_TEXT, file_site
from playwright.async_api import async_playwright
from playwright.sync_api import sync_playwright

from trusty_browser import service

SCRIPT = Path(service.__file__).with_name("extension") / "pages.js"


def load(tab):
    """Add pages_read.js, then pages.js, as the worker's importScripts order does (#11355)."""
    tab.add_script_tag(path=str(SCRIPT.with_name("pages_read.js")))
    tab.add_script_tag(path=str(SCRIPT))

PAGE = """
<title>Pages QA</title>
<main>
  <p>Visible text</p>
  <a href="/next">Next page</a>
  <button id="go" aria-label="Go now">Go</button>
  <label>Email <input id="email" type="email" value="a@b.test"></label>
  <input id="pw" type="password" value="hunter2" placeholder="Password">
  <input id="ro" readonly value="fixed" placeholder="Fixed">
  <input type="hidden" value="secret-hidden">
  <button style="visibility:hidden">Ghost</button>
  <div id="note" contenteditable="true">old note</div>
  <div contenteditable="false">not editable</div>
  <textarea id="bio" aria-label="Bio">old bio</textarea>
</main>
"""


@pytest.fixture()
def page():
    with sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            tab = browser.new_page()
            tab.set_content(PAGE)
            load(tab)
            tab.evaluate("""() => {
              window.events = [];
              for (const kind of ["pointerdown","mousedown","pointerup","mouseup","click","input","change"])
                document.addEventListener(kind, e => window.events.push(kind + ":" + (e.target.id || e.target.localName)), true);
            }""")
            yield tab
        finally:
            browser.close()


# #11294: the trusted harness logs whether each event the page saw was trusted.
TRUSTED_LOG = """() => {
  window.events = [];
  for (const kind of ["pointerdown","mousedown","pointerup","mouseup","click"])
    document.addEventListener(kind, e => window.events.push(
      kind + ":" + (e.target.id || e.target.localName) + (e.isTrusted ? "" : ":untrusted")), true);
}"""


class Tab:
    """An async page whose CDP session plays the worker's chrome.debugger (#11294).

    The async API is needed: a binding callback that sends CDP input deadlocks
    the sync API while the page's evaluate is pending.
    """

    def __init__(self, page, cdp):
        self.page, self.cdp, self.dialogs, self.policy = page, cdp, [], "dismiss"

    def answer(self, params):
        # The page is blocked until answered, so this mirrors trusted.accepts in Python.
        accept = params["type"] == "alert" or self.policy == "accept"
        self.dialogs.append((params["type"], params["message"], accept))
        asyncio.ensure_future(self.cdp.send("Page.handleJavaScriptDialog", {"accept": accept}))

    async def read(self):
        return await self.page.evaluate("() => TrustyPages.pageRead(512, {include_text: true})")

    async def click(self, ref):
        """pageTarget then the trusted CDP click, as the worker's ``act`` runs them."""
        return await self.page.evaluate("""async ref => {
          const point = TrustyPages.pageTarget(ref);
          if (!point.ok) return point;
          await TrustyPages.trusted.click((method, params = {}) => window.__cdp(method, params), point);
          return {ok:true};
        }""", ref)


def trusted(body, content=None):
    """Run ``body(tab)`` against ``content`` (default PAGE) in a fresh Chromium, bounded to 60 s."""
    async def main():
        async with async_playwright() as driver:
            browser = await driver.chromium.launch()
            try:
                page = await browser.new_page()
                await page.set_content(content or PAGE)
                await page.add_script_tag(path=str(SCRIPT.with_name("pages_read.js")))
                await page.add_script_tag(path=str(SCRIPT))
                cdp = await page.context.new_cdp_session(page)
                await cdp.send("Page.enable")
                tab = Tab(page, cdp)
                cdp.on("Page.javascriptDialogOpening", tab.answer)
                page.on("dialog", lambda _dialog: None)  # leave every dialog to the CDP session

                async def send(method, params):
                    return await cdp.send(method, params)

                await page.expose_function("__cdp", send)
                await page.evaluate(TRUSTED_LOG)
                await body(tab)
            finally:
                await browser.close()

    asyncio.run(asyncio.wait_for(main(), 60))


def click(page, ref):
    """pageTarget then a click at its point; the trusted CDP path is covered by `trusted`."""
    return page.evaluate("""ref => {
      const point = TrustyPages.pageTarget(ref);
      if (!point.ok) return point;
      document.elementFromPoint(point.x, point.y).click();
      return {ok:true};
    }""", ref)


def read(page, **options):
    return page.evaluate("o => TrustyPages.pageRead(512, o)", {"include_text": True, **options})


def ref_of(result, name):
    return next(e["ref"] for e in result["elements"] if e["name"] == name)


def test_read_reports_elements_and_bounds_size(page):
    result = read(page)
    names = [e["name"] for e in result["elements"]]
    assert names == ["Next page", "Go now", "Email", "Password", "Fixed", "old note", "Bio"]
    epoch = result["elements"][0]["ref"].split(".")[0]
    assert all(e["ref"].split(".")[0] == epoch for e in result["elements"])
    by_name = {e["name"]: e for e in result["elements"]}
    assert by_name["Next page"]["role"] == "link"
    assert by_name["Next page"]["href"].endswith("/next")
    assert by_name["Email"] == {"ref": epoch + ".2", "tag": "input", "role": "textbox", "name": "Email",
                                "type": "email", "value": "a@b.test"}
    assert "value" not in by_name["Password"]
    assert "hunter2" not in json.dumps(result) and "secret-hidden" not in json.dumps(result)
    assert result["title"] == "Pages QA" and "Visible text" in result["text"]
    assert result["truncated"] is False
    page.evaluate("""() => {
      document.body.append("x".repeat(400000));
      for (let i = 0; i < 700; i++) {
        const b = document.createElement("button"); b.textContent = "btn " + "y".repeat(150) + i; document.body.append(b);
      }
    }""")
    big = read(page)
    assert big["truncated"] is True
    assert len(big["elements"]) <= 500
    assert len(json.dumps(big, separators=(",", ":"), ensure_ascii=False).encode()) <= 262144 - 512
    # #11355: an unchanged element keeps its ref across reads.
    assert big["elements"][0]["ref"] == result["elements"][0]["ref"]


def test_read_scopes_bounds_and_truncates_in_viewport_order(page):
    # #11356: scope, viewport and size controls; truncation keeps the elements nearest the viewport.
    page.set_viewport_size({"width": 800, "height": 600})
    page.evaluate("""() => {
      const list = Object.assign(document.createElement("ul"), {id:"rows"});
      for (let i = 0; i < 60; i++) {
        const li = document.createElement("li");
        li.innerHTML = "<span>Row " + i + "</span> <button>Edit " + i + "</button>";
        list.append(li);
      }
      document.body.append(list);
    }""")
    whole = read(page, include_text=False)
    assert whole["text"] == "" and whole["truncated"] is False
    rows = whole["elements"]
    edit = lambda n: next(e["ref"] for e in rows if e["name"] == "Edit " + str(n))
    page.evaluate("() => scrollTo(0, document.querySelectorAll('#rows button')[30].offsetTop - 280)")
    near = read(page, include_text=False, max_chars=1500)
    names = [e["name"] for e in near["elements"]]
    assert near["truncated"] is True and "Edit 30" in names and "Next page" not in names
    assert len(json.dumps(near, separators=(",", ":"), ensure_ascii=False).encode()) <= 1500
    # The kept elements are the nearest ones, returned in document order.
    numbers = [int(n.split()[1]) for n in names]
    assert numbers == sorted(numbers) and numbers[-1] - numbers[0] == len(numbers) - 1
    on_screen = read(page, include_text=False, viewport_only=True)
    shown = [int(e["name"].split()[1]) for e in on_screen["elements"]]
    assert 30 in shown and 0 not in shown and 59 not in shown and on_screen["truncated"] is False
    scope = page.evaluate("""() => { const s = document.querySelector("#rows li:nth-child(8)");
      s.setAttribute("role", "button"); s.setAttribute("aria-label", "Row seven"); return true; }""")
    assert scope
    row_ref = ref_of(read(page, include_text=False), "Row seven")
    scoped = read(page, scope_ref=row_ref)
    assert [e["name"] for e in scoped["elements"]] == ["Row seven", "Edit 7"]
    assert scoped["text"] == "Row 7 Edit 7"
    assert edit(7) == ref_of(scoped, "Edit 7")
    page.evaluate("() => document.querySelector('#rows li:nth-child(8)').remove()")
    assert read(page, scope_ref=row_ref) == {"error": "StaleElement"}


def test_read_cuts_a_long_url_to_fit_a_small_budget(page):
    # #11356: url and title alone over max_chars are cut to fit; the trim loop used to spin forever.
    page.goto("data:text/html,<title>" + "T" * 150 + "</title><button>Go</button>#" + "u" * 3000)
    load(page)
    small = read(page, max_chars=1000)
    assert small["truncated"] is True and small["text"] == "" and small["elements"] == []
    assert len(json.dumps(small, separators=(",", ":"), ensure_ascii=False).encode()) <= 1000
    assert small["url"].startswith("data:text/html,") and len(small["url"]) < 1000


def find(page, **terms):
    return page.evaluate("t => TrustyPages.pageFind(t)", terms)


def test_find_returns_matches_with_their_container(page):
    # #11357: ten identical Delete buttons come back distinguishable by row.
    page.evaluate("""() => {
      const table = document.createElement("table");
      table.innerHTML = "<caption>People</caption>" + Array.from({length:10}, (_, i) =>
        "<tr><td>Person " + i + "</td><td><button>Delete</button></td></tr>").join("");
      const nav = document.createElement("nav");
      nav.setAttribute("aria-label", "Site");
      nav.innerHTML = "<a href='/home'>Home</a><button>Delete</button>";
      document.body.append(table, nav);
    }""")
    rows = find(page, role="button", name="delete", limit=50)
    assert rows["truncated"] is False and len(rows["matches"]) == 11
    assert [m["in"]["name"] for m in rows["matches"][:10]] == ["Person %d Delete" % i for i in range(10)]
    assert {m["in"]["role"] for m in rows["matches"][:10]} == {"row"}
    assert rows["matches"][10]["in"] == {"role": "navigation", "name": "Site", "ref": rows["matches"][10]["in"]["ref"]}
    assert len({m["ref"] for m in rows["matches"]}) == 11
    assert [m["name"] for m in find(page, name="Go")["matches"]] == ["Go now"]
    # text matches the container's text, so this is the Delete button in Person 4's row.
    four = find(page, role="button", text="person 4")["matches"]
    assert len(four) == 1 and four[0]["in"]["name"] == "Person 4 Delete"
    assert click(page, four[0]["ref"]) == {"ok": True}
    assert page.evaluate("events").count("click:button") == 1
    # within_ref never returns an element outside its subtree.
    row = four[0]["in"]["ref"]
    inside = find(page, role="button", within_ref=row)
    assert [m["ref"] for m in inside["matches"]] == [four[0]["ref"]]
    nav = rows["matches"][10]["in"]["ref"]
    assert [m["name"] for m in find(page, name="e", within_ref=nav)["matches"]] == ["Home", "Delete"]
    assert find(page, role="link", within_ref=row)["matches"] == []
    limited = find(page, role="button", name="Delete", limit=3)
    assert len(limited["matches"]) == 3 and limited["truncated"] is True
    assert find(page, name="no such thing") == {"origin": "null", "matches": [], "truncated": False}
    page.evaluate("() => document.querySelectorAll('tr')[4].remove()")
    assert find(page, role="button", within_ref=row) == {"error": "StaleElement"}


def test_target_refuses_stale_refs_and_covered_points():
    # #11355: refs survive reads, so a ref goes stale only when its element leaves or changes.
    async def body(tab):
        first = await tab.read()
        go = ref_of(first, "Go now")
        assert ref_of(await tab.read(), "Go now") == go
        assert await tab.click("9.0") == {"error": "StaleElement"}
        await tab.page.evaluate("""() => {
          const cover = document.createElement("div"), box = go.getBoundingClientRect();
          cover.id = "cover";
          Object.assign(cover.style, {position:"fixed", left:box.left + "px", top:box.top + "px",
            width:box.width + "px", height:box.height + "px", zIndex:9});
          document.body.append(cover);
        }""")
        assert await tab.click(go) == {"error": "Obscured"}
        assert await tab.page.evaluate("events") == []
        await tab.page.evaluate("() => { cover.remove(); go.replaceWith(go.cloneNode(true)); }")
        assert await tab.click(go) == {"error": "StaleElement"}
        assert await tab.page.evaluate("events") == []
    trusted(body)


def test_target_reads_the_rect_after_an_instant_scroll_under_smooth_css(page):
    """#11294: smooth scrolling must not leave the rect read mid-scroll."""
    page.evaluate("""() => {
      document.documentElement.style.scrollBehavior = "smooth";
      const far = document.createElement("button");
      far.id = "far";
      far.textContent = "Far away";
      far.style.cssText = "position:absolute; top:6000px; left:40px";
      document.body.append(far);
    }""")
    far = ref_of(read(page), "Far away")
    point = page.evaluate("ref => TrustyPages.pageTarget(ref)", far)
    assert point.get("ok") is True, point
    page.wait_for_timeout(600)  # a smooth scroll started by the call would finish here
    final = page.evaluate("""() => { const b = far.getBoundingClientRect();
      return {x:b.left + b.width / 2, y:b.top + b.height / 2}; }""")
    assert (point["x"], point["y"]) == (final["x"], final["y"])


def test_trusted_click_arrives_trusted():
    async def body(tab):
        result = await tab.read()
        assert await tab.click(ref_of(result, "Go now")) == {"ok": True}
        assert await tab.page.evaluate("events") == [
            "pointerdown:go", "mousedown:go", "pointerup:go", "mouseup:go", "click:go"]
    trusted(body)


@pytest.mark.parametrize("policy", ["accept", "dismiss"])
def test_native_alert_and_confirm_are_answered(policy):
    async def body(tab):
        tab.policy = policy
        await tab.page.evaluate("""() => {
          const ask = document.createElement("button");
          ask.id = "ask"; ask.textContent = "Ask";
          ask.onclick = () => { alert("Heads up"); window.answer = confirm("Delete it?"); };
          document.body.prepend(ask);
        }""")
        result = await tab.read()
        assert await tab.click(ref_of(result, "Ask")) == {"ok": True}
        assert tab.dialogs == [("alert", "Heads up", True), ("confirm", "Delete it?", policy == "accept")]
        assert await tab.page.evaluate("window.answer") is (policy == "accept")
    trusted(body)


def test_type_refuses_stale_refs(page):
    second = read(page)
    assert page.evaluate("ref => TrustyPages.pageType(ref, 'x')", ref_of(second, "Email")) == {"ok": True}
    assert page.evaluate("ref => TrustyPages.pageType(ref, 'x')", "9.0") == {"error": "StaleElement"}
    assert page.evaluate("events") == ["input:email", "change:email"]


def test_refs_survive_reads_and_go_stale_when_the_element_changes(page):
    # #11355: a ref outlives reads of an unchanged page; a changed or removed element answers StaleElement.
    page.evaluate("""() => {
      for (const label of ["Delete A", "Delete B"])
        document.body.append(Object.assign(document.createElement("button"), {textContent:label}));
    }""")
    first = read(page)
    go, a, b, note = (ref_of(first, n) for n in ("Go now", "Delete A", "Delete B", "old note"))
    second = read(page)
    assert [e["ref"] for e in second["elements"]] == [e["ref"] for e in first["elements"]]
    assert click(page, go) == {"ok": True}
    page.evaluate("() => { window.events = []; }")
    # Swap the labels: neither old ref may click the button that now carries its name.
    page.evaluate("""() => { const [x, y] = [...document.querySelectorAll("button")].slice(-2);
      [x.textContent, y.textContent] = [y.textContent, x.textContent]; }""")
    assert click(page, a) == {"error": "StaleElement"}
    assert click(page, b) == {"error": "StaleElement"}
    # An identical replacement node is a different element.
    page.evaluate("go.replaceWith(go.cloneNode(true))")
    assert click(page, go) == {"error": "StaleElement"}
    assert page.evaluate("events") == []
    third = read(page)
    assert ref_of(third, "Delete A") not in (a, b) and ref_of(third, "Go now") != go
    # Editing a field's own text keeps its ref.
    assert page.evaluate("([r, t]) => TrustyPages.pageType(r, t)", [note, "new"]) == {"ok": True}
    assert page.evaluate("([r, t]) => TrustyPages.pageType(r, t)", [note, "newer"]) == {"ok": True}
    assert ref_of(read(page), "newer") == note


def test_type_sets_values_and_refuses_non_editable(page):
    result = read(page)
    typed = lambda name, text: page.evaluate("([r, t]) => TrustyPages.pageType(r, t)", [ref_of(result, name), text])
    assert typed("Go now", "x") == {"error": "NotEditable"}
    assert typed("Fixed", "x") == {"error": "NotEditable"}
    assert page.evaluate("events") == []
    assert typed("Email", "new@b.test") == {"ok": True}
    assert typed("Bio", "") == {"ok": True}
    assert typed("old note", "new note") == {"ok": True}
    assert page.evaluate("[email.value, bio.value, note.innerText.trim(), ro.value]") == [
        "new@b.test", "", "new note", "fixed"]


def test_type_reports_text_dropped_by_maxlength_or_a_cancelled_beforeinput(page):
    # #11352: a maxlength cut answers ok with its counts; a cancelled beforeinput never answers ok.
    page.evaluate("""() => {
      const short = Object.assign(document.createElement("input"), {id:"short", maxLength:3});
      short.setAttribute("aria-label", "Short");
      const guard = Object.assign(document.createElement("input"), {id:"guard"});
      guard.setAttribute("aria-label", "Guard");
      guard.addEventListener("beforeinput", e => { if (e.data === "x") e.preventDefault(); });
      document.body.append(short, guard);
    }""")
    result = read(page)
    typed = lambda name, text: page.evaluate("([r, t]) => TrustyPages.pageType(r, t)", [ref_of(result, name), text])
    # A maxlength cut succeeds and says how much landed.
    assert typed("Short", "abcdef") == {"ok": True, "truncated": True, "typed": 6, "accepted": 3}
    assert page.evaluate("short.value") == "abc"
    assert typed("Guard", "axb") == {"error": "TextDropped", "typed": 3, "accepted": 2, "entered": "ab"}
    assert page.evaluate("guard.value") == "ab"
    assert page.evaluate("([r, k]) => TrustyPages.pagePress(r, k, [])", [ref_of(result, "Short"), "z"]) == {
        "ok": True, "truncated": True, "typed": 1, "accepted": 0}
    assert typed("Short", "ab") == {"ok": True}


FIXTURE = Path(__file__).with_name("fixtures") / "select.html"


@pytest.fixture()
def select_page():
    with sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            tab = browser.new_page()
            tab.goto(FIXTURE.as_uri())
            load(tab)
            yield tab
        finally:
            browser.close()


def chose(page, ref, value):
    return page.evaluate("([r, v]) => TrustyPages.pageSelect(r, v)", [ref, value])


def page_log(page):
    return page.evaluate("[...document.querySelectorAll('#log li')].map(li => li.textContent)")


def test_select_fixture_logs_the_change(select_page):
    result = read(select_page)
    country = next(e for e in result["elements"] if e["name"].startswith("Country"))
    assert country["role"] == "combobox" and country["value"] == ""
    assert country["options"] == [
        {"value": "", "label": "Choose one", "selected": True},
        {"value": "us", "label": "United States"},
        {"value": "ca", "label": "Canada"},
        {"value": "mx", "label": "Mexico", "disabled": True},
    ]
    assert chose(select_page, country["ref"], "ca") == {"ok": True, "changed": True, "value": "ca"}
    assert page_log(select_page) == ["country input ca", "country change ca"]
    assert select_page.evaluate("country.value") == "ca"


def test_select_sets_value_and_reports_no_effect(select_page):
    result = read(select_page)
    country, sticky = (ref_of(result, n) for n in ("Country Choose one United States Canada Mexico", "Sticky Keep Other"))
    # A label matches when no option value does.
    assert chose(select_page, country, "United States") == {"ok": True, "changed": True, "value": "us"}
    assert chose(select_page, country, "us") == {"ok": True, "changed": False, "value": "us"}
    # A handler that reverts the choice is a no-effect call, not a success.
    assert chose(select_page, sticky, "other") == {"ok": True, "changed": False, "value": "keep"}
    assert page_log(select_page) == ["country input us", "country change us", "sticky input other", "sticky change other"]


def test_select_refuses_non_select_and_unknown_options(select_page):
    result = read(select_page)
    country = ref_of(result, "Country Choose one United States Canada Mexico")
    for name in ("Locked A B", "Many X", "Name"):
        assert chose(select_page, ref_of(result, name), "a") == {"error": "NotSelectable"}, name
    assert chose(select_page, country, "mx") == {"error": "NoSuchOption"}
    assert chose(select_page, country, "nowhere") == {"error": "NoSuchOption"}
    assert chose(select_page, "9.0", "ca") == {"error": "StaleElement"}
    assert page_log(select_page) == [] and select_page.evaluate("country.value") == ""


def test_url_floor(page):
    cases = {
        "https://a.test/x?y=1": True, "http://127.0.0.1:8080/": True, "http://localhost/": False,
        "http://a.test/": False, "javascript:alert(1)": False, "data:text/html,x": False,
        "file:///etc/hosts": False, "chrome://settings": False, "about:blank": False,
        "https://u:p@a.test/": False, "https://a.test/" + "x" * 8200: False, "": False,
    }
    for url, allowed in cases.items():
        assert page.evaluate("u => TrustyPages.urlAllowed(u)", url) is allowed, url


FEED = Path(__file__).with_name("fixtures") / "feed.html"


@pytest.fixture()
def feed():
    with sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            tab = browser.new_page(viewport={"width": 800, "height": 600})
            tab.goto(FEED.as_uri())
            load(tab)
            yield tab
        finally:
            browser.close()


# #11286: key events and default actions, on the fixture the live check also serves.
KEYS = Path(__file__).with_name("fixtures") / "keys.html"


@pytest.fixture()
def keys():
    with sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            tab = browser.new_page()
            tab.set_content(KEYS.read_text())
            load(tab)
            yield tab
        finally:
            browser.close()


# #11289: file selection and downloads against the loopback fixture.
@pytest.fixture()
def files_page():
    with file_site() as site, sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            tab = browser.new_page(accept_downloads=True)
            tab.goto(site.origin + "/")
            load(tab)
            yield tab, site
        finally:
            browser.close()


def scroll(page, to, ref=None, wait=1500):
    return page.evaluate("([t, r, w]) => TrustyPages.pageScroll(t, r, w)", [to, ref, wait])


def test_scroll_drives_an_infinite_feed_until_it_ends(feed):
    # #11291: scroll to the bottom until the feed stops growing.
    views = []
    for _ in range(6):
        reply = scroll(feed, "bottom")
        assert reply["ok"] is True
        views.append(reply["view"])
        if not reply["view"]["grew"]:
            break
    assert [v["grew"] for v in views] == [True, True, False]
    assert views[-1]["at_end"] is True and views[-1]["scroll_y"] > 0
    names = [e["name"] for e in read(feed)["elements"]]
    assert names[-1] == "Item 14" and len(names) == 16
    top = scroll(feed, "top", wait=0)["view"]
    assert top["scroll_y"] == 0 and top["at_end"] is False and top["grew"] is False
    down = scroll(feed, "down", wait=0)["view"]
    assert down["scroll_y"] == round(600 * 0.9)
    assert scroll(feed, "up", wait=0)["view"]["scroll_y"] == 0


def test_scroll_to_element_and_stale_refs_after_dom_change(feed):
    for _ in range(3):
        scroll(feed, "bottom")
    result = read(feed)
    last, first, replace = (ref_of(result, n) for n in ("Item 14", "Item 0", "Replace first"))
    assert scroll(feed, "top", wait=0)["ok"] is True
    # Scrolling runs no read, so refs from the latest read keep working.
    assert scroll(feed, "element", last, wait=0)["view"]["scroll_y"] > 0
    assert click(feed, replace) == {"ok": True}
    # The replaced node's ref answers StaleElement, never a click on its successor.
    assert click(feed, first) == {"error": "StaleElement"}
    assert scroll(feed, "element", first, wait=0) == {"error": "StaleElement"}
    assert click(feed, last) == {"ok": True}
    # A fresh read hands out a working ref for the new node.
    fresh = ref_of(read(feed), "Item 0")
    assert fresh != first
    assert click(feed, fresh) == {"ok": True}


def b64(text):
    return base64.b64encode(text.encode()).decode()


def set_files(tab, ref, files):
    return tab.evaluate("([r, f]) => TrustyPages.pageSetFiles(r, f)", [ref, files])


def test_set_files_reaches_the_page_and_the_server(files_page):
    tab, site = files_page
    result = read(tab)
    by_name = {e["name"]: e for e in result["elements"]}
    attachment = by_name["Attachment"]
    assert attachment["type"] == "file" and attachment["files"] == []
    assert "value" not in attachment and by_name["Many"]["multiple"] is True
    sample = {"name": "upload-example.txt", "type": "text/plain", "data": b64("hello file\n")}
    assert set_files(tab, attachment["ref"], [sample]) == {"ok": True}
    tab.wait_for_function("server.textContent.startsWith('received')")
    assert tab.inner_text("#picked") == "upload-example.txt:11:text/plain:hello file"
    assert b"hello file\n" in site.uploads[0]
    assert b'filename="upload-example.txt"' in site.uploads[0]
    again = read(tab)
    assert ref_entry(again, "Attachment")["files"] == ["upload-example.txt"]
    two = [sample, {**sample, "name": "b.txt", "data": b64("bee")}]
    assert set_files(tab, ref_of(again, "Many"), two) == {"ok": True}
    tab.wait_for_function("picked.textContent.includes('b.txt')")
    assert tab.inner_text("#picked").endswith("|b.txt:3:text/plain:bee")


def ref_entry(result, name):
    return next(e for e in result["elements"] if e["name"] == name)


def test_set_files_refuses_other_targets(files_page):
    tab, _ = files_page
    result = read(tab)
    one = [{"name": "a.txt", "type": "text/plain", "data": b64("a")}]
    assert set_files(tab, ref_of(result, "Download report"), one) == {"error": "NotEditable"}
    assert set_files(tab, ref_of(result, "Attachment"), one * 2) == {"error": "NotEditable"}
    assert set_files(tab, "9.0", one) == {"error": "StaleElement"}
    # PR #11305 delta review: data atob rejects answers NotEditable, not a throw.
    bad = [{"name": "a.txt", "type": "text/plain", "data": "a==="}]
    assert set_files(tab, ref_of(result, "Attachment"), bad) == {
        "error": "NotEditable"
    }
    off = ref_entry(result, "Disabled")
    assert off["disabled"] is True
    assert set_files(tab, off["ref"], one) == {"error": "NotEditable"}
    assert tab.evaluate("[file.files.length, picked.textContent]") == [0, ""]


def test_fixture_download_triggers_start_downloads(files_page):
    tab, _ = files_page
    result = read(tab)
    cases = [("Download report", "report.txt", DOWNLOAD_TEXT), ("Save blob", "generated.txt", "blob body")]
    for name, filename, body in cases:
        with tab.expect_download() as info:
            clicked = click(tab, ref_of(result, name))
            assert clicked == {"ok": True}
        download = info.value
        assert download.suggested_filename == filename
        assert Path(download.path()).read_text() == body
def press(tab, ref, key, modifiers=()):
    return tab.evaluate("([r, k, m]) => TrustyPages.pagePress(r, k, m)", [ref, key, list(modifiers)])


def log(tab):
    """The fixture's page log since the last call."""
    return tab.evaluate("() => window.keyLog.splice(0)")


def test_type_emits_key_events_and_enter_submits_the_form(keys):
    refs = {e["name"]: e["ref"] for e in read(keys)["elements"]}
    assert keys.evaluate("([r, t]) => TrustyPages.pageType(r, t)", [refs["User"], "aB\n"]) == {"ok": True}
    assert log(keys) == [
        "focus @user",
        "keydown a @user", "keypress a @user", "keyup a @user",
        "keydown Shift @user", "keydown Shift+B @user", "keypress Shift+B @user", "keyup Shift+B @user",
        "keyup Shift @user",
        # Implicit submission clicks the form's default button, as a browser does.
        "keydown Enter @user", "keypress Enter @user", "click @submit", "submit user=aB", "keyup Enter @user",
    ]
    assert keys.evaluate("user.value") == "aB"


def test_press_tab_and_shift_tab_follow_the_focus_order(keys):
    refs = {e["name"]: e["ref"] for e in read(keys)["elements"]}
    keys.evaluate("window.codes = []; addEventListener('keydown', e => codes.push([e.key, e.code, e.keyCode]))")
    assert press(keys, refs["First"], "Tab") == {"ok": True}
    assert log(keys) == ["focus @first", "keydown Tab @first", "focus @link", "keyup Tab @link"]
    assert keys.evaluate("codes") == [["Tab", "Tab", 9]]
    assert press(keys, None, "Tab") == {"ok": True}
    assert keys.evaluate("document.activeElement.id") == "third"
    log(keys)
    assert press(keys, None, "Tab", ["Shift"]) == {"ok": True}
    assert log(keys) == ["keydown Shift @third", "keydown Shift+Tab @third", "focus @link",
                         "keyup Shift+Tab @link", "keyup Shift @link"]
    # Backward from the first tabbable element wraps to the last one.
    press(keys, refs["User"], "Tab", ["Shift"])
    assert keys.evaluate("document.activeElement.id") == "editor"


def test_press_edits_fields_and_honours_chords_and_prevent_default(keys):
    refs = {e["name"]: e["ref"] for e in read(keys)["elements"]}

    def value():
        return keys.evaluate("third.value")

    keys.evaluate("([r, t]) => TrustyPages.pageType(r, t)", [refs["Third"], "abc"])
    for key, expected in [("ArrowLeft", "abc"), ("Backspace", "ac"), ("Home", "ac"), ("Delete", "c"),
                          ("End", "c"), ("d", "cd")]:
        assert press(keys, None, key) == {"ok": True}
        assert value() == expected, key
    for chord in (["Control"], ["Meta"]):
        press(keys, None, "a", chord)
        press(keys, None, "x")
        assert value() == "x", chord
    log(keys)
    press(keys, None, "Escape", ["Alt"])
    assert log(keys) == ["keydown Alt @third", "keydown Alt+Escape @third", "keyup Alt+Escape @third",
                         "keyup Alt @third"]
    keys.evaluate("third.addEventListener('keydown', e => { if (e.key === 'q') e.preventDefault(); })")
    press(keys, None, "q")
    assert value() == "x"
    # Enter inserts a line in a textarea and activates a link; Space types a space.
    press(keys, refs["Notes"], "a")
    press(keys, None, "Enter")
    press(keys, None, "Space")
    assert keys.evaluate("notes.value") == "a\n "
    log(keys)
    press(keys, refs["Link"], "Enter")
    assert "click @link" in log(keys) and keys.evaluate("location.hash") == "#linked"
    press(keys, refs["Editor"], "End")
    press(keys, None, "!")
    assert keys.evaluate("editor.innerText.trim()") == "hello!"


def test_press_refuses_a_stale_ref_and_changes_nothing(keys):
    refs = {e["name"]: e["ref"] for e in read(keys)["elements"]}
    keys.evaluate("user.replaceWith(user.cloneNode())")
    assert press(keys, refs["User"], "Enter") == {"error": "StaleElement"}
    assert log(keys) == []


NESTED = """
<title>Nested QA</title>
<button id="top">Top</button>
<div id="host"></div>
<div id="closed"></div>
<iframe id="frame" srcdoc="<label>Frame name <input id='fname' value='old'></label>
  <button id='fbtn'>Frame go</button><div id='fhost'></div>"></iframe>
<iframe id="away" style="display:none" srcdoc="<button>Hidden frame</button>"></iframe>
"""
# Builds the open/closed shadow roots and logs every action event, at its real target, into one list.
NESTED_SETUP = """async () => {
  const frame = document.getElementById("frame");
  if (frame.contentDocument?.readyState !== "complete" || !frame.contentDocument.getElementById("fhost"))
    await new Promise(done => frame.addEventListener("load", done, {once:true}));
  const open = document.getElementById("host").attachShadow({mode:"open"});
  open.innerHTML = "<button id='sbtn'>Shadow go</button><textarea id='snote' aria-label='Shadow note'></textarea>";
  document.getElementById("closed").attachShadow({mode:"closed"}).innerHTML = "<button>Closed go</button>";
  const inner = frame.contentDocument.getElementById("fhost").attachShadow({mode:"open"});
  inner.innerHTML = "<span id='lbl'>Deep label</span><input id='deep' aria-labelledby='lbl'>";
  window.events = [];
  for (const doc of [document, frame.contentDocument])
    for (const kind of ["click","input","change"])
      doc.addEventListener(kind, e => window.events.push(kind + ":" + e.composedPath()[0].id), true);
  // A native change event is not composed, so a page logs it inside the shadow root.
  for (const root of [open, inner])
    root.addEventListener("change", e => window.events.push("change:" + e.target.id), true);
}"""


def test_read_and_act_inside_iframes_and_shadow_roots(page):
    page.set_content(NESTED)
    load(page)
    page.evaluate(NESTED_SETUP)
    result = read(page)
    found = {e["name"]: e.get("within", "") for e in result["elements"]}
    assert found == {"Top": "", "Shadow go": "shadow", "Shadow note": "shadow", "Frame name": "iframe",
                     "Frame go": "iframe", "Deep label": "iframe/shadow"}
    assert "Closed go" not in json.dumps(result) and "Hidden frame" not in json.dumps(result)

    def act(js, name, *rest):
        return page.evaluate(js, [ref_of(result, name), *rest])

    for name, text in (("Shadow note", "s"), ("Frame name", "f"), ("Deep label", "d")):
        assert act("([r, t]) => TrustyPages.pageType(r, t)", name, text) == {"ok": True}
    assert page.evaluate("events") == ["input:snote", "change:snote",
                                       "input:fname", "change:fname", "input:deep", "change:deep"]
    assert page.evaluate("""() => {
      const f = document.getElementById("frame").contentDocument;
      return [document.getElementById("host").shadowRoot.getElementById("snote").value,
              f.getElementById("fname").value, f.getElementById("fhost").shadowRoot.getElementById("deep").value];
    }""") == ["s", "f", "d"]
    page.evaluate("() => document.getElementById('frame').remove()")
    assert click(page, ref_of(result, "Frame go")) == {"error": "StaleElement"}


def test_trusted_click_lands_inside_iframes_and_shadow_roots():
    # #11294 on #11290: CDP takes top-viewport pixels, so pageTarget adds each frame's offset; a frame
    # pushed down and right by margin, border and padding proves the offsets are summed.
    framed = NESTED.replace('<iframe id="frame"', '<iframe id="frame" style="margin:40px 0 0 90px;'
                            'border:7px solid;padding:11px 0 0 13px"')

    async def body(tab):
        await tab.page.evaluate(NESTED_SETUP)
        await tab.page.evaluate("""() => { window.hits = [];
          for (const doc of [document, document.getElementById("frame").contentDocument])
            doc.addEventListener("click", e => hits.push(e.composedPath()[0].id + (e.isTrusted ? "" : "?")), true); }""")
        result = await tab.read()
        for name in ("Shadow go", "Frame go"):
            assert await tab.click(ref_of(result, name)) == {"ok": True}
        assert await tab.page.evaluate("hits") == ["sbtn", "fbtn"]
        await tab.page.evaluate("""() => { const f = document.getElementById("frame").contentDocument,
          cover = f.createElement("div");
          cover.style.cssText = "position:fixed;inset:0;z-index:9"; f.body.append(cover); }""")
        assert await tab.click(ref_of(result, "Frame go")) == {"error": "Obscured"}
        assert await tab.page.evaluate("hits") == ["sbtn", "fbtn"]
    trusted(body, framed)


def test_resolve_refuses_an_element_whose_frame_or_host_left_the_document(page):
    # #11290: an element inside a removed iframe or a detached shadow host is still isConnected in its own
    # document or shadow tree; resolve() must answer StaleElement for every action, find and scope alike.
    page.set_content(NESTED)
    load(page)
    page.evaluate(NESTED_SETUP)
    result = read(page)
    frame_btn, shadow_btn = ref_of(result, "Frame go"), ref_of(result, "Shadow go")
    page.evaluate("() => { window.kept = document.getElementById('frame').contentDocument.getElementById('fbtn');"
                  " document.getElementById('frame').remove(); document.getElementById('host').remove(); }")
    assert page.evaluate("() => kept.isConnected") is True
    assert click(page, frame_btn) == {"error": "StaleElement"}
    assert page.evaluate("r => TrustyPages.pageType(r, 'x')", shadow_btn) == {"error": "StaleElement"}
    assert read(page, scope_ref=frame_btn) == {"error": "StaleElement"}
    assert find(page, within_ref=shadow_btn) == {"error": "StaleElement"}


SHADOW_FORM = """
<title>Shadow scope</title>
<form aria-label="Signup"><button id="light">Light go</button><div id="host"></div></form>
<button id="outside">Outside go</button>
"""


def test_scope_ref_and_find_cross_shadow_roots(page):
    # #11290: scope_ref and within_ref reach into open shadow roots; labels resolve in the element's own root.
    page.set_content(SHADOW_FORM)
    load(page)
    page.evaluate("""() => document.getElementById("host").attachShadow({mode:"open"}).innerHTML =
      "<span id='navlbl'>Inner nav</span><nav aria-labelledby='navlbl'><button>Shadow go</button></nav>" +
      "<span id='lbl'>Shadow field</span><input aria-labelledby='lbl'>" """)
    form = find(page, name="Light go")["matches"][0]["in"]
    assert form["role"] == "form" and form["name"] == "Signup"
    scoped = read(page, scope_ref=form["ref"])
    assert [(e["name"], e.get("within", "")) for e in scoped["elements"]] == [
        ("Light go", ""), ("Shadow go", "shadow"), ("Shadow field", "shadow")]
    inside = find(page, name="go", within_ref=form["ref"])["matches"]
    assert [m["name"] for m in inside] == ["Light go", "Shadow go"]
    assert inside[1]["within"] == "shadow" and inside[1]["in"]["role"] == "navigation"
    assert inside[1]["in"]["name"] == "Inner nav"
    assert click(page, inside[1]["ref"]) == {"ok": True}


FRAMED = """
<title>Framed distance</title>
<iframe id="near" style="border:0;height:120px" srcdoc="<button>Near go</button>"></iframe>
<div style="height:3000px"></div>
<iframe id="far" style="border:0;height:120px" srcdoc="<button>Far go</button>"></iframe>
"""


def test_iframe_reads_measure_distance_in_top_coordinates(page):
    # #11290: a button at the top of a frame 3000px down is far from the viewport, not at frame-local 0.
    page.set_content(FRAMED)
    page.wait_for_function("""() => [...document.querySelectorAll("iframe")]
      .every(f => f.contentDocument?.readyState === "complete" && f.contentDocument.querySelector("button"))""")
    load(page)
    names = [(e["name"], e.get("within")) for e in read(page, viewport_only=True)["elements"]]
    assert names == [("Near go", "iframe")]
    assert [e["name"] for e in read(page)["elements"]] == ["Near go", "Far go"]


HOVER = Path(__file__).with_name("fixtures") / "hover.html"
MOVES = ["pointerover", "mouseover", "pointerenter", "mouseenter", "pointerout", "mouseout",
         "pointerleave", "mouseleave", "pointermove", "mousemove"]


def test_hover_fires_enter_over_and_move_then_leaves_the_old_target(page):
    page.evaluate("""kinds => { window.moves = [];
      for (const kind of kinds) document.addEventListener(kind, e => {
        if (e.target.id) window.moves.push(kind + ":" + e.target.id + ":" + e.bubbles); }, true); }""", MOVES)
    result = read(page)
    go, bio = ref_of(result, "Go now"), ref_of(result, "Bio")
    hover = lambda ref: page.evaluate("ref => TrustyPages.pageHover(ref)", ref)
    reset = lambda: page.evaluate("() => { window.moves = []; }")
    assert hover(go) == {"ok": True}
    assert page.evaluate("moves") == ["pointerover:go:true", "mouseover:go:true", "pointerenter:go:false",
                                      "mouseenter:go:false", "pointermove:go:true", "mousemove:go:true"]
    reset()
    assert hover(go) == {"ok": True}
    assert page.evaluate("moves") == ["pointermove:go:true", "mousemove:go:true"]
    reset()
    assert hover(bio) == {"ok": True}
    assert page.evaluate("moves") == ["pointerout:go:true", "mouseout:go:true", "pointerleave:go:false",
                                      "mouseleave:go:false", "pointerover:bio:true", "mouseover:bio:true",
                                      "pointerenter:bio:false", "mouseenter:bio:false",
                                      "pointermove:bio:true", "mousemove:bio:true"]
    assert page.evaluate("events") == []  # hovering clicks nothing
    assert hover("9.0") == {"error": "StaleElement"}


def test_hover_fixture_reveals_the_hover_action():
    with sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            tab = browser.new_page()
            tab.goto(HOVER.as_uri())
            load(tab)
            hover = lambda result, name: tab.evaluate("r => TrustyPages.pageHover(r)", ref_of(result, name))
            first = read(tab)
            assert "Pricing" not in [e["name"] for e in first["elements"]]
            assert hover(first, "Products") == {"ok": True}
            assert tab.evaluate("log") == ["products:mouseover", "menu:mouseenter", "products:mousemove"]
            second = read(tab)
            assert click(tab, ref_of(second, "Pricing")) == {"ok": True}
            assert tab.evaluate("log")[-1] == "pricing:click"
            assert hover(read(tab), "Other") == {"ok": True}
            assert tab.evaluate("log")[-1] == "menu:mouseleave"
            assert "Pricing" not in [e["name"] for e in read(tab)["elements"]]
        finally:
            browser.close()


def test_hover_reaches_an_element_inside_a_same_origin_iframe(page):
    # #11287 on #11300: the ref resolves through the frame hops and each event's view is the frame's window.
    page.set_content(NESTED)
    load(page)
    page.evaluate(NESTED_SETUP)
    page.evaluate("""() => { window.moves = [];
      const frame = document.getElementById("frame").contentWindow;
      window.views = [];
      for (const doc of [document, frame.document])
        for (const kind of ["mouseover", "mouseenter", "mouseout", "mouseleave"])
          doc.addEventListener(kind, e => {
            if (e.target.id) window.moves.push(kind + ":" + e.target.id);
            window.views.push(e.view === e.target.ownerDocument.defaultView);
          }, true); }""")
    result = read(page)
    hover = lambda name: page.evaluate("r => TrustyPages.pageHover(r)", ref_of(result, name))
    assert hover("Frame go") == {"ok": True}
    moves = page.evaluate("moves")
    assert "mouseover:fbtn" in moves and "mouseenter:fbtn" in moves and "mouseenter:frame" in moves
    assert page.evaluate("views") and all(page.evaluate("views"))
    page.evaluate("() => { window.moves = []; }")
    assert hover("Top") == {"ok": True}  # frame -> top page: leaves the frame button and the frame element
    moves = page.evaluate("moves")
    assert "mouseout:fbtn" in moves and "mouseleave:fbtn" in moves and "mouseleave:frame" in moves
    assert "mouseover:top" in moves


# #11288: the drag fixture of the behavior lab (draggable item, bare-div drop zone), plus a zone
# that never accepts a drop.
DRAG_PAGE = """
<title>Drag QA</title>
<div id="drag" draggable="true">Drag this item</div>
<div id="drop" style="padding:30px">Drop item here</div>
<div class="drop-off" style="padding:30px">Nowhere</div>
<script>
  window.drops = [];
  drag.ondragstart = e => e.dataTransfer.setData("text/plain", "item");
  drop.ondragover = e => e.preventDefault();
  drop.ondrop = e => { e.preventDefault(); drops.push(e.dataTransfer.getData("text/plain")); };
</script>
"""
DRAG_EVENTS = ["dragstart", "drag", "dragenter", "dragleave", "dragover", "drop", "dragend"]


@pytest.fixture()
def drag_page(page):
    page.set_content(DRAG_PAGE)
    load(page)
    page.evaluate("""kinds => {
      window.events = [];
      for (const kind of kinds)
        document.addEventListener(kind, e => window.events.push(kind + ":" + (e.target.id || e.target.className)), true);
    }""", DRAG_EVENTS)
    return page


def drag(tab, source, target):
    return tab.evaluate("([s, t]) => TrustyPages.pageDrag(s, t)", [source, target])


def test_drag_fires_the_drag_sequence_and_drops(drag_page):
    result = read(drag_page)
    by_name = {e["name"]: e for e in result["elements"]}
    assert set(by_name) == {"Drag this item", "Drop item here", "Nowhere"}
    assert by_name["Drag this item"]["draggable"] is True
    item, zone, off = (ref_of(result, n) for n in ("Drag this item", "Drop item here", "Nowhere"))
    assert drag(drag_page, item, zone) == {"ok": True, "dropped": True}
    assert drag_page.evaluate("events") == [
        "dragstart:drag", "drag:drag", "dragenter:drop", "dragover:drop", "drop:drop", "dragend:drag"]
    assert drag_page.evaluate("drops") == ["item"]
    drag_page.evaluate("() => { window.events = []; }")
    assert drag(drag_page, item, off) == {"ok": True, "dropped": False}
    assert drag_page.evaluate("events") == [
        "dragstart:drag", "drag:drag", "dragenter:drop-off", "dragover:drop-off", "dragleave:drop-off",
        "dragend:drag"]
    assert drag(drag_page, item, item) == {"error": "NotDraggable"}
    # #11355: refs survive an unchanged read; a zone whose name changed no longer resolves.
    drag_page.evaluate("() => { document.getElementById('drop').textContent = 'Moved'; }")
    assert drag(drag_page, item, zone) == {"error": "StaleElement"}
    assert drag_page.evaluate("drops") == ["item"]


def test_read_reports_no_drop_named_elements_without_a_draggable(page):
    page.evaluate("() => { const d = document.createElement('div'); d.id = 'drop'; d.textContent = 'Zone'; document.body.append(d); }")
    assert "Zone" not in [e["name"] for e in read(page)["elements"]]


def test_drag_reaches_a_draggable_inside_an_open_shadow_root(page):
    # #11288 on #11290: the only draggable sits in an open shadow root, so the light DOM alone holds none;
    # the read still reports it, and the shadow's drop-named zone, and the drag lands there.
    page.set_content("<title>Shadow drag</title><div id='host'></div>")
    page.evaluate("""() => {
      const root = document.getElementById("host").attachShadow({mode: "open"});
      root.innerHTML = '<div id="item" draggable="true">Shadow item</div>' +
        '<div class="dropzone" style="padding:30px">Shadow zone</div>';
      window.drops = [];
      root.querySelector("#item").ondragstart = e => e.dataTransfer.setData("text/plain", "deep");
      const zone = root.querySelector(".dropzone");
      zone.ondragover = e => e.preventDefault();
      zone.ondrop = e => drops.push(e.dataTransfer.getData("text/plain"));
    }""")
    load(page)
    result = read(page)
    by_name = {e["name"]: e for e in result["elements"]}
    assert by_name["Shadow item"] | {"ref": None} == {
        "ref": None, "tag": "div", "role": "generic", "name": "Shadow item", "within": "shadow", "draggable": True}
    assert by_name["Shadow zone"]["within"] == "shadow"
    assert drag(page, by_name["Shadow item"]["ref"], by_name["Shadow zone"]["ref"]) == {"ok": True, "dropped": True}
    assert page.evaluate("drops") == ["deep"]
