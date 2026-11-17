"""Exercise the viewport border in a renderer, including accessibility and stale cleanup."""

from pathlib import Path

from playwright.sync_api import sync_playwright


def test_control_notice_is_visible_accessible_and_clears():
    from trusty_browser import service

    script = Path(service.__file__).with_name("extension") / "indicator_content.js"
    with sync_playwright() as driver:
        browser = driver.chromium.launch()
        try:
            page = browser.new_page(viewport={"width": 480, "height": 300})
            page.set_content("""
                <style>body { margin: 0 } main { height: 200vh }</style>
                <title>Indicator QA</title><main>QA page</main>
            """)
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
            label = "Trusty Browser is controlling this tab · travel-web-qa"
            border = page.get_by_role("status", name=label)
            assert border.is_visible()
            assert label in page.locator("html").aria_snapshot()
            assert border.inner_text() == ""
            for width, height in [(480, 300), (800, 600)]:
                page.set_viewport_size({"width": width, "height": height})
                page.evaluate("window.scrollTo(0, 100)")
                assert border.bounding_box() == {"x": 0, "y": 0, "width": width, "height": height}
            sides = border.evaluate("""el => ["Top", "Right", "Bottom", "Left"].map(
                side => getComputedStyle(el)["border" + side]
            )""")
            assert sides == ["3px solid rgb(183, 65, 14)"] * 4
            assert page.evaluate("document.elementFromPoint(1, 1).tagName") == "MAIN"
            page.evaluate("() => deliver({action:'clear',claim:'different'})")
            assert border.is_visible()
            page.evaluate("() => deliver({action:'clear',claim:'a'})")
            assert border.count() == 0
            page.evaluate("() => deliver({action:'show',claim:'b',workflow:'travel-web-qa',ttl_ms:50})")
            border.wait_for(state="detached")
        finally:
            browser.close()
