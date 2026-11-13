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
                    throw Error('unexpected request');
                }}};
            """)
            page.goto(popup.as_uri())
            activate = page.get_by_role("button", name="Activate Extension")
            expect(activate).to_be_enabled()
            assert page.get_by_role("textbox").count() == 0
            page.mouse.move(260, 80)
            robot = page.locator("#robot")
            assert "rotateY(" in robot.get_attribute("style")
            page.emulate_media(reduced_motion="reduce")
            expect(robot).to_have_css("transform", "none")
            activate.focus()
            page.keyboard.press("Enter")
            expect(page.get_by_role("status")).to_have_text("Extension active")
            assert page.evaluate("window.calls") == ["preview", "grant", "enable", "preview", "activate"]
            page.get_by_text("Activation details", exact=True).click()
            expect(page.locator("#workflow")).to_have_text("trusty-managed")
            expect(page.locator("#account")).to_have_text("QA")
        finally:
            browser.close()
