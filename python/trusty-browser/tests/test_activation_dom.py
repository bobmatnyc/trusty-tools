"""Exercise the popup's one-click Connect, Disconnect, motion and recovery in Chromium (#11239)."""

from pathlib import Path

from playwright.sync_api import expect, sync_playwright

from trusty_browser import service

POPUP = Path(service.__file__).with_name("extension") / "setup.html"

# A fake worker: `status` reflects whether a grant stands; `connect` makes one unless told to fail.
FAKE = """
    window.calls = [];
    window.held = %(held)s;
    window.connectFails = %(fails)s;
    let grant = null;
    window.chrome = {
      permissions:{
        contains:async () => window.held,
        request:async () => { window.calls.push('permission'); window.held = window.allow !== false; return window.held; },
      },
      runtime:{sendMessage:async m => {
        window.calls.push(m.type);
        if (m.type === 'status') return {ok:true, email:'qa@example.test', profile:grant ? 'p' : null, connected:grant};
        if (m.type === 'connect') {
          if (window.refusal) return window.refusal;
          if (window.connectFails) return {ok:false, error:'AuditUnavailable'};
          grant = 'g-0000000000aa'; return {ok:true, grant, profile:'p'};
        }
        if (m.type === 'revoke_grants') { grant = null; return {ok:true, revoked:1}; }
        if (m.type === 'unpair') { grant = null; return {ok:true, profile:'p'}; }
        throw Error('unexpected request ' + m.type);
      }}};
"""


def popup(driver, held="false", fails="false", height=420):
    browser = driver.chromium.launch()
    page = browser.new_page(viewport={"width": 284, "height": height})
    page.add_init_script(FAKE % {"held": held, "fails": fails})
    page.goto(POPUP.as_uri())
    return browser, page


def test_connect_is_one_click_and_disconnect_revokes_at_once():
    with sync_playwright() as driver:
        browser, page = popup(driver)
        try:
            connect = page.get_by_role("button", name="Connect", exact=True)
            expect(connect).to_be_enabled()
            assert page.get_by_role("textbox").count() == 0  # no code, nothing to paste
            connect.click()
            expect(page.locator("#badge")).to_be_visible()
            expect(page.locator("#badge")).to_have_text("Connected")
            expect(page.locator("#connected-email")).to_have_text("qa@example.test")
            calls = page.evaluate("window.calls")
            # The permission is asked inside the click, before the worker connects.
            assert calls == ["status", "permission", "connect", "status"]
            assert "tb-" not in page.locator("main").inner_text()
            page.get_by_role("button", name="Disconnect").click()
            expect(connect).to_be_visible()
            expect(page.locator("#online")).to_be_hidden()
            expect(page.get_by_role("status")).to_have_text("Disconnected")
            assert page.evaluate("window.calls").count("revoke_grants") == 1
        finally:
            browser.close()


def test_a_connect_whose_grant_creation_fails_does_not_show_connected():
    with sync_playwright() as driver:
        browser, page = popup(driver, held="true", fails="true")
        try:
            page.get_by_role("button", name="Connect", exact=True).click()
            expect(page.get_by_role("status")).to_have_text("AuditUnavailable")
            expect(page.locator("#online")).to_be_hidden()
            expect(page.get_by_role("button", name="Connect", exact=True)).to_be_enabled()
            # A held permission is not asked again.
            assert "permission" not in page.evaluate("window.calls")
        finally:
            browser.close()


def test_a_refused_connect_shows_the_broker_reason_code():
    # Owner ruling Dqw 2026-11-16: never a bare "Unauthorized".
    with sync_playwright() as driver:
        browser, page = popup(driver, held="true")
        try:
            page.evaluate(
                "window.refusal = {ok:false, error:'Unauthorized', reason:'host_codesign'}"
            )
            page.get_by_role("button", name="Connect", exact=True).click()
            expect(page.get_by_role("status")).to_have_text("Unauthorized (host_codesign)")
            expect(page.locator("#online")).to_be_hidden()
        finally:
            browser.close()


def test_forget_unpairs_and_disconnect_does_not():
    with sync_playwright() as driver:
        browser, page = popup(driver, held="true")
        try:
            connect = page.get_by_role("button", name="Connect", exact=True)
            connect.click()
            page.get_by_role("button", name="Disconnect").click()
            expect(page.get_by_role("status")).to_have_text("Disconnected")
            assert "unpair" not in page.evaluate("window.calls")
            connect.click()
            forget = page.get_by_role("link", name="Forget this profile")
            expect(forget).to_be_visible()
            forget.click()
            expect(page.get_by_role("status")).to_have_text("Forgotten")
            expect(connect).to_be_visible()
            assert page.evaluate("window.calls").count("unpair") == 1
        finally:
            browser.close()


def test_a_declined_permission_does_not_connect():
    with sync_playwright() as driver:
        browser, page = popup(driver)
        try:
            page.evaluate("window.allow = false")
            page.get_by_role("button", name="Connect", exact=True).click()
            expect(page.get_by_role("status")).to_contain_text("needs access to your sites")
            assert "connect" not in page.evaluate("window.calls")
            expect(page.locator("#online")).to_be_hidden()
        finally:
            browser.close()


def test_the_robot_follows_the_pointer_and_holds_still_under_reduced_motion():
    with sync_playwright() as driver:
        browser, page = popup(driver, height=300)
        try:
            head = page.locator("#robot > img")
            bounds = head.bounding_box()
            page.mouse.move(260, 80)
            for eye in page.locator(".eye").all():
                assert float(eye.evaluate('el => el.style.getPropertyValue("--look-x")')[:-1]) > 0
            page.mouse.move(5, 80)
            for eye in page.locator(".eye").all():
                assert float(eye.evaluate('el => el.style.getPropertyValue("--look-x")')[:-1]) < 0
            assert head.bounding_box() == bounds
            page.emulate_media(reduced_motion="reduce")
            for iris in page.locator(".iris").all():
                expect(iris).to_have_css("transform", "none")
        finally:
            browser.close()


def test_an_unreachable_service_offers_retry():
    with sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            page = browser.new_page()
            page.add_init_script("""
                window.chrome = {permissions:{contains:async () => false},
                  runtime:{sendMessage:async () => ({ok:false, error:'Specified native messaging host not found.'})}};
            """)
            page.goto(POPUP.as_uri())
            expect(page.get_by_role("button", name="Retry")).to_be_visible()
            expect(page.get_by_role("status")).to_contain_text("could not reach its local service")
        finally:
            browser.close()
