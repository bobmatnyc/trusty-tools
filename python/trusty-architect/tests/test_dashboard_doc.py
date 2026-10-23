"""dashboard-serve.py GET /doc/<project>/<rel> (brief reader), GET /vendor/<name>, load_projects, and the briefs
collection through the store.

Why: the reader serves files from other projects' trees to a browser, so every escape must be a 404.
What: allowed paths (briefs and any nested .md under the root), deny list (dot segments, node_modules, target,
secret/credential/.env names), dot-dot (plain and encoded), symlink escape (file and directory), unknown project, non-.md,
absolute path, writes, vendor name list, project allowlist parsing, briefs round trip.
"""
import json
import os
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import dashboard_store as ds  # noqa: E402
from test_dashboard_store import serve  # noqa: E402


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


class TestDocRoute(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        t = os.path.realpath(tmp.name)
        self.t = t
        proj, arch = os.path.join(t, "acme"), os.path.join(t, "arch")
        write(os.path.join(proj, "docs", "briefs", "2026-10-11-a.md"), "# brief a\n")
        write(os.path.join(proj, "docs", "briefs", "sub", "b.md"), "# nested\n")
        write(os.path.join(proj, "docs", "briefs", "notes.txt"), "plain")
        write(os.path.join(proj, "docs", "other.md"), "OTHER DOC")
        write(os.path.join(proj, "README.md"), "ROOT README")
        write(os.path.join(proj, "projects", "hs", "ds", "CHRIS.md"), "# nested doc\n")
        for denied in (".git/notes.md", ".claude/x.md", ".trusty-mpm/y.md", "docs/.hidden/z.md", "docs/.dot.md",
                       "node_modules/pkg/README.md", "target/doc/a.md", "docs/my-secret-plan.md", "docs/Credentials.md",
                       "docs/prod.env.md"):
            write(os.path.join(proj, *denied.split("/")), "DENIED")
        os.symlink(os.path.join(proj, ".git", "notes.md"), os.path.join(proj, "docs", "innocent.md"))
        write(os.path.join(t, "secret.md"), "SECRET")
        write(os.path.join(t, "outside", "docs", "briefs", "o.md"), "OUTSIDE")
        os.symlink(os.path.join(t, "secret.md"), os.path.join(proj, "docs", "briefs", "link.md"))
        os.symlink(os.path.join(proj, "docs", "briefs", "notes.txt"), os.path.join(proj, "docs", "briefs", "txtlink.md"))
        os.symlink(t, os.path.join(proj, "docs", "briefs", "linkdir"))
        write(os.path.join(arch, "docs", "briefs", "x.md"), "# arch brief\n")
        write(os.path.join(arch, "local", "briefs", "old.md"), "# old brief\n")
        write(os.path.join(arch, "local", "other", "y.md"), "ARCH OTHER")
        # a project whose docs/briefs is itself a symlink out of the project
        esc = os.path.join(t, "esc")
        os.makedirs(os.path.join(esc, "docs"))
        os.symlink(os.path.join(t, "outside", "docs", "briefs"), os.path.join(esc, "docs", "briefs"))
        vendor = os.path.join(t, "vendor")
        write(os.path.join(vendor, "marked.umd.js"), "/*marked*/")
        write(os.path.join(vendor, "extra.js"), "/*not listed*/")
        page = os.path.join(t, "index.html")
        write(page, "<html><head></head><body></body></html>")
        projects = {"acme": proj, "architect": arch, "esc": esc}
        self.srv = serve.DashboardServer(0, ds.Store(os.path.join(t, "oq")), page, os.path.join(arch, "local", "briefs"),
                                         projects=projects, vendor=vendor)
        self.port = self.srv.server_address[1]
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.addCleanup(self.srv.server_close)
        self.addCleanup(self.srv.shutdown)

    def get(self, path, method="GET", host=None):
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}", method=method)
        if host:
            req.add_header("Host", host)
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, r.headers, r.read()
        except urllib.error.HTTPError as e:
            return e.code, e.headers, e.read()

    def test_allowed_briefs_served_read_only_as_plain_text(self):
        st, h, body = self.get("/doc/acme/docs/briefs/2026-10-11-a.md")
        self.assertEqual((st, h["Content-Type"], body), (200, "text/plain; charset=utf-8", b"# brief a\n"))
        self.assertEqual(h["X-Content-Type-Options"], "nosniff")
        self.assertEqual(self.get("/doc/acme/docs/briefs/sub/b.md")[2], b"# nested\n")
        self.assertEqual(self.get("/doc/architect/docs/briefs/x.md")[0], 200)
        self.assertEqual(self.get("/doc/architect/local/briefs/old.md")[2], b"# old brief\n")
        self.assertEqual(self.get("/doc/acme/docs/briefs/2026-10-11-a.md", "HEAD")[0], 200)

    def test_briefs_list_reachable_only_sorted(self):
        write(os.path.join(self.t, "acme", "docs", "briefs", "2026-10-20-newer.md"), "---\nx: 1\n---\n# Newer brief\n\nsome words here\n")
        st, h, body = self.get("/briefs")
        self.assertEqual((st, h["Content-Type"]), (200, "application/json; charset=utf-8"))
        rows = json.loads(body)["briefs"]
        keys = {(r["project"], r["path"]) for r in rows}
        self.assertIn(("acme", "docs/briefs/2026-10-20-newer.md"), keys)
        self.assertIn(("acme", "docs/briefs/sub/b.md"), keys)
        self.assertIn(("architect", "local/briefs/old.md"), keys)
        self.assertNotIn(("acme", "docs/other.md"), keys)             # outside docs/briefs
        for p in ("docs/briefs/link.md", "docs/briefs/txtlink.md", "docs/briefs/linkdir/secret.md"):
            self.assertNotIn(("acme", p), keys)                         # symlink escapes
        self.assertFalse([r for r in rows if "secret" in r["path"].lower() or "/." in r["path"]])
        newer = next(r for r in rows if r["path"].endswith("newer.md"))
        self.assertEqual((newer["title"], newer["date"], newer["words"]), ("Newer brief", "2026-10-20", 6))
        self.assertEqual(next(r for r in rows if r["path"].endswith("sub/b.md"))["title"], "nested")
        dated = [r["date"] for r in rows]
        self.assertEqual(dated, sorted(dated, reverse=True))
        self.assertEqual(self.get("/briefs", "HEAD")[0], 200)
        self.assertEqual(self.get("/briefs/x")[0], 404)

    def test_dot_dot_encoded_and_absolute_are_404(self):
        for p in ("/doc/acme/docs/briefs/../../../secret.md", "/doc/acme/docs/briefs/%2e%2e/%2e%2e/%2e%2e/secret.md",
                  "/doc/acme/docs/briefs/..%2f..%2f..%2fsecret.md", "/doc/acme/docs/briefs/sub/../../other.md",
                  "/doc/acme/docs/briefs/%2e%2e%2f%2e%2e%2fsecret.md", "/doc/acme//etc/passwd.md",
                  "/doc/acme/docs/briefs//etc/hosts.md", "/doc/acme/%2Fetc%2Fpasswd.md", "/doc//etc/passwd.md",
                  "/doc/acme/docs/briefs/a.md%00.png", "/doc/acme/docs/briefs/2026-10-11-a.md%00",
                  "/doc/acme/docs/briefs/..\\..\\secret.md", "/doc/%2e%2e/acme/docs/briefs/2026-10-11-a.md"):
            self.assertEqual(self.get(p)[0], 404, p)

    def test_symlink_escapes_are_404(self):
        for p in ("/doc/acme/docs/briefs/link.md", "/doc/acme/docs/briefs/linkdir/secret.md",
                  "/doc/acme/docs/briefs/txtlink.md", "/doc/esc/docs/briefs/o.md"):
            self.assertEqual(self.get(p)[0], 404, p)

    def test_unknown_project_non_md_and_wrong_tree_are_404(self):
        for p in ("/doc/nope/docs/briefs/2026-10-11-a.md", "/doc/acme/docs/briefs/notes.txt",
                  "/doc/acme/docs", "/doc/acme/docs/briefs/",
                  "/doc/acme/docs/briefs/sub", "/doc/acme/docs/briefs/missing.md", "/doc/acme/local/briefs/old.md", "/doc/acme/projects/hs/ds/CHRIS.txt",
                  "/doc/", "/doc", "/doc/acme", "/doc/acme/docs/briefs/.md.bak"):
            self.assertEqual(self.get(p)[0], 404, p)

    def test_any_md_under_the_root_is_served(self):
        for p, want in (("/doc/acme/projects/hs/ds/CHRIS.md", b"# nested doc\n"), ("/doc/acme/README.md", b"ROOT README"),
                        ("/doc/acme/docs/other.md", b"OTHER DOC"), ("/doc/architect/local/other/y.md", b"ARCH OTHER"),
                        ("/doc/acme/projects/hs/ds/CHRIS.md?x=1", b"# nested doc\n")):
            self.assertEqual(self.get(p)[::2], (200, want), p)

    def test_deny_list_is_404(self):
        for p in ("/doc/acme/.git/notes.md", "/doc/acme/.claude/x.md", "/doc/acme/.trusty-mpm/y.md", "/doc/acme/docs/.hidden/z.md",
                  "/doc/acme/docs/.dot.md", "/doc/acme/node_modules/pkg/README.md", "/doc/acme/target/doc/a.md",
                  "/doc/acme/docs/my-secret-plan.md", "/doc/acme/docs/Credentials.md", "/doc/acme/docs/prod.env.md",
                  "/doc/acme/docs/%2Egit/x.md", "/doc/acme/docs/innocent.md"):
            self.assertEqual(self.get(p)[0], 404, p)

    def test_writes_and_foreign_host_refused(self):
        for m in ("POST", "PUT", "DELETE"):
            self.assertEqual(self.get("/doc/acme/docs/briefs/2026-10-11-a.md", m)[0], 405)
        self.assertEqual(self.get("/doc/acme/docs/briefs/2026-10-11-a.md", host="evil.example")[0], 403)
        self.assertEqual(self.get("/vendor/marked.umd.js", host="evil.example")[0], 403)

    def test_vendor_serves_only_the_fixed_names(self):
        st, h, body = self.get("/vendor/marked.umd.js")
        self.assertEqual((st, h["Content-Type"], body), (200, "application/javascript; charset=utf-8", b"/*marked*/"))
        for p in ("/vendor/extra.js", "/vendor/purify.min.js", "/vendor/../index.html", "/vendor/%2e%2e/index.html", "/vendor/", "/vendor"):
            self.assertEqual(self.get(p)[0], 404, p)


class TestProjectAllowlist(unittest.TestCase):
    def test_load_projects_reads_root_lines_and_roots_json(self):
        with tempfile.TemporaryDirectory() as t:
            t = os.path.realpath(t)
            for name in ("alpha", "beta", "gamma", "delta", "noroot"):
                os.makedirs(os.path.join(t, "r", name))
            write(os.path.join(t, "local", "projects", "alpha.md"), f"# alpha\n\n- Root `{t}/r/alpha`.\n")
            write(os.path.join(t, "local", "projects", "beta.md"), f"- Root: `{t}/r/beta`\n- Root AGENTS: not a root line\n")
            write(os.path.join(t, "local", "projects", "gone.md"), f"- Root `{t}/r/missing`.\n")
            write(os.path.join(t, "local", "projects", "noroot.md"), "no root here\n")
            write(os.path.join(t, "local", "projects", "roots.json"), json.dumps({"gamma": f"{t}/r/gamma", "a/b": f"{t}/r/delta"}))
            got = serve.load_projects(t)
            self.assertEqual(got, {"architect": t, "alpha": f"{t}/r/alpha", "beta": f"{t}/r/beta", "gamma": f"{t}/r/gamma"})


class TestBriefsCollection(unittest.TestCase):
    def test_briefs_round_trip_in_the_store(self):
        with tempfile.TemporaryDirectory() as t:
            store = ds.Store(os.path.join(t, "oq"))
            body = {"question": "How does X work?", "answer": "One paragraph.", "project": "architect", "status": "open",
                    "doc": "docs/briefs/2026-10-11-x.md", "created": "2026-10-11T06:30:00Z", "updated": "2026-10-11T06:30:00Z"}
            store.set("briefs", "2026-10-11-x", body, None, actor="test")
            self.assertEqual(store.get("briefs", "2026-10-11-x")["data"]["doc"], "docs/briefs/2026-10-11-x.md")
            self.assertEqual([d["id"] for d in store.list_docs("briefs")], ["2026-10-11-x"])
            self.assertTrue(os.path.isfile(os.path.join(t, "oq", "briefs", "2026-10-11-x.json")))


if __name__ == "__main__":
    unittest.main()
