"""Exercise the popup's single activation gesture, motion and recovery in Chromium."""

from pathlib import Path

from playwright.sync_api import expect, sync_playwright

from trusty_browser import service


def test_activation_popup():
    popup = Path(service.__file__).with_name("extension") / "setup.html"
    with sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            page = browser.new_page(viewport={"width": 284, "height": 300})
            page.add_init_script("""
                window.calls = [];
                let permitted = false;
                const target = {tab:1,url:'https://test.example/app'};
                const route = {...target,document:'doc-1',email:'qa@example.test',account:'QA'};
                window.chrome = {permissions:{request:async () => {
                    window.calls.push('grant'); return true;
                }},runtime:{sendMessage:async m => {
                    window.calls.push(m.type);
                    if (m.type === 'preview') return permitted ? {ok:true,route} :
                        {ok:true,...target,permissionPattern:'https://test.example/*'};
                    if (m.type === 'enable') {permitted = true; return {ok:true};}
                    if (m.type === 'activate') return {ok:true,workflow:'trusty-managed'};
                    if (m.type === 'code_preview') return {ok:true,paired:false,email:'qa@example.test',
                        profile:null,target:null};
                    throw Error('unexpected request');
                }}};
            """)
            page.goto(popup.as_uri())
            activate = page.get_by_role("button", name="Activate Extension")
            expect(activate).to_be_enabled()
            assert page.get_by_role("textbox").count() == 0
            head = page.locator("#robot > img")
            bounds = head.bounding_box()
            page.mouse.move(260, 80)
            for eye in page.locator(".eye").all():
                assert float(eye.evaluate('el => el.style.getPropertyValue("--look-x")')[:-1]) > 0
            page.mouse.move(5, 80)
            for eye in page.locator(".eye").all():
                assert float(eye.evaluate('el => el.style.getPropertyValue("--look-x")')[:-1]) < 0
            assert head.bounding_box() == bounds
            expect(head).to_have_css("transform", "none")
            page.emulate_media(reduced_motion="reduce")
            for iris in page.locator(".iris").all():
                expect(iris).to_have_css("transform", "none")
            activate.focus()
            page.keyboard.press("Enter")
            expect(page.get_by_role("status")).to_have_text("Extension active")
            page.wait_for_function("window.calls.filter(c => c === 'code_preview').length === 2")
            assert page.evaluate("window.calls") == [
                "code_preview", "preview", "grant", "enable", "preview", "activate", "code_preview"]
            expect(page.locator("#access")).to_be_hidden()
            page.get_by_text("Activation details", exact=True).click()
            expect(page.locator("#workflow")).to_have_text("trusty-managed")
            expect(page.locator("#account")).to_have_text("QA")
        finally:
            browser.close()


CODE_POPUP = """
    window.calls = [];
    window.access = %s;
    window.grants = [{grant:'g-0000000000aa',scope:'profile',profile:'p',state:'active',idle_s:65}];
    let active = 42;
    window.chrome = {
      tabs:{query:async q => { window.calls.push({query:q}); return [{id:active++}]; }},
      permissions:{request:async () => { window.calls.push('permission'); return false; }},
      runtime:{sendMessage:async m => {
        window.calls.push(m);
        if (m.type === 'preview') return {ok:true,enrolled:'w',route:{email:'qa@example.test',url:'https://a.test/',account:'QA'}};
        if (m.type === 'code_preview') return {ok:true,paired:false,email:'qa@example.test',profile:'p',
          target:{tab:m.tab,access:window.access,origin:window.access ? 'https://a.test' : null,title:window.access ? 'Alpha' : null}};
        if (m.type === 'grants') return {ok:true,grants:window.grants};
        if (m.type === 'mint') return {ok:true,code:'tb-7K3M9Q-1ZXD4R-H8V2NB-QW6TPA',grant:'g-0000000000bb',
          scope:m.scope,profile:'p',...(m.tab === undefined ? {} : {tab:m.tab}),expires_at:Date.now() / 1000 + 2.5};
        if (m.type === 'revoke_grant') return {ok:true,revoked:1};
        throw Error('unexpected request ' + m.type);
      }}};
"""


def sent(page, kind):
    return [c for c in page.evaluate("window.calls") if isinstance(c, dict) and c.get("type") == kind]


def test_code_popup_offers_tab_first_and_forgets_the_code():
    popup = Path(service.__file__).with_name("extension") / "setup.html"
    with sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            page = browser.new_page(viewport={"width": 284, "height": 700})
            page.add_init_script(CODE_POPUP % "true")
            page.goto(popup.as_uri())
            tab_choice = page.get_by_role("button", name="Give access to this tab")
            profile_choice = page.get_by_role("button", name="Give access to the whole profile (every tab in this profile)")
            expect(tab_choice).to_be_enabled()
            assert tab_choice.evaluate(
                "(a, b) => !!(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING)",
                profile_choice.element_handle())
            expect(page.locator("#target")).to_have_text("https://a.test — Alpha")
            expect(page.locator("#grants li")).to_contain_text("agent connected, idle 1:05")
            tab_choice.click()
            expect(page.locator("#code")).to_have_value("tb-7K3M9Q-1ZXD4R-H8V2NB-QW6TPA")
            expect(page.locator("#countdown")).to_have_text("0:03")
            calls = page.evaluate("window.calls")
            assert calls.index("permission") < calls.index(sent(page, "mint")[0])
            assert sent(page, "mint") == [{"type": "mint", "scope": "tab", "tab": 42}]
            assert [c for c in calls if isinstance(c, dict) and "query" in c] == [
                {"query": {"active": True, "currentWindow": True}}]
            assert all(c.get("tab") == 42 for c in sent(page, "code_preview"))
            expect(page.get_by_text("Code expired", exact=True)).to_be_visible(timeout=5000)
            expect(page.locator("#code")).to_have_value("")
            assert "tb-" not in page.locator("main").inner_text()
            page.get_by_role("button", name="Disconnect", exact=True).click()
            page.wait_for_function("window.calls.some(c => c.type === 'revoke_grant')")
            assert sent(page, "revoke_grant") == [{"type": "revoke_grant", "grant": "g-0000000000aa"}]
            page.get_by_role("button", name="New code").click()
            profile_choice.click()
            expect(page.locator("#code-scope")).to_have_text("Whole profile (every tab in this profile)")
            page.get_by_role("button", name="Cancel code").click()
            page.wait_for_function("window.calls.filter(c => c.type === 'revoke_grant').length === 2")
            assert sent(page, "revoke_grant")[-1] == {"type": "revoke_grant", "grant": "g-0000000000bb"}
            expect(page.locator("#code")).to_have_value("")
        finally:
            browser.close()


def test_code_popup_disables_the_tab_choice_without_page_access():
    popup = Path(service.__file__).with_name("extension") / "setup.html"
    with sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            page = browser.new_page(viewport={"width": 284, "height": 700})
            page.add_init_script(CODE_POPUP % "false")
            page.goto(popup.as_uri())
            expect(page.locator("#target")).to_have_text("Open a web page and click Trusty Browser on it")
            expect(page.get_by_role("button", name="Give access to this tab")).to_be_disabled()
            expect(page.get_by_role("button", name="Give access to the whole profile (every tab in this profile)")).to_be_enabled()
        finally:
            browser.close()


PAIR_POPUP = """
    window.calls = [];
    let paired = false, held = false;
    const removedListeners = [];
    // Chrome's own sequence: the worker removes https://*/* after the revoke reply, on `grant_end` last.
    const workerRemoves = () => setTimeout(() => {
      held = false;
      window.removed = true;
      for (const fn of removedListeners) fn({origins:['https://*/*'], permissions:[]});
    }, 50);
    window.chrome = {
      tabs:{query:async () => [{id:42}]},
      permissions:{request:async p => { window.calls.push({permission:p.origins}); held = true; return true; },
        contains:async () => held, onRemoved:{addListener:fn => removedListeners.push(fn)}},
      runtime:{sendMessage:async m => {
        window.calls.push(m.type);
        if (m.type === 'code_preview') return {ok:true,paired,email:'qa@example.test',profile:paired ? 'p' : null,
          target:{tab:m.tab,access:true,origin:'https://a.test',title:'Alpha'}};
        if (m.type === 'preview') return {ok:false,error:'Open a supported signed-in site in the active tab.'};
        if (m.type === 'pair') { paired = true; return {ok:true,profile:'p'}; }
        if (m.type === 'unpair') { paired = false; held = false; return {ok:true,profile:'p'}; }
        if (m.type === 'grants') return {ok:true,grants:[]};
        if (m.type === 'revoke_grant') { if (window.lastCodeEnds) workerRemoves(); return {ok:true,revoked:1}; }
        if (m.type === 'mint') return {ok:true,code:'tb-7K3M9Q-1ZXD4R-H8V2NB-QW6TPA',grant:'g-0000000000bb',
          scope:m.scope,profile:'p',tab:m.tab,expires_at:Date.now() / 1000 + 300};
        throw Error('unexpected request ' + m.type);
      }}};
"""


def permission_requests(page):
    return [c["permission"] for c in page.evaluate("window.calls") if isinstance(c, dict) and "permission" in c]


def test_pairing_popup_activates_mints_and_disconnects_a_profile():
    popup = Path(service.__file__).with_name("extension") / "setup.html"
    with sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            page = browser.new_page(viewport={"width": 284, "height": 800})
            page.add_init_script(PAIR_POPUP)
            page.goto(popup.as_uri())
            expect(page.get_by_text("Activate Trusty Browser in this profile", exact=True)).to_be_visible()
            expect(page.locator("#access")).to_be_hidden()
            expect(page.get_by_role("button", name="Activate Extension")).to_be_visible()
            page.get_by_role("button", name="Activate", exact=True).click()
            page.wait_for_function("window.calls.filter(c => c === 'code_preview').length === 2")
            calls = page.evaluate("window.calls")
            # Contract "All-https access": the Activate click sends `pair` only and requests nothing.
            assert calls.index("pair") < calls.index("code_preview", 1)
            assert permission_requests(page) == []
            expect(page.get_by_role("button", name="Give access to this tab")).to_be_enabled()
            expect(page.locator("#unpaired")).to_be_hidden()
            for workflow_control in ("#activate", "#details", "#transfer"):
                expect(page.locator(workflow_control)).to_be_hidden()
            expect(page.locator("#paired-email")).to_have_text("qa@example.test")
            # The first mint click asks for all-https access once, before it sends `mint`.
            page.get_by_role("button", name="Give access to this tab").click()
            expect(page.locator("#code")).to_have_value("tb-7K3M9Q-1ZXD4R-H8V2NB-QW6TPA")
            calls = page.evaluate("window.calls")
            assert permission_requests(page) == [["https://*/*"]]
            assert calls.index({"permission": ["https://*/*"]}) < calls.index("mint")
            # The permission is held now, so a second mint click mints without asking again.
            page.wait_for_function("window.calls.filter(c => c === 'code_preview').length === 3")
            page.get_by_role("button", name="Cancel code").click()
            page.get_by_role("button", name="Give access to this tab").click()
            page.wait_for_function("window.calls.filter(c => c === 'mint').length === 2")
            assert permission_requests(page) == [["https://*/*"]]
            page.get_by_role("button", name="Disconnect this profile").click()
            expect(page.get_by_text("Activate Trusty Browser in this profile", exact=True)).to_be_visible()
            expect(page.locator("#access")).to_be_hidden()
            expect(page.locator("#paired")).to_be_hidden()
            expect(page.locator("#code")).to_have_value("")
            assert "unpair" in page.evaluate("window.calls")
            assert "tb-" not in page.locator("main").inner_text()
            expect(page.get_by_role("button", name="Activate Extension")).to_be_visible()
        finally:
            browser.close()


def test_a_mint_click_asks_again_after_the_worker_removed_the_permission():
    popup = Path(service.__file__).with_name("extension") / "setup.html"
    with sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            page = browser.new_page(viewport={"width": 284, "height": 800})
            page.add_init_script("window.lastCodeEnds = true;")
            page.add_init_script(PAIR_POPUP)
            page.goto(popup.as_uri())
            page.get_by_role("button", name="Activate", exact=True).click()
            page.get_by_role("button", name="Give access to this tab").click()
            expect(page.locator("#code")).to_have_value("tb-7K3M9Q-1ZXD4R-H8V2NB-QW6TPA")
            page.wait_for_function("window.calls.filter(c => c === 'code_preview').length === 3")
            assert permission_requests(page) == [["https://*/*"]]
            # Cancel code ends the last code; the worker then removes https://*/* (contract "All-https access").
            page.get_by_role("button", name="Cancel code").click()
            page.wait_for_function("window.removed === true")
            page.get_by_role("button", name="Give access to this tab").click()
            page.wait_for_function("window.calls.filter(c => c === 'mint').length === 2")
            calls = page.evaluate("window.calls")
            assert permission_requests(page) == [["https://*/*"], ["https://*/*"]]
            second_ask = len(calls) - 1 - calls[::-1].index({"permission": ["https://*/*"]})
            second_mint = len(calls) - 1 - calls[::-1].index("mint")
            assert second_ask < second_mint
        finally:
            browser.close()
