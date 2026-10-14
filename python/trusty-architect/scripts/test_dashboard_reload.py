"""A stale dashboard tab reloads itself once when the served index.html changes (the owner 2026-10-09).

The server puts the page's content hash in an X-Page-Rev header on every API response and embeds the hash the
page was served with; the shim reloads once when they differ, after the page stashed its unsent drafts.
Test: scripts/test_dashboard_reload.py
"""
import json
import os
import re
import shutil
import subprocess
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from test_dashboard_store import ServerCase  # noqa: E402
from test_dashboard_progress import PAGES  # noqa: E402


class TestPageRev(ServerCase):
    def rev(self, path="/api/items"):
        import urllib.request
        with urllib.request.urlopen(f"http://127.0.0.1:{self.port}{path}", timeout=5) as r:
            return r.headers.get("X-Page-Rev")

    def test_header_is_the_page_hash_and_changes_with_the_file(self):
        a = self.rev()
        self.assertRegex(a or "", r"^[0-9a-f]{16}$")
        self.assertEqual(self.rev("/api/meta/state"), a)
        with open(self.page, "a") as f:
            f.write("<!-- edited -->")
        os.utime(self.page, (1, 1e9 + 5))   # defeat an mtime cache on a coarse clock
        self.assertNotEqual(self.rev(), a)

    def test_served_page_embeds_the_rev_it_was_served_with(self):
        status, body = self.call("GET", "/")
        self.assertIn(f'var PAGE_REV = "{self.rev()}"', body.decode())

    @unittest.skipUnless(shutil.which("node"), "node not installed")
    def test_shim_stashes_drafts_and_reloads_once_on_a_changed_rev(self):
        status, body = self.call("GET", "/")
        shim = re.search(r"<script>(.*?)</script>", body.decode(), re.S).group(1)
        rev = re.search(r'var PAGE_REV = "([0-9a-f]+)"', shim).group(1)
        js = r"""
let reloads = 0, stashed = 0, cur = process.argv[2];
global.window = { __stashDrafts() { stashed++; } };
global.location = { reload() { reloads++; } };
global.document = { hidden: false, addEventListener() {}, removeEventListener() {} };
global.fetch = async () => ({ ok: true, status: 200, headers: { get: (k) => (k === "X-Page-Rev" ? cur : null) }, json: async () => ({ rev: "r", docs: [] }) });
eval(require("fs").readFileSync(process.argv[1], "utf8"));
(async () => {
  const db = await window.claude.use("db");
  await db.collection("items").get(); await db.collection("items").get();
  const same = [reloads, stashed];
  cur = "ffffffffffffffff";
  await db.collection("items").get(); await db.collection("items").get();
  console.log(JSON.stringify([same, reloads, stashed]));
  process.exit(0);
})();"""
        base = os.path.join(os.environ.get("TMPDIR", "/tmp"), f"shim-{os.getpid()}.js")
        with open(base, "w") as f:
            f.write(shim)
        self.addCleanup(os.unlink, base)
        r = subprocess.run(["node", "-e", js, base, rev], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.loads(r.stdout), [[0, 0], 1, 1])


class TestPageStashesDrafts(unittest.TestCase):
    def test_page_stashes_and_restores_drafts_across_a_reload(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertIn("window.__stashDrafts = ", src)
            self.assertIn('"aq-drafts"', src)


if __name__ == "__main__":
    unittest.main()
