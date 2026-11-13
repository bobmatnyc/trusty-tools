"""Exercise the actual banner in a renderer, including accessibility and stale cleanup."""

from pathlib import Path

from playwright.sync_api import sync_playwright


def test_control_notice_is_visible_accessible_and_clears():
    from trusty_browser import service
    script = Path(service.__file__).with_name("extension") / "indicator_content.js"
    with sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            page = browser.new_page(viewport={"width": 480, "height": 300})
            page.set_content("<title>Indicator QA</title><main>QA page</main>")
            page.evaluate("""() => {
              window.chrome = {runtime:{id:"test",onMessage:{addListener(listener){window.listener=listener;}}}};
              window.deliver = m => new Promise(resolve => {
                window.listener({...m,type:"trusty-control-indicator"}, {id:"test"}, resolve)
              });
            }""")
            page.add_script_tag(path=str(script))
            assert page.evaluate(
                "() => deliver({action:'show',claim:'a',workflow:'travel-web-qa',ttl_ms:30000})"
            ) == {"shown": True}
            banner = "Trusty Browser is controlling this tab · travel-web-qa"
            assert banner in page.locator("html").aria_snapshot()
            page.evaluate("() => deliver({action:'clear',claim:'different'})")
            assert banner in page.locator("html").aria_snapshot()
            page.evaluate("() => deliver({action:'clear',claim:'a'})")
            assert banner not in page.locator("html").aria_snapshot()
        finally:
            browser.close()
