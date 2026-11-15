"""Exercise the injected page functions of pages.js on a static page in Chromium (#11239)."""

import json
from pathlib import Path

import pytest
from playwright.sync_api import sync_playwright

from trusty_browser import service

SCRIPT = Path(service.__file__).with_name("extension") / "pages.js"
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
            tab.add_script_tag(path=str(SCRIPT))
            tab.evaluate("""() => {
              window.events = [];
              for (const kind of ["pointerdown","mousedown","pointerup","mouseup","click","input","change"])
                document.addEventListener(kind, e => window.events.push(kind + ":" + (e.target.id || e.target.localName)), true);
            }""")
            yield tab
        finally:
            browser.close()


def read(page):
    return page.evaluate("() => TrustyPages.pageRead(512)")


def ref_of(result, name):
    return next(e["ref"] for e in result["elements"] if e["name"] == name)


def test_read_reports_elements_and_bounds_size(page):
    result = read(page)
    names = [e["name"] for e in result["elements"]]
    assert names == ["Next page", "Go now", "Email", "Password", "Fixed", "old note", "Bio"]
    assert all(e["ref"].startswith("1.") for e in result["elements"])
    by_name = {e["name"]: e for e in result["elements"]}
    assert by_name["Next page"]["role"] == "link"
    assert by_name["Next page"]["href"].endswith("/next")
    assert by_name["Email"] == {"ref": "1.2", "tag": "input", "role": "textbox", "name": "Email",
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
    assert all(e["ref"].startswith("2.") for e in big["elements"])


def test_click_and_type_refuse_stale_refs(page):
    first = read(page)
    go = ref_of(first, "Go now")
    second = read(page)
    assert page.evaluate("ref => TrustyPages.pageClick(ref)", go) == {"error": "StaleElement"}
    assert page.evaluate("events") == []
    fresh = ref_of(second, "Go now")
    assert page.evaluate("ref => TrustyPages.pageClick(ref)", fresh) == {"ok": True}
    assert page.evaluate("events") == ["pointerdown:go", "mousedown:go", "pointerup:go", "mouseup:go", "click:go"]
    page.evaluate("() => { window.events = []; document.getElementById('go').remove(); }")
    assert page.evaluate("ref => TrustyPages.pageClick(ref)", fresh) == {"error": "StaleElement"}
    assert page.evaluate("ref => TrustyPages.pageType(ref, 'x')", ref_of(second, "Email")) == {"ok": True}
    assert page.evaluate("ref => TrustyPages.pageType(ref, 'x')", "9.0") == {"error": "StaleElement"}
    assert page.evaluate("events") == ["input:email", "change:email"]


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


def test_url_floor(page):
    cases = {
        "https://a.test/x?y=1": True, "http://127.0.0.1:8080/": True, "http://localhost/": False,
        "http://a.test/": False, "javascript:alert(1)": False, "data:text/html,x": False,
        "file:///etc/hosts": False, "chrome://settings": False, "about:blank": False,
        "https://u:p@a.test/": False, "https://a.test/" + "x" * 8200: False, "": False,
    }
    for url, allowed in cases.items():
        assert page.evaluate("u => TrustyPages.urlAllowed(u)", url) is allowed, url


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
    page.add_script_tag(path=str(SCRIPT))
    page.evaluate("""kinds => {
      window.events = [];
      for (const kind of kinds)
        document.addEventListener(kind, e => window.events.push(kind + ":" + (e.target.id || e.target.className)), true);
    }""", DRAG_EVENTS)
    return page


def test_drag_fires_the_drag_sequence_and_drops(drag_page):
    result = read(drag_page)
    by_name = {e["name"]: e for e in result["elements"]}
    assert set(by_name) == {"Drag this item", "Drop item here", "Nowhere"}
    assert by_name["Drag this item"]["draggable"] is True
    drag = lambda source, target: drag_page.evaluate(
        "([s, t]) => TrustyPages.pageDrag(s, t)", [ref_of(result, source), ref_of(result, target)])
    assert drag("Drag this item", "Drop item here") == {"ok": True, "dropped": True}
    assert drag_page.evaluate("events") == [
        "dragstart:drag", "drag:drag", "dragenter:drop", "dragover:drop", "drop:drop", "dragend:drag"]
    assert drag_page.evaluate("drops") == ["item"]
    drag_page.evaluate("() => { window.events = []; }")
    assert drag("Drag this item", "Nowhere") == {"ok": True, "dropped": False}
    assert drag_page.evaluate("events") == [
        "dragstart:drag", "drag:drag", "dragenter:drop-off", "dragover:drop-off", "dragleave:drop-off",
        "dragend:drag"]
    assert drag("Drag this item", "Drag this item") == {"error": "NotDraggable"}
    read(drag_page)
    assert drag("Drag this item", "Drop item here") == {"error": "StaleElement"}
    assert drag_page.evaluate("drops") == ["item"]


def test_read_reports_no_drop_named_elements_without_a_draggable(page):
    page.evaluate("() => { const d = document.createElement('div'); d.id = 'drop'; d.textContent = 'Zone'; document.body.append(d); }")
    assert "Zone" not in [e["name"] for e in read(page)["elements"]]
