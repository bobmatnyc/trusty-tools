"""Open decisions render oldest first by default, or newest first / project A-Z from the Sort select (#11047); run under
node, skipped when node is absent."""
import json
import os
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import PAGES


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardDecisionOrder(unittest.TestCase):
    def test_open_decisions_sorted_oldest_first_and_wired_to_decisions_only(self):
        body = """
        const txt = (v) => (v == null ? "" : String(v));
        const byRank = (a, b) => txt(a.rank).localeCompare(txt(b.rank));
        const items = [
          { id: "D3", created: "2026-10-09T01:00:00Z", rank: "1" },
          { id: "D1", created: "2026-10-07T01:00:00Z", rank: "9" },
          { id: "D2b", created: "2026-10-08T01:00:00Z", rank: "2" },
          { id: "D2a", created: "2026-10-08T01:00:00Z", rank: "1" },
        ];
        console.log(JSON.stringify(items.sort(oldestFirst).map((i) => i.id)));"""
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                src = open(page, encoding="utf-8").read()
                part = re.search(r"/\* BEGIN decisionOrder \*/(.*?)/\* END decisionOrder \*/", src, re.S).group(1)
                r = subprocess.run(["node", "-e", body.split("const items")[0] + part + "\nconst items" + body.split("const items")[1]],
                                   capture_output=True, text=True, timeout=30)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(json.loads(r.stdout), ["D1", "D2a", "D2b", "D3"])
                self.assertIn("const q = decisionList(P.q, decisionSort)", src)   # #11047: the Sort select; oldest first by default
                self.assertIn('let decisionSort = "oldest"', src)
                self.assertIn("const a = P.a.sort(sortKey)", src)

    def run_block(self, src, body):
        part = re.search(r"/\* BEGIN decisionOrder \*/(.*?)/\* END decisionOrder \*/", src, re.S).group(1)
        pre = """
        const txt = (v) => (v == null ? "" : String(v));
        const byRank = (a, b) => txt(a.rank).localeCompare(txt(b.rank));
        """
        r = subprocess.run(["node", "-e", pre + part + "\n" + body], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        return json.loads(r.stdout)

    def test_three_sort_orders_with_no_project_last(self):
        """#11047 (D80): oldest first (today's order), newest first, and project A-Z with "No project" last."""
        body = """
        const mk = () => [
          { id: "D4", created: "2026-10-10T01:00:00Z", project: "beta" },
          { id: "D1", created: "2026-10-07T01:00:00Z" },
          { id: "D3", created: "2026-10-09T01:00:00Z", project: "Alpha" },
          { id: "D2", created: "2026-10-08T01:00:00Z", project: "beta" },
          { id: "D5", created: "2026-10-11T01:00:00Z", project: "" },
          { id: "D0", created: "2026-10-06T01:00:00Z", project: "alpha" },
        ];
        const ids = (l) => l.map((i) => i.id);
        console.log(JSON.stringify({
          oldest: ids(decisionList(mk(), "oldest")),
          unknown: ids(decisionList(mk(), "bogus")),
          newest: ids(decisionList(mk(), "newest")),
          project: ids(decisionList(mk(), "project")),
          groups: decisionGroups(decisionList(mk(), "oldest")).map((g) => [g.project, ids(g.items)]),
          sorts: DECISION_SORTS.map((s) => s[0]),
        }));"""
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                got = self.run_block(open(page, encoding="utf-8").read(), body)
                self.assertEqual(got["oldest"], ["D0", "D1", "D2", "D3", "D4", "D5"])
                self.assertEqual(got["unknown"], got["oldest"])
                self.assertEqual(got["newest"], ["D5", "D4", "D3", "D2", "D1", "D0"])
                # A-Z ignoring case; "Alpha" and "alpha" stay separate groups (code-point tie-break); each group oldest
                # first; the "No project" group (absent or blank) last
                self.assertEqual(got["groups"], [["Alpha", ["D3"]], ["alpha", ["D0"]], ["beta", ["D2", "D4"]], ["", ["D1", "D5"]]])
                self.assertEqual(got["project"], ["D3", "D0", "D2", "D4", "D1", "D5"])
                self.assertEqual(got["sorts"], ["oldest", "newest", "project"])

    def test_project_mode_headers_count_what_the_filter_leaves(self):
        """Project mode: a header (name + count) before each group, "No project" last; the other modes are flat. dgSync,
        run after the pane filter hides rows, recounts each header and hides one whose rows are all filtered out."""
        fake_dom = """
        function el(tag, cls, text) {
          const n = { tag, children: [], dataset: {}, attrs: {}, textContent: text == null ? "" : String(text),
            classList: { s: new Set((cls || "").split(" ").filter(Boolean)),
              contains(c) { return this.s.has(c); }, toggle(c, on) { if (on) this.s.add(c); else this.s.delete(c); } },
            append(...k) { this.children.push(...k); }, setAttribute(k, v) { this.attrs[k] = v; },
            matches(sel) { return sel === "[data-pid]" && "pid" in this.dataset; },
            querySelector(sel) { return this.children.find((c) => c.classList.contains(sel.slice(1))) || null; } };
          return n;
        }
        const row = (it) => { const r = el("details", "item"); r.dataset.pid = it.id; return r; };
        const view = (nodes) => nodes.map((n) => n.classList.contains("dg-head")
          ? ["H", n.children[0].textContent, n.querySelector(".n").textContent, n.classList.contains("nv-off")] : n.dataset.pid);
        """
        body = """
        const list = decisionList([
          { id: "D3", created: "3", project: "web" }, { id: "D1", created: "1" },
          { id: "D2", created: "2", project: "api" }, { id: "D4", created: "4", project: "web" },
        ], "project");
        const nodes = decisionNodes(list, "project", row);
        const before = view(nodes);
        nodes.find((n) => n.dataset.pid === "D2").classList.toggle("nv-off", true);   /* the pane filter hides api's one row */
        nodes.find((n) => n.dataset.pid === "D4").classList.toggle("nv-off", true);
        dgSync({ children: nodes });
        console.log(JSON.stringify({ before, after: view(nodes), flat: view(decisionNodes(list, "newest", row)) }));"""
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                src = open(page, encoding="utf-8").read()
                dom = re.search(r"/\* BEGIN decisionGroupsDom \*/(.*?)/\* END decisionGroupsDom \*/", src, re.S).group(1)
                got = self.run_block(src, fake_dom + dom + body)
                self.assertEqual(got["before"], [["H", "api", "1", False], "D2", ["H", "web", "2", False], "D3", "D4",
                                                 ["H", "No project", "1", False], "D1"])
                self.assertEqual(got["after"], [["H", "api", "0", True], "D2", ["H", "web", "1", False], "D3", "D4",
                                                ["H", "No project", "1", False], "D1"])
                self.assertEqual(got["flat"], ["D2", "D3", "D4", "D1"])   # no headers outside Project mode

    def test_parked_groups_keep_wake_order_and_sort_is_wired(self):
        """Project mode groups the Parked decisions too, keeping the parked.until order inside each group."""
        body = """
        const parked = [{ id: "D9", project: "b" }, { id: "D7" }, { id: "D8", project: "a" }, { id: "D6", project: "b" }];
        console.log(JSON.stringify(decisionGroups(parked).map((g) => [g.project, g.items.map((i) => i.id)])));"""
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                src = open(page, encoding="utf-8").read()
                self.assertEqual(self.run_block(src, body), [["a", ["D8"]], ["b", ["D9", "D6"]], ["", ["D7"]]])
                self.assertIn('const grouped = k[0] === "D" && decisionSort === "project"', src)
                self.assertIn('fillList($("list-q"), decisionNodes(q, decisionSort, renderQuestion)', src)
                self.assertIn('const DSORT_KEY = OPEN_KEY + "-decision-sort"', src)
                self.assertIn("dgSync(box);", src)
                self.assertIn(".cbar input[type=search], .pane-filter select.pf-sort, .attach", src)   # 44 px / 16 px on phones

    def test_page_clears_window_opener_on_load(self):
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                src = open(page, encoding="utf-8").read()
                part = re.search(r"/\* BEGIN clearOpener \*/(.*?)/\* END clearOpener \*/", src, re.S).group(1)
                body = "const window = { opener: { name: 'console' } };\n" + part + "\nconsole.log(JSON.stringify(window.opener));"
                r = subprocess.run(["node", "-e", body], capture_output=True, text=True, timeout=30)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(json.loads(r.stdout), None)
                # A throwing setter must not break page load.
                body = "const window = { get opener() { return 1; }, set opener(v) { throw new Error('ro'); } };\n" + part + "\nconsole.log('ok');"
                r = subprocess.run(["node", "-e", body], capture_output=True, text=True, timeout=30)
                self.assertEqual(r.returncode, 0, r.stderr)


if __name__ == "__main__":
    unittest.main()
