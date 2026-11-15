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
    page.add_script_tag(path=str(SCRIPT))
    page.evaluate(NESTED_SETUP)
    result = read(page)
    found = {e["name"]: e.get("within", "") for e in result["elements"]}
    assert found == {"Top": "", "Shadow go": "shadow", "Shadow note": "shadow", "Frame name": "iframe",
                     "Frame go": "iframe", "Deep label": "iframe/shadow"}
    assert "Closed go" not in json.dumps(result) and "Hidden frame" not in json.dumps(result)

    def act(js, name, *rest):
        return page.evaluate(js, [ref_of(result, name), *rest])

    for name in ("Shadow go", "Frame go"):
        assert act("([r]) => TrustyPages.pageClick(r)", name) == {"ok": True}
    for name, text in (("Shadow note", "s"), ("Frame name", "f"), ("Deep label", "d")):
        assert act("([r, t]) => TrustyPages.pageType(r, t)", name, text) == {"ok": True}
    assert page.evaluate("events") == ["click:sbtn", "click:fbtn", "input:snote", "change:snote",
                                       "input:fname", "change:fname", "input:deep", "change:deep"]
    assert page.evaluate("""() => {
      const f = document.getElementById("frame").contentDocument;
      return [document.getElementById("host").shadowRoot.getElementById("snote").value,
              f.getElementById("fname").value, f.getElementById("fhost").shadowRoot.getElementById("deep").value];
    }""") == ["s", "f", "d"]
    page.evaluate("() => document.getElementById('frame').remove()")
    assert act("([r]) => TrustyPages.pageClick(r)", "Frame go") == {"error": "StaleElement"}
