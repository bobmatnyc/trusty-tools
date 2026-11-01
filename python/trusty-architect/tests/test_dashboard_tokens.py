"""Dashboard design system (owner ruling D5e, brief local/briefs/2026-10-24-dash-design-system.md): one shared token file.

Why: three pages used to carry hand copies of their colour tokens and drifted (two state vocabularies, "working" a different
colour per page, a failing dark chart palette). tokens.css is the single source; this file keeps it that way.
What: the /static/tokens.css route and its rev, the light / OS-dark / data-theme-dark blocks, no raw colour in a page's CSS, every
var(--x) a page uses is defined, a golden table of the validated hex values, text contrast of the -ink tokens, one set of context bands.
Run: python3 -m unittest tests/test_dashboard_tokens.py
Test: tests/test_dashboard_tokens.py
"""
import os
import re
import shutil
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import dashboard_store as ds  # noqa: E402
from test_dashboard_store import ServerCase, serve  # noqa: E402

DASH = os.path.join(os.path.dirname(HERE), "templates", "dashboard")
TOKENS = os.path.join(DASH, "tokens.css")
PAGES = {n: os.path.join(DASH, n) for n in ("index.html", "live.html")}   # #10571: preview-live.html is not shipped


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def strip_comments(css):
    return re.sub(r"/\*.*?\*/", "", css, flags=re.S)


def style_text(html):
    return strip_comments("\n".join(re.findall(r"<style>(.*?)</style>", html, re.S)))


def blocks(css):
    """{selector-header: {token: value}} for the top-level :root blocks (the OS-dark block is nested in its @media)."""
    css = strip_comments(css)
    out = {}
    for m in re.finditer(r"(:root[^{]*)\{([^{}]*)\}", css):
        decls = {}
        for d in m.group(2).split(";"):
            if ":" in d:
                k, v = d.split(":", 1)
                decls[k.strip()] = re.sub(r"\s+", " ", v.strip())
        out.setdefault(re.sub(r"\s+", " ", m.group(1).strip()), []).append(decls)
    return out


def hex_rgb(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) / 255 for i in (0, 2, 4))


def luminance(h):
    f = lambda c: c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4   # noqa: E731
    r, g, b = (f(c) for c in hex_rgb(h))
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def contrast(a, b):
    la, lb = sorted((luminance(a), luminance(b)), reverse=True)
    return (la + 0.05) / (lb + 0.05)


LIGHT = blocks(read(TOKENS))[":root"][0]
OS_DARK = blocks(read(TOKENS))[':root:not([data-theme="light"])'][0]
ATTR_DARK = blocks(read(TOKENS))[':root[data-theme="dark"]'][0]

# Golden values: the validated hexes of the brief (runs P1-P8). A change here must come with a new validator run recorded in the
# brief's appendix (node <dataviz>/scripts/validate_palette.js) and a changed number in that table.
GOLDEN_LIGHT = {
    "--surface-0": "#f5efe7", "--surface-1": "#fffdf9", "--surface-2": "#efe6d8", "--ink-1": "#2b1c12", "--ink-2": "#6e5843", "--ink-3": "#8a7460",
    "--line": "#d8c9b4", "--line-strong": "#c4b096", "--accent": "#b7410e", "--accent-hover": "#8a2f0b", "--accent-ink": "#ffffff", "--accent-soft": "#f3d9cb",
    "--st-ok": "#2a9d67", "--st-ok-ink": "#1f7a50", "--st-slow": "#e3a924", "--st-slow-ink": "#8a6008", "--st-stop": "#8e1855", "--st-stop-ink": "#8e1855",
    "--st-info": "#2a75ba", "--st-info-ink": "#1d5f9a", "--st-park": "#aea298",
    "--prio-1": "#772e11", "--prio-2": "#983803", "--prio-3": "#b24b0a", "--prio-4": "#c7692c", "--prio-5": "#d78951",
    "--series-1": "#2a78d6", "--series-2": "#eb6834", "--series-3": "#1baf7a", "--series-4": "#eda100", "--series-5": "#e87ba4", "--series-6": "#008300",
    "--series-7": "#6250d6", "--series-other": "#8a7a68", "--series-unattr": "#b3a590",
}
GOLDEN_DARK = {
    "--surface-0": "#201612", "--surface-1": "#2b1c12", "--surface-2": "#382513", "--ink-1": "#f0e7d8", "--ink-2": "#cbb69c", "--ink-3": "#a58a6b",
    "--line": "#46311f", "--line-strong": "#5c4126", "--accent": "#d97742", "--accent-hover": "#e8905c", "--accent-ink": "#201612",
    "--st-ok": "#51bd85", "--st-ok-ink": "#51bd85", "--st-slow": "#f0b135", "--st-slow-ink": "#f0b135", "--st-stop": "#ca4b85", "--st-stop-ink": "#e27aa5",
    "--st-info": "#4b99d7", "--st-info-ink": "#64a6e0", "--st-park": "#6c6158",
    "--prio-1": "#f9c59c", "--prio-2": "#eea471", "--prio-3": "#de844f", "--prio-4": "#c96736", "--prio-5": "#ab502a",
    "--series-1": "#3987e5", "--series-2": "#d95926", "--series-3": "#199e70", "--series-4": "#c98500", "--series-5": "#d55181", "--series-6": "#008300",
    "--series-7": "#9085e9", "--series-other": "#a39482", "--series-unattr": "#7d6c58",
}


class TestTokenFile(unittest.TestCase):
    def test_dark_blocks_are_identical(self):
        self.assertEqual(OS_DARK, ATTR_DARK)                       # the toggle must win over an OS light setting, with the same values
        self.assertTrue(set(OS_DARK) <= set(LIGHT))                # every dark override has a light default
        self.assertEqual(OS_DARK["color-scheme"], "dark")
        self.assertEqual(LIGHT["color-scheme"], "light")

    def test_golden_state_series_and_priority_values(self):
        for name, want in GOLDEN_LIGHT.items():
            self.assertEqual(LIGHT[name], want, "light " + name)
        for name, want in GOLDEN_DARK.items():
            self.assertEqual(OS_DARK[name], want, "dark " + name)

    def test_the_accent_is_never_a_state(self):
        sem = blocks(read(TOKENS))[":root"][1]
        for name, val in sem.items():
            if name.startswith(("--sess-", "--ctx-", "--msg-")):
                self.assertNotIn("--accent", val, name)
        self.assertEqual(sem["--sess-working"], "var(--st-ok)")     # D5e (2): the main page's rust "working" chips are green
        self.assertEqual(sem["--ctx-hot"], "var(--st-stop)")         # D5e (3): no magenta overflow hue
        self.assertNotIn("--blk", read(TOKENS))

    def test_ink_tokens_are_readable_text(self):
        for name, theme in (("light", LIGHT), ("dark", {**LIGHT, **OS_DARK})):
            surface = theme["--surface-1"]
            for t in ("--ink-2", "--st-ok-ink", "--st-slow-ink", "--st-stop-ink", "--st-info-ink", "--accent"):
                self.assertGreaterEqual(contrast(theme[t], surface), 4.5, "%s %s on surface-1" % (name, t))
            self.assertGreaterEqual(contrast(theme["--accent"], theme["--surface-0"]), 4.5, name + " accent on surface-0")
            self.assertGreaterEqual(contrast(theme["--accent-ink"], theme["--accent"]), 4.5, name + " accent-ink on accent")

    def test_no_transitional_aliases_remain(self):
        tok = read(TOKENS)
        for old in ("--bg:", "--fg:", "--muted:", "--warn:", "--bad:", "--conf-", "--viz-", "--rec-bg:", "--done-bg:", "--slow:", "--stop:", "--park:"):
            self.assertNotIn(old, tok)                              # step 6 of the plan: removed once nothing used them


class TestPages(unittest.TestCase):
    def test_no_page_keeps_its_own_colour_block(self):
        for name, path in PAGES.items():
            for header, decls_list in blocks(style_text(read(path))).items():
                for decls in decls_list:
                    for k, v in decls.items():
                        self.assertNotRegex(v, r"#[0-9a-fA-F]{3,8}\b|rgba?\(", "%s %s { %s }" % (name, header, k))

    def test_no_raw_hex_or_rgba_in_page_css(self):
        for name, path in PAGES.items():
            raw = re.findall(r".{30}(?:#[0-9a-fA-F]{3,8}\b|rgba?\().{20}", style_text(read(path)))
            self.assertEqual(raw, [], name)

    def test_every_var_a_page_uses_is_defined(self):
        tok = set(re.findall(r"(--[a-z0-9-]+)\s*:", strip_comments(read(TOKENS))))
        for name, path in PAGES.items():
            src = read(path)
            own = set(re.findall(r"(--[a-z0-9-]+)\s*:", style_text(src))) | set(re.findall(r'setProperty\("(--[a-z0-9-]+)"', src)) | set(re.findall(r"[\"'](--[a-z0-9-]+):", src))
            used = set(re.findall(r"var\((--[a-z0-9-]+)\)", src))   # a var() with a fallback may stay undefined
            self.assertEqual(sorted(used - tok - own), [], name)

    def test_working_is_green_and_the_accent_is_not_in_a_state_chip(self):
        css = style_text(read(PAGES["index.html"]))
        self.assertIn(".state.st-working { --st: var(--sess-working); }", css)
        for line in css.split("\n"):
            if line.startswith(".state"):
                self.assertNotIn("var(--accent)", line)

    def test_both_pages_cut_context_at_60_85_100(self):
        idx, live = read(PAGES["index.html"]), read(PAGES["live.html"])
        self.assertIn("CTX_WARN = 60, CTX_HOT = 85", idx)
        self.assertIn('raw >= 0.85 ? "var(--ctx-hot)" : raw >= 0.6 ? "var(--ctx-warn)" : "var(--ctx-ok)"', live)
        self.assertNotIn("--blk", live)
        self.assertNotIn("magenta", live)


class TestServedTokens(ServerCase):
    def raw(self, path, method="GET", port=None):
        req = urllib.request.Request("http://127.0.0.1:%d%s" % (port or self.port, path), method=method)
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, r.headers, r.read()
        except urllib.error.HTTPError as e:
            return e.code, e.headers, e.read()

    def test_architect_mode_serves_tokens_css(self):
        status, headers, body = self.raw("/static/tokens.css")
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "text/css; charset=utf-8")
        self.assertEqual(body.decode(), read(TOKENS))

    def test_session_mode_serves_tokens_css(self):
        with tempfile.TemporaryDirectory() as tmp:
            srv = serve.DashboardServer(0, ds.Store(os.path.join(tmp, "s")), self.page, config=serve.session_config("t", "b/"), projects={})
            threading.Thread(target=srv.serve_forever, daemon=True).start()
            self.addCleanup(srv.server_close)
            self.addCleanup(srv.shutdown)
            status, headers, body = self.raw("/static/tokens.css", port=srv.server_address[1])
        self.assertEqual((status, headers["Content-Type"]), (200, "text/css; charset=utf-8"))
        self.assertIn("--st-ok:", body.decode())

    def test_other_names_404_and_only_get_and_head_are_allowed(self):
        for path in ("/static/index.html", "/static/live.html", "/static/", "/static", "/static/tokens.css/x", "/static/%2e%2e/index.html", "/static/..%2findex.html"):
            self.assertEqual(self.raw(path)[0], 404, path)
        self.assertEqual(self.raw("/static/tokens.css", "POST")[0], 405)
        self.assertEqual(self.raw("/static/tokens.css", "PUT")[0], 405)
        status, headers, body = self.raw("/static/tokens.css", "HEAD")
        self.assertEqual((status, body), (200, b""))

    def test_foreign_host_is_refused(self):
        req = urllib.request.Request("http://127.0.0.1:%d/static/tokens.css" % self.port, headers={"Host": "evil.example:7890"})
        with self.assertRaises(urllib.error.HTTPError) as cm:
            urllib.request.urlopen(req, timeout=5)
        self.assertEqual(cm.exception.code, 403)

    def test_served_page_links_tokens_first_with_the_rev(self):
        status, headers, body = self.raw("/")
        text = body.decode()
        link = '<link rel="stylesheet" href="/static/tokens.css?v=%s">' % headers["X-Page-Rev"]
        self.assertIn(link, text)
        self.assertLess(text.index(link), text.index("window.claude = { use:"))     # before the shim and the page script
        self.assertLess(text.index(link), text.index("/*PAGE*/"))

    def test_rev_changes_when_only_tokens_css_changes(self):
        with tempfile.TemporaryDirectory() as tmp:
            static = os.path.join(tmp, "static")
            os.makedirs(static)
            shutil.copy(TOKENS, static)
            srv = serve.DashboardServer(0, self.store, self.page, static=static)
            self.addCleanup(srv.server_close)
            a = srv.rev()
            self.assertEqual(srv.rev(), a)
            with open(os.path.join(static, "tokens.css"), "a") as f:
                f.write("/* edited */")
            os.utime(os.path.join(static, "tokens.css"), (1, 1e9 + 7))
            self.assertNotEqual(srv.rev(), a)                                         # open tabs reload on a token-only change
            self.assertEqual(serve.page_rev(self.page), serve.page_rev(self.page))     # page_rev alone still hashes just the page

    def test_live_page_links_tokens_and_has_no_shim(self):
        live = os.path.join(self._tmp.name, "live.html")
        with open(live, "w") as f:
            f.write("<!doctype html><html><head><title>l</title></head><body>LIVE</body></html>")
        srv = serve.DashboardServer(0, self.store, self.page, live_page=live)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        status, headers, body = self.raw("/live", port=srv.server_address[1])
        text = body.decode()
        self.assertEqual(status, 200)
        self.assertIn('<head>\n<link rel="stylesheet" href="/static/tokens.css?v=', text)
        self.assertNotIn("window.claude", text)
        self.assertEqual(self.raw("/live", "POST", srv.server_address[1])[0], 405)


if __name__ == "__main__":
    unittest.main()
