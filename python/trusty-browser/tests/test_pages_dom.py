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
            tab.add_script_tag(path=str(SCRIPT))
            hover = lambda result, name: tab.evaluate("r => TrustyPages.pageHover(r)", ref_of(result, name))
            first = read(tab)
            assert "Pricing" not in [e["name"] for e in first["elements"]]
            assert hover(first, "Products") == {"ok": True}
            assert tab.evaluate("log") == ["products:mouseover", "menu:mouseenter", "products:mousemove"]
            second = read(tab)
            assert tab.evaluate("r => TrustyPages.pageClick(r)", ref_of(second, "Pricing")) == {"ok": True}
            assert tab.evaluate("log")[-1] == "pricing:click"
            assert hover(read(tab), "Other") == {"ok": True}
            assert tab.evaluate("log")[-1] == "menu:mouseleave"
            assert "Pricing" not in [e["name"] for e in read(tab)["elements"]]
        finally:
            browser.close()
