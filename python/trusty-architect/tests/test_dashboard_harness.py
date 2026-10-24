"""Live view collector: dashboard_harness.build and GET /api/harness, GET /live (IL ar-Te8).

Covers: state mapping (ok/slow/stop/park incl. blocks_session item and blocker message), message window, one-line truncation
and secret redaction, in/out directions, missing-source reporting, subagent discovery (live, ended, stale, token dedup, session join),
cache TTL, the endpoint (shape, GET only, Host check, session-mode 404, no write) and the /live page wiring.
Run: python3 -m unittest scripts/test_dashboard_harness.py
Test: scripts/test_dashboard_harness.py
"""
import json
import os
import sys
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "scripts"))
import dashboard_harness as dh  # noqa: E402
import dashboard_store as ds  # noqa: E402
from test_dashboard_store import serve  # noqa: E402

NOW = 1792576800   # 2026-10-21T10:00:00Z


def iso(offset):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(NOW + offset))


def jl(path, rows):
    with open(path, "w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")


class Fx(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        t = self._tmp.name
        self.inbox, self.proj = os.path.join(t, "inbox"), os.path.join(t, "projects")
        os.makedirs(self.inbox)
        os.makedirs(self.proj)
        self.cwd = "/work/alpha.repo"
        self.enc = dh.encode_cwd(self.cwd)
        json.dump({"tm-a": {"state": "working", "idle_min": 0}, "tm-b": {"state": "idle", "idle_min": 3}, "tm-c": {"state": "idle", "idle_min": 90},
                   "tm-d": {"state": "working", "idle_min": 0}}, open(os.path.join(self.inbox, "dashboard-live.json"), "w"))
        json.dump({"sessions": {"tm-a": {"ctx_k": 120.5, "out_tokens_1h": 90000}, "tm-d": {"ctx_k": 10, "out_tokens_1h": 100}}},
                  open(os.path.join(self.inbox, "dashboard-feed.json"), "w"))
        jl(os.path.join(self.inbox, "events.jsonl"), [{"tmux_session": "tm-a", "cwd": self.cwd}])
        dh._doc_state.update({"off": {}, "changes": [], "sends": []})
        self.sdocs = [{"name": "tm-a", "goal": "G01 · Ship it", "summary": "doing the thing\nacross lines"}]

    def add_agent(self, aid, age=10, end=False, tokens=(100, 50), desc="Fix the thing", finished=False, sess="s1"):
        d = os.path.join(self.proj, self.enc, sess, "subagents")
        os.makedirs(d, exist_ok=True)
        meta = {"agentType": "rust-engineer", "description": desc, "spawnedWithWorktree": True, "worktreeBranch": "worktree-agent-" + aid}
        if finished:
            meta["finished"] = True
        json.dump(meta, open(os.path.join(d, f"agent-{aid}.meta.json"), "w"))
        rows = [{"type": "user", "timestamp": iso(-300), "message": {"role": "user", "content": "SECRET PROMPT TEXT"}}]
        for i, n in enumerate(tokens):
            m = {"role": "assistant", "usage": {"output_tokens": n}, "content": [{"type": "tool_use", "name": "Edit", "input": {"file_path": "/x/y/lib.rs", "old_string": "SECRET"}}]}
            if end and i == len(tokens) - 1:
                m["stop_reason"] = "end_turn"
            rows.append({"type": "assistant", "requestId": f"r{i}", "uuid": f"u{i}", "timestamp": iso(-200 + i), "message": m})
            rows.append({"type": "assistant", "requestId": f"r{i}", "uuid": f"u{i}b", "timestamp": iso(-200 + i), "message": m})   # streamed duplicate
        p = os.path.join(d, f"agent-{aid}.jsonl")
        jl(p, rows)
        os.utime(p, (NOW - age, NOW - age))

    def add_main(self, calls, name="sess1.jsonl", enc=None, age=5):
        """Main-session transcript with one assistant row per (offset, tool, input) call."""
        d = os.path.join(self.proj, enc or self.enc)
        os.makedirs(d, exist_ok=True)
        p = os.path.join(d, name)
        jl(p, [{"type": "assistant", "timestamp": iso(off), "message": {"role": "assistant", "content": [{"type": "tool_use", "name": tool, "input": inp}]}}
               for off, tool, inp in calls])
        os.utime(p, (NOW - age, NOW - age))

    def snap(self, items=(), now=NOW):
        dh._agent_cache.clear()
        return dh.build(self.inbox, self.proj, self.sdocs, list(items), now=now)

    def by(self, snap, name):
        return next(s for s in snap["sessions"] if s["name"] == name)


class TestStates(Fx):
    def test_mapping_and_goal_ctx(self):
        s = self.snap()
        self.assertEqual({x["name"]: x["state"] for x in s["sessions"]}, {"tm-a": "ok", "tm-b": "slow", "tm-c": "park", "tm-d": "slow"})
        a = self.by(s, "tm-a")
        self.assertEqual((a["goal"], a["goal_title"], a["ctx_k"], a["idle_min"]), ("G01", "Ship it", 120.5, 0))
        self.assertNotIn("\n", a["summary"])

    def test_blocking_item_and_blocker_message_mean_stop(self):
        s = self.snap([{"status": "open", "blocks_session": True, "session": "tm-b"}, {"status": "done", "blocks_session": True, "session": "tm-c"}])
        self.assertEqual(self.by(s, "tm-b")["state"], "stop")
        self.assertEqual(self.by(s, "tm-c")["state"], "park")      # a done item blocks nothing
        jl(os.path.join(self.inbox, "pm-messages.jsonl"), [{"ts": iso(-60), "session": "tm-a", "kind": "blocker", "text": "CI red"}])
        self.assertEqual(self.by(self.snap(), "tm-a")["state"], "stop")


class TestMessages(Fx):
    def test_window_direction_truncation_redaction(self):
        jl(os.path.join(self.inbox, "pm-messages.jsonl"), [
            {"ts": iso(-100), "session": "tm-a", "kind": "decision", "text": "x" * 500 + " token=abc123def"},
            {"ts": iso(-100), "session": "tm-a", "kind": "done", "text": "api_key: sk-live-999 rotated"},
            {"ts": iso(-4000), "session": "tm-a", "kind": "done", "text": "too old"}])
        jl(os.path.join(self.inbox, "relays.jsonl"), [{"ts": iso(-50), "session": "tm-b", "kind": "relay", "text": "line one\nline two"}])
        s = self.snap()
        m = s["messages"]
        self.assertEqual([(x["dir"], x["session"]) for x in m], [("in", "tm-a"), ("in", "tm-a"), ("out", "tm-b")])
        self.assertTrue(all(len(x["text"]) <= dh.TEXT_MAX and "\n" not in x["text"] for x in m))
        self.assertNotIn("abc123def", json.dumps(s))
        self.assertNotIn("sk-live-999", json.dumps(s))
        self.assertEqual(m[2]["text"], "line one line two")

    def test_missing_sources_reported_not_fatal(self):
        os.remove(os.path.join(self.inbox, "dashboard-live.json"))
        s = self.snap()
        self.assertEqual(s["sources"]["live"], "missing")
        self.assertEqual(s["sources"]["relays"], "missing")
        self.assertEqual(s["sources"]["messages"], "missing")
        self.assertEqual(sorted(x["name"] for x in s["sessions"]), ["tm-a", "tm-d"])   # falls back to the feed's session names


class TestAgents(Fx):
    def test_live_agent_joined_to_session_without_content(self):
        self.add_agent("abcd1234ef", tokens=(100, 50))
        s = self.snap()
        ag = self.by(s, "tm-a")["agents"]
        self.assertEqual(len(ag), 1)
        a = ag[0]
        self.assertEqual((a["id"], a["type"], a["task"], a["state"], a["tokens"], a["worktree"]), ("abcd1234", "rust-engineer", "Fix the thing", "ok", 150, True))
        self.assertEqual(a["elapsed_s"], 300)
        self.assertEqual(a["act"], "Edit lib.rs")
        self.assertNotIn("SECRET", json.dumps(s))

    def test_ended_finished_and_stale_agents_are_not_listed(self):
        self.add_agent("a1", end=True)
        self.add_agent("a2", finished=True)
        self.add_agent("a3", age=20 * 60)
        self.add_agent("a4", age=200)
        ag = self.by(self.snap(), "tm-a")["agents"]
        self.assertEqual([(x["id"], x["state"]) for x in ag], [("a4", "slow")])

    def test_incremental_read_adds_only_new_tokens(self):
        self.add_agent("a5", tokens=(100,))
        dh._agent_cache.clear()
        first = dh.build(self.inbox, self.proj, self.sdocs, [], now=NOW)
        p = os.path.join(self.proj, self.enc, "s1", "subagents", "agent-a5.jsonl")
        with open(p, "a") as f:
            f.write(json.dumps({"type": "assistant", "requestId": "rN", "uuid": "uN", "timestamp": iso(-1), "message": {"role": "assistant", "usage": {"output_tokens": 7}, "content": []}}) + "\n")
        os.utime(p, (NOW - 5, NOW - 5))
        second = dh.build(self.inbox, self.proj, self.sdocs, [], now=NOW)
        self.assertEqual(self.by(first, "tm-a")["agents"][0]["tokens"], 100)
        self.assertEqual(self.by(second, "tm-a")["agents"][0]["tokens"], 107)

    def test_worktree_cwd_folds_into_repo_dir(self):
        self.assertEqual(dh.fold_worktree(dh.encode_cwd("/work/alpha.repo/.claude/worktrees/wt")), self.enc)


class TestDocs(Fx):
    def test_edit_write_listed_newest_first_with_diff_and_session(self):
        self.add_main([(-300, "Edit", {"file_path": "/work/alpha.repo/docs/a.md", "old_string": "one\ntwo", "new_string": "one\nthree"}),
                       (-100, "Write", {"file_path": "/work/alpha.repo/docs/b.md", "content": "hello\nworld"}),
                       (-50, "Edit", {"file_path": "/work/alpha.repo/docs/a.md", "old_string": "three", "new_string": "four"}),
                       (-40, "Write", {"file_path": "/work/alpha.repo/src/x.rs", "content": "fn main(){}"})])
        s = self.snap_with_roots()
        self.assertEqual([d["rel"] for d in s["docs"]], ["docs/a.md", "docs/b.md"])      # .rs skipped, newest first, one row per file
        a = s["docs"][0]
        self.assertEqual((a["project"], a["session"], a["n"], a["view"]), ("alpha", "tm-a", 2, {"project": "alpha", "rel": "docs/a.md"}))
        d = dh.doc_diff(a["id"])
        self.assertIn("-three", d["diff"])
        self.assertIn("+four", d["diff"])
        w = dh.doc_diff(s["docs"][1]["id"])
        self.assertTrue(w["new_file"])
        self.assertIn("+hello", w["diff"])
        self.assertIsNone(dh.doc_diff("nope"))

    def test_secret_files_and_values_never_served_and_old_changes_dropped(self):
        self.add_main([(-100, "Write", {"file_path": "/work/alpha.repo/docs/secrets.md", "content": "x"}),
                       (-90, "Write", {"file_path": "/work/alpha.repo/docs/c.md", "content": "token=abc123 and " + "ghp_" + "ABCDEFGHIJKLMNOP1234"}),
                       (-90000, "Write", {"file_path": "/work/alpha.repo/docs/old.md", "content": "x"})])
        s = self.snap_with_roots()
        self.assertEqual([d["rel"] for d in s["docs"]], ["docs/c.md"])
        out = json.dumps(dh.doc_diff(s["docs"][0]["id"]))
        self.assertNotIn("abc123", out)
        self.assertNotIn("ghp_ABCDEF", out)

    def test_architect_sendmessage_becomes_out_message_and_dot_dirs_not_viewable(self):
        arch = "/work/arch"
        self.add_main([(-60, "SendMessage", {"to": "tm-a", "message": "go ahead"}), (-30, "Edit", {"file_path": "/work/arch/.claude/skills/s/SKILL.md", "old_string": "a", "new_string": "b"})],
                      enc=dh.encode_cwd(arch))
        s = self.snap_with_roots({"architect": arch})
        self.assertEqual([(m["dir"], m["session"], m["kind"], m["text"]) for m in s["messages"]], [("out", "tm-a", "sendmessage", "go ahead")])
        self.assertEqual((s["docs"][0]["session"], s["docs"][0]["view"]), ("tm-architect", None))
        self.assertEqual((s["docs"][0]["title"], s["docs"][0]["full"]), ("SKILL", "/work/arch/.claude/skills/s/SKILL.md"))

    def snap_with_roots(self, roots=None):
        dh._agent_cache.clear()
        return dh.build(self.inbox, self.proj, self.sdocs, [], now=NOW, roots=roots or {"alpha": self.cwd})


class TestDocTitle(unittest.TestCase):
    def test_first_h1_else_readable_basename(self):
        with tempfile.TemporaryDirectory() as d:
            w = lambda n, t: (open(os.path.join(d, n), "w").write(t), os.path.join(d, n))[1]  # noqa: E731
            self.assertEqual(dh.doc_title(w("a.md", "intro\n```\n# not me\n```\n# Real Title ##\n## sub\n")), "Real Title")
            self.assertEqual(dh.doc_title(w("pr10212-body_notes.md", "no heading here\n## only h2\n")), "pr10212 body notes")
            self.assertEqual(dh.doc_title(os.path.join(d, "gone-file.md")), "gone file")      # unreadable: basename fallback


class TestCache(unittest.TestCase):
    def test_ttl(self):
        t = [100.0]
        c = dh.HarnessCache(ttl=4, clock=lambda: t[0])
        n = []
        fn = lambda: n.append(1) or len(n)  # noqa: E731
        self.assertEqual((c.get(fn), c.get(fn)), (1, 1))
        t[0] += 4.1
        self.assertEqual(c.get(fn), 2)


class TestEndpoint(Fx):
    def setUp(self):
        super().setUp()
        self.store = ds.Store(os.path.join(self._tmp.name, "q"))
        self.store.set("sessions", "S01", {"name": "tm-a", "goal": "G01 · Ship it", "summary": "s"}, None)
        page = os.path.join(self._tmp.name, "index.html")
        open(page, "w").write("<html><head></head><body></body></html>")
        live = os.path.join(self._tmp.name, "live.html")
        open(live, "w").write("<html>LIVE</html>")
        self.srv = serve.DashboardServer(0, self.store, page, projects={}, live_page=live, inbox=self.inbox, claude_projects=self.proj)
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.addCleanup(self.srv.server_close)
        self.addCleanup(self.srv.shutdown)

    def call(self, path, method="GET", headers=None, srv=None):
        req = urllib.request.Request(f"http://127.0.0.1:{(srv or self.srv).server_address[1]}{path}", method=method, headers=headers or {})
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, r.read()
        except urllib.error.HTTPError as e:
            return e.code, e.read()

    def test_shape_read_only_and_live_page(self):
        s, b = self.call("/api/harness")
        self.assertEqual(s, 200)
        d = json.loads(b)
        self.assertEqual(set(d), {"generated", "epoch", "window_s", "sessions", "messages", "docs", "sources"})
        self.assertEqual(next(x for x in d["sessions"] if x["name"] == "tm-a")["goal"], "G01")
        for m in ("POST", "PUT", "DELETE"):
            self.assertEqual(self.call("/api/harness", m)[0], 405)
        self.assertEqual(self.call("/api/harness", headers={"Host": "evil.example:7890"})[0], 403)
        self.assertEqual(self.call("/live"), (200, b"<html>LIVE</html>"))
        self.assertEqual(self.call("/live", "POST")[0], 405)

    def test_docs_in_snapshot_and_diff_endpoint(self):
        jl(os.path.join(self.proj, self.enc, "live.jsonl") if os.makedirs(os.path.join(self.proj, self.enc), exist_ok=True) is None else "",
           [{"type": "assistant", "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - 60)),      # the server reads the wall clock
             "message": {"content": [{"type": "tool_use", "name": "Edit", "input": {"file_path": "/work/alpha.repo/docs/a.md", "old_string": "x", "new_string": "y"}}]}}])
        d = json.loads(self.call("/api/harness")[1])
        self.assertEqual(len(d["docs"]), 1)
        s, b = self.call("/api/harness/diff?id=" + d["docs"][0]["id"])
        self.assertEqual(s, 200)
        self.assertIn("+y", json.loads(b)["diff"])
        self.assertEqual(self.call("/api/harness/diff?id=zzz")[0], 404)
        self.assertEqual(self.call("/api/harness/diff?id=x", "POST")[0], 405)
        self.assertEqual(self.call("/api/harness/diff?id=x", headers={"Host": "evil.example:7890"})[0], 403)

    def test_session_mode_does_not_expose_it(self):
        srv = serve.DashboardServer(0, self.store, os.path.join(self._tmp.name, "index.html"), config=serve.session_config("t", "b/"), projects={})
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        self.assertEqual(self.call("/api/harness", srv=srv)[0], 404)
        self.assertEqual(self.call("/live", srv=srv)[0], 404)
        self.assertEqual(self.call("/api/harness/diff?id=x", srv=srv)[0], 404)


class TestPage(unittest.TestCase):
    def test_live_page_polls_the_endpoint_and_index_links_it(self):
        base = os.path.join(os.path.dirname(HERE), "templates", "dashboard")
        live = open(os.path.join(base, "live.html"), encoding="utf-8").read()
        self.assertIn('fetch("/api/harness"', live)
        self.assertIn("demo", live)
        self.assertNotIn("sendMsg", live)       # the mock generators are gone from the live path
        idx = open(os.path.join(base, "index.html"), encoding="utf-8").read()
        self.assertIn('href="/live"', idx)
        self.assertIn('data-src="/live?embed=1"', idx)                 # Visualize opens on Live (ar-Tea)
        self.assertIn('def: "live", subs: [["live", "Live"', idx)
        self.assertIn('data-tab="docs"', live)
        self.assertIn('data-tab="ev">Events</button>', live)           # tab renamed from "Events / messages"
        self.assertNotIn("Events / messages", live + idx)
        self.assertIn('#main[data-cat="visualize"][data-sub="live"] { max-width: none; }', idx)   # Live uses the full main width
        self.assertIn("ResizeObserver", live)

    def test_live_node_click_filters_side_panel_and_docs_get_a_symbol(self):
        live = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "live.html"), encoding="utf-8").read()
        for hook in ("function selectNode(", "function clearFilter(", 'data-fclear="1"', 'id="fbar"',   # click -> filter chip, removable
                     "docsOf(snap)", "evOf(snap)",                                                       # panel rows filtered by node
                     'svg.addEventListener("click"',                                                     # empty canvas clears
                     "function docMark(", 'class: "dg"', "has docs"):                                    # doc symbol on nodes + legend
            self.assertIn(hook, live)
        self.assertNotIn("openDrawer(h.name, null", live)      # a node click no longer opens the drawer; the chip's Details button does
        self.assertIn("data-fdet", live)

    def test_live_docs_rows_are_title_links_with_path_hover_only(self):
        live = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "live.html"), encoding="utf-8").read()
        self.assertIn('class="dlink"', live)
        self.assertIn('title="\' + esc(x.full || x.path)', live)           # full path only as the hover title
        self.assertIn("x.title", live)
        self.assertNotIn('class="m">', live)                              # the meta line is gone
        self.assertNotIn('<span class="p">', live)                        # so is the visible path line

    def test_docs_rows_carry_the_owning_session_for_the_node_filter(self):
        self.assertIn('"session": c["session"]', open(dh.__file__, encoding="utf-8").read())

    def test_live_side_panel_closed_by_default_with_close_control_and_doc_glyph_opens_docs(self):
        live = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "live.html"), encoding="utf-8").read()
        self.assertIn("setSide(false);", live)                            # closed on load
        self.assertNotIn("setSide(window.innerWidth", live)
        self.assertIn('id="sclose"', live)
        self.assertIn('aria-label="Close panel"', live)
        self.assertIn('e.key === "Escape") { if (ui.h) closeDrawer(); else if (!$("side").hidden) setSide(false)', live)
        self.assertIn('selectNode(f, "docs")', live)                      # doc glyph opens the Docs tab


if __name__ == "__main__":
    unittest.main()
