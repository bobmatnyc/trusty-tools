"""Exercise the injected page functions of pages.js on a static page in Chromium (#11239, #11294)."""

import asyncio
import json
from pathlib import Path

import pytest
from playwright.async_api import async_playwright
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


KINDS = ["pointerdown", "mousedown", "pointerup", "mouseup", "click", "keydown", "keyup",
         "beforeinput", "input", "change"]
LOG = """kinds => {
  window.events = [];
  for (const kind of kinds)
    document.addEventListener(kind, e => window.events.push(
      kind + ":" + (e.target.id || e.target.localName) + (e.isTrusted ? "" : ":untrusted")), true);
}"""


@pytest.fixture()
def page():
    with sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            tab = browser.new_page()
            tab.set_content(PAGE)
            tab.add_script_tag(path=str(SCRIPT))
            tab.evaluate(LOG, KINDS)
            yield tab
        finally:
            browser.close()


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
        return await self.page.evaluate("() => TrustyPages.pageRead(512)")

    async def act(self, ref, kind, text=""):
        """pageTarget then the trusted CDP driver, as the worker's ``act`` runs them."""
        return await self.page.evaluate("""async ([ref, kind, text]) => {
          const point = TrustyPages.pageTarget(ref, kind);
          if (!point.ok) return point;
          const send = (method, params = {}) => window.__cdp(method, params);
          if (kind === "click") await TrustyPages.trusted.click(send, point);
          else await TrustyPages.trusted.type(send, text);
          return {ok:true};
        }""", [ref, kind, text])


def trusted(body):
    """Run ``body(tab)`` against PAGE in a fresh Chromium, bounded to 60 s."""
    async def main():
        async with async_playwright() as driver:
            browser = await driver.chromium.launch()
            try:
                page = await browser.new_page()
                await page.set_content(PAGE)
                await page.add_script_tag(path=str(SCRIPT))
                cdp = await page.context.new_cdp_session(page)
                await cdp.send("Page.enable")
                tab = Tab(page, cdp)
                cdp.on("Page.javascriptDialogOpening", tab.answer)
                page.on("dialog", lambda _dialog: None)  # leave every dialog to the CDP session

                async def send(method, params):
                    return await cdp.send(method, params)

                await page.expose_function("__cdp", send)
                await page.evaluate(LOG, KINDS)
                await body(tab)
            finally:
                await browser.close()

    asyncio.run(asyncio.wait_for(main(), 60))


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


def test_target_refuses_stale_refs_and_covered_points():
    async def body(tab):
        first = await tab.read()
        go = ref_of(first, "Go now")
        second = await tab.read()
        assert await tab.act(go, "click") == {"error": "StaleElement"}
        assert await tab.page.evaluate("events") == []
        await tab.page.evaluate("""() => {
          const cover = document.createElement("div"), box = go.getBoundingClientRect();
          cover.id = "cover";
          Object.assign(cover.style, {position:"fixed", left:box.left + "px", top:box.top + "px",
            width:box.width + "px", height:box.height + "px", zIndex:9});
          document.body.append(cover);
        }""")
        fresh = ref_of(second, "Go now")
        assert await tab.act(fresh, "click") == {"error": "Obscured"}
        assert await tab.page.evaluate("events") == []
        await tab.page.evaluate("() => { cover.remove(); go.remove(); }")
        assert await tab.act(fresh, "click") == {"error": "StaleElement"}
        assert await tab.act("9.0", "type", "x") == {"error": "StaleElement"}
    trusted(body)


def test_trusted_click_and_type_arrive_trusted():
    async def body(tab):
        result = await tab.read()
        events = lambda: tab.page.evaluate("() => { const seen = events; window.events = []; return seen; }")
        assert await tab.act(ref_of(result, "Go now"), "click") == {"ok": True}
        assert await events() == ["pointerdown:go", "mousedown:go", "pointerup:go", "mouseup:go", "click:go"]
        assert await tab.act(ref_of(result, "Email"), "type", "ab") == {"ok": True}
        assert await events() == ["keydown:email", "beforeinput:email", "input:email", "keyup:email"] * 2
        assert await tab.act(ref_of(result, "Bio"), "type", "one\ntwo") == {"ok": True}
        assert [e for e in await events() if e.endswith(":untrusted")] == []
        assert await tab.page.evaluate("[email.value, bio.value]") == ["ab", "one\ntwo"]
    trusted(body)


def test_type_sets_values_and_refuses_non_editable():
    async def body(tab):
        result = await tab.read()
        typed = lambda name, text: tab.act(ref_of(result, name), "type", text)
        assert await typed("Go now", "x") == {"error": "NotEditable"}
        assert await typed("Fixed", "x") == {"error": "NotEditable"}
        assert await tab.page.evaluate("events") == []
        assert await typed("Email", "new@b.test") == {"ok": True}
        assert await typed("Bio", "") == {"ok": True}
        assert await typed("old note", "new note") == {"ok": True}
        assert await tab.page.evaluate("[email.value, bio.value, note.innerText.trim(), ro.value]") == [
            "new@b.test", "", "new note", "fixed"]
        assert await tab.page.evaluate("events.filter(e => e.endsWith(':untrusted'))") == []
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
        assert await tab.act(ref_of(result, "Ask"), "click") == {"ok": True}
        assert tab.dialogs == [("alert", "Heads up", True), ("confirm", "Delete it?", policy == "accept")]
        assert await tab.page.evaluate("window.answer") is (policy == "accept")
    trusted(body)


def test_url_floor(page):
    cases = {
        "https://a.test/x?y=1": True, "http://127.0.0.1:8080/": True, "http://localhost/": False,
        "http://a.test/": False, "javascript:alert(1)": False, "data:text/html,x": False,
        "file:///etc/hosts": False, "chrome://settings": False, "about:blank": False,
        "https://u:p@a.test/": False, "https://a.test/" + "x" * 8200: False, "": False,
    }
    for url, allowed in cases.items():
        assert page.evaluate("u => TrustyPages.urlAllowed(u)", url) is allowed, url
