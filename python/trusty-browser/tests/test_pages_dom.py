"""Exercise the injected page functions of pages.js on a static page in Chromium (#11239)."""

import base64
import json
from pathlib import Path

import pytest
from file_io_fixture import DOWNLOAD_TEXT, file_site
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


FEED = Path(__file__).with_name("fixtures") / "feed.html"


@pytest.fixture()
def feed():
    with sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            tab = browser.new_page(viewport={"width": 800, "height": 600})
            tab.goto(FEED.as_uri())
            tab.add_script_tag(path=str(SCRIPT))
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
            tab.add_script_tag(path=str(SCRIPT))
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
    assert feed.evaluate("ref => TrustyPages.pageClick(ref)", replace) == {"ok": True}
    # The replaced node's ref answers StaleElement, never a click on its successor.
    assert feed.evaluate("ref => TrustyPages.pageClick(ref)", first) == {"error": "StaleElement"}
    assert scroll(feed, "element", first, wait=0) == {"error": "StaleElement"}
    assert feed.evaluate("ref => TrustyPages.pageClick(ref)", last) == {"ok": True}
    # A fresh read hands out a working ref for the new node.
    fresh = ref_of(read(feed), "Item 0")
    assert fresh != first
    assert feed.evaluate("ref => TrustyPages.pageClick(ref)", fresh) == {"ok": True}


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
            clicked = tab.evaluate("ref => TrustyPages.pageClick(ref)", ref_of(result, name))
            assert clicked == {"ok": True}
        download = info.value
        assert download.suggested_filename == filename
        assert Path(download.path()).read_text() == body
