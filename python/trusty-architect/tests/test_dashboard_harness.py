"""Live view collector: dashboard_harness.build and GET /api/harness, GET /live (IL ar-Te8).

Covers: state mapping (ok/slow/stop/park incl. blocks_session item and blocker message), message window, one-line truncation
and secret redaction, in/out directions, missing-source reporting, subagent discovery (live, ended, stale, token dedup, session join),
cache TTL, the endpoint (shape, GET only, Host check, session-mode 404, no write) and the /live page wiring.
Run: python3 -m unittest tests/test_dashboard_harness.py
Test: tests/test_dashboard_harness.py
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

    def add_agent(self, aid, age=10, end=False, tokens=(100, 50), desc="Fix the thing", finished=False, sess="s1", meta_file=True, enc=None, ctx=0):
        d = os.path.join(self.proj, enc or self.enc, sess, "subagents")
        os.makedirs(d, exist_ok=True)
        meta = {"agentType": "rust-engineer", "description": desc, "spawnedWithWorktree": True, "worktreeBranch": "worktree-agent-" + aid}
        if finished:
            meta["finished"] = True
        if meta_file:
            json.dump(meta, open(os.path.join(d, f"agent-{aid}.meta.json"), "w"))
        rows = [{"type": "user", "timestamp": iso(-300), "message": {"role": "user", "content": "SECRET PROMPT TEXT"}}]
        for i, n in enumerate(tokens):
            m = {"role": "assistant", "usage": {"output_tokens": n, "input_tokens": ctx // 2, "cache_read_input_tokens": ctx - ctx // 2}, "content": [{"type": "tool_use", "name": "Edit", "input": {"file_path": "/x/y/lib.rs", "old_string": "SECRET"}}]}
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


class TestSessionTasks(Fx):
    def test_tasks_and_factory_task_come_from_items_and_factory_goals(self):
        items = [{"kind": "task", "session": "tm-a", "status": "open", "title": "Ship X"}, {"kind": "task", "session": "tm-a", "status": "done", "title": "Old"},
                 {"kind": "task", "session": "tm-a", "status": "open", "blocks_session": True, "title": "Stuck"}, {"kind": "task", "session": "tm-a", "status": "wont_do", "title": "No"},
                 {"kind": "action", "session": "tm-a", "status": "open", "title": "not a task"}, {"kind": "task", "session": "tm-b", "status": "open", "title": "B"}]
        goals = [{"kind": "factory", "lifecycle": "active", "sessions": ["tm-a", "tm-c"]}, {"kind": "factory", "lifecycle": "done", "sessions": ["tm-b"]}, {"kind": "directed", "sessions": ["tm-d"]}]
        dh._agent_cache.clear()
        s = dh.build(self.inbox, self.proj, self.sdocs, items, now=NOW, goal_docs=goals)
        self.assertEqual([(t["state"], t["title"]) for t in self.by(s, "tm-a")["tasks"]],
                         [("factory", dh.FACTORY_TASK), ("open", "Ship X"), ("done", "Old"), ("blocked", "Stuck")])
        self.assertEqual([t["state"] for t in self.by(s, "tm-c")["tasks"]], ["factory"])        # factory task only, no items
        self.assertEqual([t["state"] for t in self.by(s, "tm-b")["tasks"]], ["open"])           # a closed factory goal adds nothing
        self.assertEqual(self.by(s, "tm-d")["tasks"], [])                                        # a directed goal is not a factory
        self.assertEqual(self.by(self.snap(items), "tm-a")["tasks"][0]["state"], "open")         # no goal docs: no factory task, never by name


class TestAgentOverridesIdle(unittest.TestCase):
    """ar-Tf9: running subagents make an idle-reading session working, not parked."""

    def test_agents_force_working(self):
        self.assertEqual(dh.map_state("idle", 389, 0, False), "park")
        self.assertEqual(dh.map_state("idle", 389, 0, False, [{"state": "ok"}]), "ok")
        self.assertEqual(dh.map_state("idle", 389, 0, False, [{"state": "slow"}]), "slow")
        self.assertEqual(dh.map_state("idle", 389, 0, False, [{"state": "park"}]), "park")
        self.assertEqual(dh.map_state("idle", 389, 0, True, [{"state": "ok"}]), "stop")


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

    def test_only_active_agents_are_listed(self):
        # the owner 2026-10-25: "Only show active subagents." Finished, ended and parked-age agents are dropped; running ones (ok, slow) stay.
        self.add_agent("a1", end=True)
        self.add_agent("a2", finished=True)
        self.add_agent("a3", age=20 * 60)
        self.add_agent("a4", age=200)
        self.add_agent("a5", age=600)
        self.add_agent("a9")
        ag = self.by(self.snap(), "tm-a")["agents"]
        self.assertEqual([(x["id"], x["state"]) for x in ag], [("a9", "ok"), ("a4", "slow")])

    def test_agent_without_meta_file_is_listed(self):
        self.add_agent("a6", meta_file=False)
        ag = self.by(self.snap(), "tm-a")["agents"]
        self.assertEqual([(x["id"], x["type"], x["state"]) for x in ag], [("a6", "agent", "ok")])

    def test_agent_carries_ctx_and_tool_events_without_content(self):
        self.add_agent("a7", tokens=(10, 20), ctx=80000)
        a = self.by(self.snap(), "tm-a")["agents"][0]
        self.assertEqual(a["ctx_k"], 80.0)
        self.assertEqual([(e["tool"], e["file"]) for e in a["events"]], [("Edit", "lib.rs")])      # the streamed duplicate and the repeat collapse
        self.assertNotIn("SECRET", json.dumps(a))
        self.add_agent("a8")
        self.assertIsNone(next(x for x in self.by(self.snap(), "tm-a")["agents"] if x["id"] == "a8")["ctx_k"])    # no usage figure -> unknown

    def test_agent_joins_the_session_whose_session_id_spawned_it(self):
        # two sessions share one cwd: tm-a is working (it used to take every agent), tm-b spawned this one by session_id
        jl(os.path.join(self.inbox, "events.jsonl"), [{"tmux_session": "tm-a", "cwd": self.cwd, "session_id": "sid-a"},
                                                      {"tmux_session": "tm-b", "cwd": self.cwd, "session_id": "sid-b"}])
        self.add_agent("b1", sess="sid-b")
        self.add_agent("a1x", sess="sid-a")
        s = self.snap()
        self.assertEqual([x["id"] for x in self.by(s, "tm-b")["agents"]], ["b1"])
        self.assertEqual([x["id"] for x in self.by(s, "tm-a")["agents"]], ["a1x"])

    def test_session_with_no_event_joins_its_transcript_dir_by_name(self):
        # tm-zed-01 has no event and no store project; its dir ends "-zed" (as tm-ai-power-rankings did)
        live = os.path.join(self.inbox, "dashboard-live.json")
        d = json.load(open(live))
        d["tm-zed-01"] = {"state": "working", "idle_min": 0}
        json.dump(d, open(live, "w"))
        self.add_agent("z1", enc="-work-zed")
        self.add_agent("z2", enc="-work-notzed")
        self.assertEqual([x["id"] for x in self.by(self.snap(), "tm-zed-01")["agents"]], ["z1"])

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
        self.assertEqual([d["rel"] for d in s["docs"]], ["src/x.rs", "docs/a.md", "docs/b.md"])      # newest first, one row per file; the .rs edit counts (the owner 2026-10-24)
        code, a = s["docs"][0], s["docs"][1]
        self.assertEqual((code["kind"], code["view"], code["title"], code["path"]), ("code", None, "x.rs", "src/x.rs"))     # no reader link for code
        self.assertIn("+fn main(){}", dh.doc_diff(code["id"])["diff"])
        self.assertEqual((a["project"], a["session"], a["n"], a["view"], a["kind"]), ("alpha", "tm-a", 2, {"project": "alpha", "rel": "docs/a.md"}, "doc"))
        d = dh.doc_diff(a["id"])
        self.assertIn("-three", d["diff"])
        self.assertIn("+four", d["diff"])
        w = dh.doc_diff(s["docs"][2]["id"])
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

    def test_agent_edits_are_attributed_to_the_agent_and_secret_code_files_skipped(self):
        self.add_agent("abcd1234ef")                       # its transcript Edits /x/y/lib.rs
        self.add_main([(-30, "Write", {"file_path": "/work/alpha.repo/.env", "content": "K=v"}), (-20, "Edit", {"file_path": "/work/alpha.repo/src/m.py", "old_string": "a", "new_string": "b"})])
        rows = {d["rel"]: d for d in self.snap_with_roots()["docs"]}
        self.assertEqual((rows["lib.rs"]["agent"], rows["lib.rs"]["session"], rows["lib.rs"]["kind"]), ("abcd1234", "tm-a", "code"))
        self.assertEqual(rows["src/m.py"]["agent"], "")
        self.assertNotIn(".env", rows)

    def snap_with_roots(self, roots=None):
        dh._agent_cache.clear()
        return dh.build(self.inbox, self.proj, self.sdocs, [], now=NOW, roots=roots or {"alpha": self.cwd})


class TestMessagedNeverPark(Fx):
    """the owner 2026-10-25: a session with a message in or out within ACTIVITY_S is never park; SendMessage counts as a relay; Architect state."""

    def test_messaged_idle_session_is_not_park_but_unmessaged_stays_park(self):
        self.assertEqual(self.by(self.snap(), "tm-c")["state"], "park")   # idle 90 min, no messages
        jl(os.path.join(self.inbox, "relays.jsonl"), [{"ts": iso(-120), "session": "tm-c", "kind": "relay", "text": "go"}])
        self.assertEqual(self.by(self.snap(), "tm-c")["state"], "slow")   # relay out
        os.remove(os.path.join(self.inbox, "relays.jsonl"))
        jl(os.path.join(self.inbox, "pm-messages.jsonl"), [{"ts": iso(-120), "session": "tm-c", "kind": "msg", "text": "hi"}])
        self.assertEqual(self.by(self.snap(), "tm-c")["state"], "slow")   # PM message in

    def test_message_older_than_activity_window_does_not_save_a_parked_session(self):
        jl(os.path.join(self.inbox, "relays.jsonl"), [{"ts": iso(-dh.ACTIVITY_S - 60), "session": "tm-c", "kind": "relay", "text": "old"}])
        self.assertEqual(self.by(self.snap(), "tm-c")["state"], "park")

    def test_sendmessage_counts_as_relay_source_and_unparks_target(self):
        arch = "/work/arch"
        self.add_main([(-60, "SendMessage", {"to": "tm-c", "message": "go"})], enc=dh.encode_cwd(arch))
        dh._agent_cache.clear()
        s = dh.build(self.inbox, self.proj, self.sdocs, [], now=NOW, roots={"architect": arch})
        self.assertEqual(s["sources"]["relays"], "ok")
        self.assertEqual(self.by(s, "tm-c")["state"], "slow")

    def test_sendmessage_to_short_name_resolves_to_the_session(self):
        self.assertEqual(dh.resolve_target("c", ["tm-a", "tm-c-01"]), "tm-c-01")
        self.assertEqual(dh.resolve_target("tm-a", ["tm-a"]), "tm-a")
        self.assertEqual(dh.resolve_target("zzz", ["tm-a"]), "zzz")

    def test_architect_state_rules(self):
        st = dh.architect_state
        self.assertEqual(st(NOW - 30, 0, "sleeping", NOW), "ok")       # transcript written
        self.assertEqual(st(0, NOW - 30, "sleeping", NOW), "ok")       # message
        self.assertEqual(st(NOW - 3600, NOW - 3600, "sleeping", NOW), "park")
        self.assertEqual(st(NOW - 3600, 0, "active", NOW), "slow")      # idle but not sleeping is never park
        self.assertEqual(st(0, 0, None, NOW), "slow")

    def test_architect_state_in_snapshot_follows_transcript_mtime(self):
        arch = "/work/arch"
        self.add_main([(-60, "Bash", {"command": "ls"})], enc=dh.encode_cwd(arch), age=20)
        dh._agent_cache.clear()
        s = dh.build(self.inbox, self.proj, self.sdocs, [], now=NOW, roots={"architect": arch}, user_state="sleeping")
        self.assertEqual(s["architect"]["state"], "ok")
        p = os.path.join(self.proj, dh.encode_cwd(arch), "sess1.jsonl")
        os.utime(p, (NOW - 3600, NOW - 3600))
        s = dh.build(self.inbox, self.proj, self.sdocs, [], now=NOW, roots={"architect": arch}, user_state="sleeping")
        self.assertEqual(s["architect"]["state"], "park")


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


    def test_swr_serves_stale_while_rebuilding(self):
        t, gate, n = [100.0], threading.Event(), []
        def fn():
            n.append(1)
            if len(n) > 1:
                gate.wait(5)
            return len(n)
        c = dh.HarnessCache(ttl=4, clock=lambda: t[0], swr=True)
        self.assertEqual(c.get(fn), 1)
        t[0] += 5
        self.assertEqual(c.get(fn), 1, "expired get blocked on the rebuild instead of serving the stale value")
        gate.set()
        for _ in range(100):
            if c._val == 2:
                break
            time.sleep(0.02)
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

    def test_harness_gzip_when_accepted_and_plain_otherwise(self):
        import gzip, http.client
        def get(h):
            c = http.client.HTTPConnection("127.0.0.1", self.srv.server_address[1], timeout=5)
            c.request("GET", "/api/harness", headers=h)
            r = c.getresponse()
            return r.status, r.getheader("Content-Encoding"), r.getheader("Vary"), r.getheader("Content-Length"), r.read()
        s, enc, vary, ln, body = get({"Accept-Encoding": "gzip"})
        self.assertEqual((s, enc, vary), (200, "gzip", "Accept-Encoding"))
        self.assertEqual(int(ln), len(body))
        plain = json.loads(gzip.decompress(body))
        self.assertIn("sessions", plain)
        s, enc, vary, ln, body2 = get({})
        self.assertEqual((s, enc, vary), (200, None, "Accept-Encoding"))
        self.assertEqual(json.loads(body2)["sessions"], plain["sessions"])
        self.assertEqual(get({"Host": "evil.example:7890"})[0], 403)   # Host check unchanged

    def test_client_disconnect_is_quiet_not_a_500(self):
        import io, contextlib
        def boom(snap):
            raise BrokenPipeError(32, "Broken pipe")
        self.srv.harness_body = boom
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            try:
                self.call("/api/harness")
            except Exception:
                pass   # the connection is closed without a response; that is the point
            time.sleep(0.2)
        self.assertNotIn("internal error", err.getvalue())

    def test_shape_read_only_and_live_page(self):
        s, b = self.call("/api/harness")
        self.assertEqual(s, 200)
        d = json.loads(b)
        self.assertEqual(set(d), {"generated", "epoch", "window_s", "architect", "sessions", "messages", "docs", "sources"})
        self.assertEqual(next(x for x in d["sessions"] if x["name"] == "tm-a")["goal"], "G01")
        for m in ("POST", "PUT", "DELETE"):
            self.assertEqual(self.call("/api/harness", m)[0], 405)
        self.assertEqual(self.call("/api/harness", headers={"Host": "evil.example:7890"})[0], 403)
        status, live_body = self.call("/live")
        self.assertEqual(status, 200)
        self.assertTrue(live_body.endswith(b"<html>LIVE</html>"))                 # the page file, unchanged ...
        self.assertTrue(live_body.startswith(b'<link rel="stylesheet" href="/static/tokens.css?v='))   # ... behind the shared tokens link (no shim)
        self.assertNotIn(b"window.claude", live_body)
        self.assertEqual(self.call("/live", "POST")[0], 405)
        ks, kb = self.call("/live/kiosk")                                   # the owner 2026-10-30: kiosk opens in its own tab
        self.assertEqual((ks, kb), (200, live_body))
        self.assertEqual(self.call("/live/kiosk", "POST")[0], 405)
        self.assertEqual(self.call("/live/kiosk", headers={"Host": "evil.example:7890"})[0], 403)

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
        self.assertEqual(self.call("/live/kiosk", srv=srv)[0], 404)
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
        self.assertIn("if (paused || inflight) return;", live)          # one poll in flight at a time
        self.assertIn("new AbortController()", live)                     # a hung fetch times out and shows offline

    def test_live_node_click_filters_side_panel_and_docs_get_a_symbol(self):
        live = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "live.html"), encoding="utf-8").read()
        for hook in ("function selectNode(", "function clearFilter(", 'data-fclear="1"', 'id="fbar"',   # click -> filter chip, removable
                     "docsOf(snap)", "evOf(snap)",                                                       # panel rows filtered by node
                     'svg.addEventListener("click"',                                                     # empty canvas clears
                     "function docMark(", 'class: "dg"', "modified files"):                                    # doc symbol on nodes + legend
            self.assertIn(hook, live)
        self.assertNotIn("openDrawer(h.name, null", live)      # a node click no longer opens the drawer; the chip's Details button does
        self.assertIn("data-fdet", live)

    def test_live_arrows_pie_events_and_doc_icon(self):
        live = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "live.html"), encoding="utf-8").read()
        for hook in ('id: "ah-" + id', 'orient: "auto"', '"marker-end": "url(#ah-out)"', '"marker-end": "url(#ah-in)"',   # direction arrowheads, both ways
                     "@keyframes flow", "prefers-reduced-motion:reduce){#scene .lane{animation:none}",                 # animated lane; static under reduced motion
                     "function lanes(", "emitPulse(PMS.find((q) => q.h === h), \"out\", false, a)",                    # PM<->agent arrows
                     ".pie-used{fill:color-mix(in srgb,var(--c) 60%,var(--mix-dark))}", ".pie-rest{fill:color-mix(in srgb,var(--c) 50%,var(--mix-light))}",
                     "function wedge(", "ctxFrac(a.ctx)", "ctxRaw(h.ctx)", "ctx n/a (unfilled)",                       # agent pie, PM ring gauge; unknown = unfilled + tooltip
                     'setTab(tab || "ev")', "function nodeEvents(", "x.agent === side.f.a"):                            # node click -> events; agent docs by agent id
            self.assertIn(hook, live)
        self.assertNotIn('setTab(!ne && nd ? "docs" : "ev")', live)    # a node click never lands on Docs
        self.assertIn('selectNode(f, "docs")', live)                   # the doc icon does

    def test_live_phone_width_keeps_nodes_and_labels_readable(self):
        live = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "live.html"), encoding="utf-8").read()
        for hook in ("const NARROW = 700", "L.W = Math.round(w);",           # 1 SVG unit = 1 px below 700 px wide
                     "stackPlan(PMS.map(",                                    # stacked rows, scene grows with the PM count, page scrolls vertically
                     "html.narrow #scene .lbl{font-size:12px}", "#scene .lbl{font:600 11px"):   # labels >= 11 px
            self.assertIn(hook, live)

    def test_live_phone_stack_labels_never_overlap_or_clip_at_375(self):
        """Runs the page's own stackPlan/ellip in node for the real fleet's names at 375 px: label boxes (with doc icon) stay inside the width
        and rows never overlap vertically, with up to 7 subagents per PM."""
        import subprocess
        import re
        live = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "live.html"), encoding="utf-8").read()
        src = live[live.index("const NARROW = 700"):live.index("const isNarrow")]
        names = ["tm-proj00-xxxxxxxxxx", "tm-proj", "tm-proj02-xxxxxxx", "tm-proj03-xxxxxxxx", "tm-proj04-xxxxxxxxxx", "tm-proj05-xxxx", "tm-proj06-xxxx", "tm-proj07-", "tm-proj08-xxxxxxxxx", "tm-proj09-xxxxx", "tm-proj", "tm-proj11-xxxxxxxx", "tm-proj12-xxxxx", "tm-proj13-xxxxxxxx", "tm-proj14-x", "tm-pro", "tm-proj16-xx"]
        js = src + """
const W = 375, counts = %s.map((n, i) => i %% 8), plan = stackPlan(counts, W), names = %s, boxes = [];
names.forEach((n, i) => { const r = plan.rows[i], t = ellip(n, plan.maxChars); boxes.push([ST.labelX, r.y - 8, ST.labelX + t.length * ST.charW + 12 + 16, r.y + 8, r.y + 22 + r.lines * ST.agentDy]); });
let bad = [];
const items = [];   // every subagent circle (r = ST.agentR) and doc icon (half 8) in every row, 0..7 agents
plan.rows.forEach((r, i) => { for (let ix = 0; ix < counts[i]; ix++) { const s = agentSlot(r.y, r.per, ix); items.push(["c", s.x - ST.agentR, s.y - ST.agentR, s.x + ST.agentR, s.y + ST.agentR, names[i], ix], ["d", s.docX - 8, s.docY - 8, s.docX + 8, s.docY + 8, names[i], ix]); } });
for (let a = 0; a < items.length; a++) {
  if (items[a][3] > W) bad.push("clip " + items[a][5] + items[a][6]);
  for (let b = a + 1; b < items.length; b++) {
    const g = 4 - 1e-9, A = items[a], B = items[b];
    if (A[5] === B[5] && A[6] === B[6]) continue;   // a circle and its own doc icon are adjacent by design
    if (A[1] < B[3] + g && B[1] < A[3] + g && A[2] < B[4] + g && B[2] < A[4] + g) bad.push("touch " + A[0] + A[5] + A[6] + " " + B[0] + B[5] + B[6]);
  }
}
boxes.forEach((b, i) => { if (b[2] > W) bad.push("clip " + names[i]); if (i && boxes[i - 1][4] > b[1]) bad.push("overlap " + names[i]); });
console.log(JSON.stringify({ bad, H: plan.H, per: plan.per }));
""" % (json.dumps(list(range(len(names)))), json.dumps(names))
        out = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=20)
        self.assertEqual(out.returncode, 0, out.stderr)
        res = json.loads(out.stdout)
        self.assertEqual(res["bad"], [])
        self.assertIn("function stackPlan(", live)
        self.assertIn("isNarrow() && !V ? { x: ST.trunkX", live)            # arrows run trunk -> row in the stacked layout

    def test_live_active_inactive_split_greys_and_packs_inactive_sessions(self):
        """Active = state chip working/waiting (ok, slow, stop) or a running subagent; idle/parked/done are inactive. Runs the page's own fanPlan/stackPlan in
        node: active circles never overlap each other nor an inactive one; inactive ones may overlap only each other; phone rows for inactive are compact."""
        import subprocess
        live = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "live.html"), encoding="utf-8").read()
        for hook in ("#scene .node.inact{opacity:.42;filter:grayscale(1)", "#scene .lbl.inact{", "const isActive = (h) =>", 'classList.toggle("inact", ic)', "gN.insertBefore(p.node.g, gN.firstChild)"):
            self.assertIn(hook, live)
        src = live[live.index("const NARROW = 700"):live.index("const isNarrow")]
        fan = live[live.index("function fanOffset("):live.index("/* rank by activity score")]
        js = "const running = () => 0;\n" + src + fan + """
const L = { cx: 400, cy: 300, rx: 270, ry: 185 }, bad = [];
const place = (off, f) => ({ x: L.cx + L.rx * f * Math.cos(-Math.PI / 2 + off), y: L.cy + L.ry * f * Math.sin(-Math.PI / 2 + off) });
for (let nA = 0; nA <= 8; nA++) for (let nI = 0; nI <= 12; nI++) {
  const fp = fanPlan(nA, nI), A = fp.act.map((o, i) => Object.assign(place(o, 1), { r: 18 + 32 * (i === 0) , a: 1 })), I = fp.ina.map((o, j) => Object.assign(place(o, 1.03 + (j % 2) * 0.04), { r: 9 }));
  const all = A.concat(I);
  for (let i = 0; i < all.length; i++) for (let j = i + 1; j < all.length; j++) {
    const d = Math.hypot(all[i].x - all[j].x, all[i].y - all[j].y);
    if (d < all[i].r + all[j].r && !(i >= nA && j >= nA)) bad.push("overlap " + nA + "/" + nI + " " + i + "," + j);
  }
  if (nI > 2 && Math.hypot(I[0].x - I[1].x, I[0].y - I[1].y) >= 18) bad.push("inactive not packed " + nI);
}
const pl = stackPlan([2, 2, 2, 2], 375, [false, false, true, true]);
const full = stackPlan([2, 2, 2, 2], 375);
console.log(JSON.stringify({ bad, compact: pl.rows[3].y < full.rows[3].y - 20, flag: pl.rows[2].inact === true && !pl.rows[0].inact }));
"""
        out = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=20)
        self.assertEqual(out.returncode, 0, out.stderr)
        res = json.loads(out.stdout)
        self.assertEqual(res["bad"], [])
        self.assertTrue(res["compact"] and res["flag"])

    def test_live_legend_sits_below_the_visualization_not_over_it(self):
        """Every key lives in the one #legend block, after #live in the DOM, in normal flow: never inside #live or the SVG, never positioned over it."""
        import re
        live = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "live.html"), encoding="utf-8").read()
        body = live[live.index("<body"):]
        self.assertEqual(body.count('id="legend"'), 1)
        lv = body.index('<div id="live">')
        end_live = body.index('</svg></div>', lv)
        self.assertGreater(body.index('id="legend"'), end_live)                     # after the visualization container closes
        self.assertNotIn('id="legend"', body[lv:end_live])
        css = re.search(r"\.legend\{[^}]*\}", live).group(0)
        for bad in ("position:absolute", "position:fixed", "float:", "z-index"):
            self.assertNotIn(bad, css)
        self.assertEqual(len(re.findall(r'class="legend"|class="key"|id="legend2"', live)), 1)

    def test_live_no_session_label_meets_any_subagent_or_file_icon_at_768_and_1280(self):
        """Runs the page's own fan layout in node on the 800x600 scene (what 1280 and 768 landscape render) and the 600x800 portrait one: for every PM with
        0..7 subagents (with and without file icons) no PM label box (incl. its own file icon) intersects any subagent circle or its file glyph."""
        import subprocess
        live = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "live.html"), encoding="utf-8").read()
        fan = live[live.index("function fanOffset("):live.index("/* rank by activity score")]
        js = "const running = () => 0;\n" + fan + """
const bad = [];
for (const [W, H, fx, fy] of [[800, 600, 0.34, 0.31], [600, 800, 0.33, 0.33]]) {
  const cx = W / 2, cy = H / 2, rx = W * fx, ry = H * fy;
  for (let nP = 1; nP <= 10; nP++) for (let nA = 0; nA <= 7; nA++) for (const doc of [0, 1]) for (const len of [7, 20]) {
    const fp = fanPlan(nP, 0);
    for (let i = 0; i < nP; i++) {
      const ang = -Math.PI / 2 + fp.act[i], x = cx + rx * Math.cos(ang), y = cy + ry * Math.sin(ang), r = 30, out = Math.atan2(y - cy, x - cx), lb = labelBox(x, y, r, len, doc);
      for (let k = 0; k < nA; k++) {
        const th = out + fanOffset(k, nA), rr = 10, d = clearLabel(x, y, th, r + rr + 8, rr, doc, lb);
        if (hits(agentRect(x + d * Math.cos(th), y + d * Math.sin(th), rr, doc), lb, 0)) bad.push([W, nP, nA, i, k]);
      }
    }
  }
}
console.log(JSON.stringify({ bad: bad.slice(0, 5), n: bad.length }));
"""
        out = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=60)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(json.loads(out.stdout)["n"], 0, out.stdout)
        self.assertIn("clearLabel(p.x, p.y, th, d,", live)          # the page applies it to every subagent in the fan

    def test_live_agent_type_maps_to_base_class_icon(self):
        import subprocess
        live = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "live.html"), encoding="utf-8").read()
        src = live[live.index("const ICONS = {"):live.index("const PSL = {")]
        want = {"ops": ["ops", "local-ops", "vercel-ops", "gcp-ops", "mpm-agent-manager", "mpm-skills-manager"],
                "engineer": ["engineer", "rust-engineer", "web-ui-engineer", "refactoring-engineer", "data-engineer"],
                "research": ["research", "Explore", "Plan", "improver", "curator"],
                "review": ["code-critic", "code-analyzer", "drift-guard"], "qa": ["qa", "web-qa", "api-qa"], "documentation": ["documentation"],
                "ticketing": ["ticketing"], "version-control": ["version-control"], "security": ["security", "secrets-manager"],
                "memory": ["memory-manager"], "other": ["general-purpose", "claude", "agent", "", "totally-new"]}
        js = src + "const want = %s, bad = [];\nfor (const c in want) want[c].forEach((t) => { if (baseClass(t) !== c) bad.push(t + '->' + baseClass(t)); if (!ICONS[c]) bad.push('no icon ' + c); });\nconsole.log(JSON.stringify(bad));" % json.dumps(want)
        out = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=20)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(json.loads(out.stdout), [])
        self.assertIn("agentIcon(a,", live)                           # drawn on every subagent node
        self.assertIn('baseClass(a.type) + ") | "', live)   # tooltip names type and class

    def test_live_ops_class_uses_lucide_settings_gear(self):
        live = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "live.html"), encoding="utf-8").read()
        self.assertIn("Lucide (https://lucide.dev), ISC License", live)          # license note sits with the icons
        self.assertIn("ops=settings", live)
        self.assertIn('ops: "M12.22 2h-.44a2 2 0 0 0-2 2v.18', live)             # Lucide "settings" outer gear path, not a hand-made rays icon
        self.assertNotIn("http://", live[live.index("const ICONS = {"):live.index("const PSL = {")])   # vendored inline, no runtime fetch

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
        self.assertIn('e.key === "Escape") { if (!$("viewer").hidden) closeViewer(); else if (ui.h) closeDrawer(); else if (SPREAD.open) SPREAD.open = false; else if (FOC.name) exitFocus(); else if (!$("side").hidden) setSide(false)', live)
        self.assertIn('selectNode(f, "docs")', live)                      # doc glyph opens the Docs tab

    def _live_src(self):
        return open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "live.html"), encoding="utf-8").read()

    def test_live_focus_state_toggles_restores_and_opens_no_popup(self):
        """Runs the page's own toggleFocus/enterFocus/exitFocus in node with stubs: click centres a session, click again restores, another session
        (a cluster sibling) switches the centre, the Architect and Escape restore. Selecting opens no drawer or panel; the old focus panel is gone."""
        import subprocess
        live = self._live_src()
        src = live[live.index("function focusGeom() {"):live.index("function step(dt) {")]
        js = """const FOC = { name: null, z: 0, view: null }, calls = [], opened = [], L = { W: 800, H: 600, cx: 400, cy: 300, rx: 272, ry: 186 }, PMS = [];
const CL_ROW = 30, FOCUS_TOP = 380, FOC_SPAN = 1.6, FOC_SPAN_NARROW = 2.4, layout = () => {}, isNarrow = () => false, scrollTo = () => {}, isActive = () => true;
const side = {}, bar = { hidden: true }, hideTip = () => {}, setTab = () => {}, renderFbar = () => {}, renderSide = () => {}, clearFilter = () => calls.push("clear"), openDrawer = () => opened.push(1);
const $ = (id) => id === "focusbar" ? bar : { focus() {} }, document = { querySelector: () => null };
""" + src + """
const st = [];
toggleFocus("a"); st.push(FOC.name, bar.hidden);
toggleFocus("b"); st.push(FOC.name);
toggleFocus("b"); st.push(FOC.name, bar.hidden, calls.length);
enterFocus("a"); exitFocus(); st.push(FOC.name, opened.length);
console.log(JSON.stringify(st));"""
        out = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=20)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(json.loads(out.stdout), ["a", False, "b", None, True, 1, None, 0])
        self.assertIn('FOC.name ? exitFocus() : selectNode', live)                      # the Architect node restores the layout
        self.assertIn('else if (FOC.name) exitFocus();', live)                          # Escape: drawer first, then focus
        self.assertIn('p.link.setAttribute("display", isF ? "inline" : nar || V ? "none" : "inline")', live)   # other links hidden; the centred session keeps its spoke
        self.assertIn('if (V && V.name !== h.name) { p.lo.setAttribute("display", "none"); p.li.setAttribute("display", "none"); } else lanes(p, !!V);', live)   # the centred session keeps its Architect lanes
        self.assertIn("#scene .node>.body{pointer-events:all}", live)                    # the fill:none body takes clicks across the whole disc
        self.assertIn("nar && !FOC.name", live)                                          # phone trunk hidden
        self.assertIn("REDUCE.matches ? fT", live)                                       # reduced motion: no easing
        pm = live[live.index("function pmClick("):live.index("function focusGeom(")]
        self.assertNotIn("openDrawer", pm)                                               # selecting a session opens no drawer or modal
        for gone in ("fpanel", "renderFocus", "bandPlan", "rimShare", "gF = el(", "gT = el(", "gT.appendChild"):
            self.assertNotIn(gone, live)                                                 # no popup-style panel any more

    def test_live_focus_cluster_orbit_and_highlight_plans(self):
        """clusterPlan/orbitPlan/isHl in node. Cluster (Architect = slot 0 plus its active siblings): inside the scene at 800x600, 600x800 and the phone
        scene, no two slots meet, slot 0 is the lower-left corner; on desktop the cluster never meets the orbit of 0..12 subagents (r up to 30). Orbit
        circle area is proportional to tokens, no-token tasks get the minimum and a flag, and every circle stays inside the scene."""
        import subprocess
        live = self._live_src()
        fan = live[live.index("function fanOffset("):live.index("/* active / inactive split")]
        sizes = live[live.index("function agentSizes("):live.index("function pmClick(")]
        plan = live[live.index("const CL_ROW"):live.index("/* pure: positions for n spread")]
        js = fan + sizes + "const side = { f: null };\n" + plan + """
const bad = [];
for (const [W, H] of [[800, 600], [600, 800]]) {
  const g = { cx: W / 2, cy: H / 2, rx: W * (W > H ? 0.34 : 0.33), ry: H * (W > H ? 0.31 : 0.33), span: FOC_SPAN };
  for (let n = 1; n <= 20; n++) {
    const c = clusterPlan(n, W, H, false);
    if (c[0].x > 40 || c[0].y < H - 40) bad.push(["slot0", W, n]);
    c.forEach((q, i) => { if (q.x < 0 || q.x + 11 + 8 + 20 * 7.5 > W || q.y < 11 || q.y > H) bad.push(["out", W, n, i]); for (let j = i + 1; j < n; j++) if (Math.hypot(q.x - c[j].x, q.y - c[j].y) < 28) bad.push(["meet", W, n, i, j]); });
    if (n <= 12) for (let m = 0; m <= 12; m++) {
      const o = orbitPlan(Array.from({ length: m }, (_, i) => ({ id: "a" + i, tok: 1000 * (m - i) })), g, g.span);
      Object.values(o).forEach((q) => { if (q.x - q.r < 0 || q.x + q.r > W || q.y - q.r < 0 || q.y + q.r > H) bad.push(["orbit out", W, m]); c.forEach((s, i) => { const lblw = i ? 20 * 7.5 : 70; if (q.x - q.r < s.x + 11 + 8 + lblw && q.x + q.r > s.x - 11 && Math.abs(q.y - s.y) < q.r + 14) bad.push(["orbit meets cluster", W, n, m, i]); }); });
    }
  }
}
const Wp = 375, gp = { cx: Wp / 2, cy: 215, rx: Math.max(70, Wp / 2 - 48), ry: 125, span: FOC_SPAN_NARROW };
for (let m = 0; m <= 10; m++) Object.values(orbitPlan(Array.from({ length: m }, (_, i) => ({ id: "a" + i, tok: 500 * (m - i) })), gp, gp.span)).forEach((q) => { if (q.x - q.r < 0 || q.x + q.r > Wp || q.y - q.r < 0 || q.y + q.r > 330) bad.push(["phone orbit", m]); });
const cp = clusterPlan(4, 375, 540, true);
for (const Wn of [320, 375, 390, 430]) for (let n = 1; n <= 20; n++) {   /* phone: compact lower-left group below the orbit, inside the scene, no two slots meet */
  const Hn = FOCUS_TOP + Math.min(n, clusterPer(n, Wn, 0, true)) * CL_ROW + 24, c = clusterPlan(n, Wn, Hn, true), gn = { cx: Wn / 2, cy: 215, rx: Math.max(70, Wn / 2 - 48), ry: 125, span: FOC_SPAN_NARROW };
  if (c[0].x !== 26 || c[0].y !== Hn - 26) bad.push(["phone slot0", Wn, n]);
  c.forEach((q, i) => { if (q.x < 0 || q.y - 11 < 0 || q.y + 11 > Hn || q.x + 11 + 8 + 15 * 7.3 > Wn) bad.push(["phone out", Wn, n, i]); for (let j = i + 1; j < n; j++) if (Math.hypot(q.x - c[j].x, q.y - c[j].y) < 28) bad.push(["phone meet", Wn, n, i, j]); });
  for (let m = 0; m <= 12; m++) Object.values(orbitPlan(Array.from({ length: m }, (_, i) => ({ id: "a" + i, tok: 800 * (m - i) })), gn, gn.span)).forEach((q) => { c.forEach((s, i) => { if (q.y + q.r > s.y - 14 && q.x - q.r < s.x + 11 + 8 + 15 * 7.3 && q.x + q.r > s.x - 11) bad.push(["phone orbit meets cluster", Wn, n, m, i]); }); });
}
const o = orbitPlan([{ id: "a", tok: 40000 }, { id: "b", tok: 10000 }, { id: "c", tok: 0 }], { cx: 400, cy: 300, rx: 270, ry: 185 }, FOC_SPAN);
side.f = { a: "sel" };
console.log(JSON.stringify({ bad, ratio: (o.a.r * o.a.r) / (o.b.r * o.b.r), ra: o.a.r, c: [o.c.r, o.c.noData, o.a.noData], first: [Math.round(o.a.x), Math.round(o.a.y)],
  phone: cp.map((q) => [q.x, q.y]), hl: ["ok", "slow", "stop", "park", "done"].map((ps) => isHl({ id: "x", ps })), gone: isHl({ id: "g", ps: "ok", leaving: true }) }));"""
        out = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=60)
        self.assertEqual(out.returncode, 0, out.stderr)
        r = json.loads(out.stdout)
        self.assertEqual(r["bad"], [])
        self.assertAlmostEqual(r["ratio"], 4.0, places=6)                      # area ratio = token ratio
        self.assertAlmostEqual(r["ra"], 30)                                    # largest task gets the full radius
        self.assertEqual(r["c"], [8, True, False])                             # no tokens: minimum, flagged
        self.assertEqual(r["first"], [400, 115])                               # rank 0 at 12 o'clock
        self.assertEqual(r["phone"], [[26, 514], [26, 484], [26, 454], [26, 424]])   # phone: lower-left corner, Architect at the bottom, rows grow upward
        self.assertEqual(r["hl"], [True] * 5)            # the centred PM shows every subagent: running, parked and done (the owner 2026-10-24)
        self.assertFalse(r["gone"])

    def test_live_ring_gauge_and_task_pie_plans(self):
        """pieSlices/gaugeLaps/lapPath in node: 0 tasks = no slice, 1 task = one solid slice, up to 6 = one per task, more = one per state; the ring is
        a gauge (unknown = null, 100% = one full lap, over 100% = magenta first lap plus a second lap capped at 200%); the PM bubble draws a tick."""
        import subprocess
        live = self._live_src()
        src = live[live.index("const PIE_MAX") if False else live.index("const TSK_C"):live.index("function addGauge(")]
        js = "const CTX_FULL_K = 250;\n" + src + """
const mk = (...s) => s.map((state) => ({ state }));
console.log(JSON.stringify({ z: pieSlices([]), one: pieSlices(mk("factory")), six: pieSlices(mk("open", "open", "open", "done", "done", "blocked")).length,
  many: pieSlices(mk("open", "open", "open", "open", "done", "done", "blocked", "factory")),
  unk: gaugeLaps(ctxRaw(null)), half: gaugeLaps(ctxRaw(125)), full: gaugeLaps(ctxRaw(250)), over: gaugeLaps(ctxRaw(290)), huge: gaugeLaps(ctxRaw(900)),
  path: [lapPath(10, 0), lapPath(10, 1), lapPath(10, 0.5).startsWith("M0 -10A10 10 0 0 1")], txt: taskTxt(mk("open", "open", "factory")),
  ord: pieSlices(mk("done", "open", "blocked", "factory", "open")).map((x) => x.st).join(), txt2: taskTxt(mk("open", "blocked", "open")) }));"""
        out = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=60)
        self.assertEqual(out.returncode, 0, out.stderr)
        r = json.loads(out.stdout)
        self.assertEqual((r["z"], r["one"], r["six"]), ([], [{"st": "factory", "n": 1}], 6))
        self.assertEqual(r["many"], [{"st": "blocked", "n": 1}, {"st": "open", "n": 4}, {"st": "factory", "n": 1}, {"st": "done", "n": 2}])   # fixed slice order: blocked, open, factory, done
        self.assertIsNone(r["unk"])
        self.assertEqual((r["half"]["l1"], r["half"]["l2"], r["half"]["c"]), (0.5, 0, "var(--ctx-ok)"))
        self.assertEqual((r["full"]["l1"], r["full"]["l2"]), (1, 0))
        self.assertEqual((r["over"]["c"], round(r["over"]["l2"], 2)), ("var(--ctx-hot)", 0.16))   # 290k = 116%: oxblood lap plus a 16% second lap (the magenta lap is gone)
        self.assertEqual(r["huge"]["l2"], 1)                                                  # capped at two laps
        self.assertEqual(r["path"][0], ""); self.assertTrue(r["path"][1].startswith("M-10 0a") and r["path"][2])
        self.assertEqual(r["txt"], "3 tasks: 2 open, 1 factory (standing)")
        self.assertEqual(r["ord"], "blocked,open,open,factory,done")                          # per-task slices use the same fixed order
        self.assertEqual(r["txt2"], "3 tasks: 1 blocked, 2 open")                            # hover text in slice order
        for hook in ("addGauge(p.node)", "paintPM(p, spin)", 'class: "gtick"', "gaugeLaps(raw)", "session ring = context used", "session pie = one slice per open task"):
            self.assertIn(hook, live)

    def test_live_focus_hides_inactive_sessions_and_unhighlighted_agents(self):
        live = self._live_src()
        for hook in ("const focusSibs = () => PMS.filter((q) => q.h.name !== FOC.name && isActive(q.h))",   # cluster = active siblings only
                     "sb = pa.filter((p) => p.h.name !== FOC.name)",
                     "hidP = !!V && !isF && !isSib",                       # everyone else (inactive sessions) is hidden
                     "hidA = !!V && !sl",                                  # subagents without an orbit slot (not highlighted, or another session's) are hidden
                     "orbitPlan(ar.filter(isHl)",                          # the orbit holds the centred session's subagents (all states)
                     'e.setAttribute("display", "none")',
                     "FOC.name ? ARCH_R :",                                # the Architect shrinks into the cluster
                     "V.cl[0]"):
            self.assertIn(hook, live)

    def test_live_focus_renders_cluster_and_hides_others_in_a_browser(self):
        """Demo data in headless Chromium at desktop and phone width, light and dark: the focused session and its subagents are drawn, the Architect and
        only the active siblings are labelled, inactive sessions are display:none, no drawer opens, and the page never scrolls sideways."""
        try:
            from playwright.sync_api import sync_playwright
        except ImportError:
            self.skipTest("playwright not installed")
        url = "file://" + os.path.join(os.path.dirname(HERE), "templates", "dashboard", "live.html") + "?demo=1&focus=tm-dogfood"
        with sync_playwright() as p:
            try:
                b = p.chromium.launch()
            except Exception as e:   # no browser binary on this host
                self.skipTest("chromium unavailable: %s" % e)
            try:
                for w, h, scheme in ((1280, 800, "light"), (375, 800, "dark")):
                    pg = b.new_page(viewport={"width": w, "height": h}, color_scheme=scheme)
                    errs = []
                    pg.on("pageerror", lambda e: errs.append(str(e)))
                    pg.goto(url)
                    pg.wait_for_function("() => FOC.name === 'tm-dogfood' && FOC.z > 0.99", timeout=10000)
                    r = pg.evaluate("""() => {
                      const vis = (e) => e.getAttribute("display") !== "none", pm = (n) => PMS.find((q) => q.h.name === n);
                      const sib = PMS.filter((q) => q.h.name !== FOC.name && isActive(q.h)).map((q) => q.h.name), ina = PMS.filter((q) => q.h.name !== FOC.name && !isActive(q.h)).map((q) => q.h.name);
                      const shownAg = HARNESSES.flatMap((h) => h.agents.filter((a) => vis(a.node.g)).map((a) => [h.name, a.ps]));
                      return { sw: document.documentElement.scrollWidth, cw: document.documentElement.clientWidth, drawer: !document.getElementById("drawer").hidden,
                        centre: [pm("tm-dogfood").x, pm("tm-dogfood").y], sibShown: sib.every((n) => vis(pm(n).node.g) && vis(pm(n).lbl)), inaHidden: ina.every((n) => !vis(pm(n).node.g) && !vis(pm(n).lbl)),
                        agents: shownAg, hub: [hub.x, hub.y, L.W, L.H], sibY: sib.map((n) => pm(n).y), L: [L.W, L.H], nsib: sib.length };
                    }""")
                    self.assertEqual(errs, [])
                    self.assertEqual(r["sw"], r["cw"])                                               # no horizontal scroll
                    self.assertFalse(r["drawer"])                                                    # no popup or modal
                    self.assertTrue(r["sibShown"] and r["inaHidden"])
                    self.assertTrue(all(a[0] == "tm-dogfood" and a[1] in ("ok", "slow", "stop", "park", "done") for a in r["agents"]), r["agents"])
                    if w > 700:
                        self.assertLess(r["hub"][0], r["L"][0] * 0.2); self.assertGreater(r["hub"][1], r["L"][1] * 0.8)   # Architect: lower-left corner
                        self.assertTrue(all(y < r["hub"][1] for y in r["sibY"]))                                          # siblings stack above it
                        self.assertAlmostEqual(r["centre"][0], r["L"][0] / 2, delta=12)                                    # centred session
                    else:   # phone: the cluster stays a lower-left group, below the selected bubble, and does not overlap it
                        self.assertLess(r["hub"][0], r["L"][0] * 0.2); self.assertGreater(r["hub"][1], r["centre"][1] + 100)
                        self.assertTrue(all(y < r["hub"][1] and y > r["centre"][1] + 80 for y in r["sibY"]))
                    # the label text is a click target too (desktop and phone): a sibling label jumps to that session
                    nm = pg.evaluate("() => { const q = PMS.find((q) => q.h.name !== FOC.name && isActive(q.h)); return q ? q.h.name : null; }")
                    if nm:
                        bb = pg.evaluate("(n) => { const r = PMS.find((q) => q.h.name === n).lbl.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; }", nm)
                        self.assertEqual(pg.evaluate("(n) => getComputedStyle(PMS.find((q) => q.h.name === n).lbl).cursor", nm), "pointer")
                        pg.mouse.click(bb[0], bb[1])
                        pg.wait_for_function("(n) => FOC.name === n", arg=nm, timeout=5000)
                    pg.close()
            finally:
                b.close()

    def test_live_inactive_cluster_spreads_on_tap_and_collapses_elsewhere(self):
        """spreadPlan in node: for 0..10 inactive sessions on the 800x600 and 600x800 scenes no circle (r 16) or label box meets another, all stay inside the scene.
        pmClick: first tap on a packed inactive node spreads (no focus); a tap on a spread one focuses it; a click elsewhere or Escape collapses."""
        import subprocess
        live = self._live_src()
        plan = live[live.index("const SP_DX"):live.index("/* pure: subagent circle radii")]
        click = live[live.index("function pmClick("):live.index("function focusGeom(")]
        js = """const labelBox = (x, y, r, len) => ({ x0: x - len * 3.4 - 4, x1: x + len * 3.4 + 4, y0: y + r + 3, y1: y + r + 22 });
""" + plan + """
const bad = [];
for (const [W, H] of [[800, 600], [600, 800]]) for (let n = 0; n <= 10; n++) {
  const q = spreadPlan(n, W, H), bx = q.map((o) => { const b = labelBox(o.x, o.y, 16, 20); b.x0 = Math.min(b.x0, o.x - 16); b.x1 = Math.max(b.x1, o.x + 16); b.y0 = o.y - 16; return b; });
  q.forEach((o, i) => { if (bx[i].x0 < 0 || bx[i].x1 > W || bx[i].y1 > H || bx[i].y0 < 0) bad.push(["out", W, n, i]); });
  for (let i = 0; i < n; i++) for (let j = i + 1; j < n; j++) if (bx[i].x0 < bx[j].x1 && bx[j].x0 < bx[i].x1 && bx[i].y0 < bx[j].y1 && bx[j].y0 < bx[i].y1) bad.push(["hit", W, n, i, j]);
}
const SPREAD = { open: false, z: 0 }, FOC = { name: null }, log = [], isNarrow = () => false, hideTip = () => {}, toggleFocus = (n) => log.push("focus " + n);
""" + click + """
const p = { ia: true, h: { name: "x" } };
pmClick(p); const a = [SPREAD.open, log.length];
pmClick(p); const b = [SPREAD.open, log.slice()];
console.log(JSON.stringify({ bad, a, b }));"""
        out = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=20)
        self.assertEqual(out.returncode, 0, out.stderr)
        r = json.loads(out.stdout)
        self.assertEqual(r["bad"], [])
        self.assertEqual(r["a"], [True, 0])                      # first tap spreads, no focus
        self.assertEqual(r["b"], [False, ["focus x"]])           # second tap focuses and collapses
        self.assertIn('if (SPREAD.open && !e.target.closest(".node.inact")) SPREAD.open = false;', live)   # click elsewhere collapses
        self.assertIn("else if (SPREAD.open) SPREAD.open = false;", live)                                    # Escape collapses
        self.assertIn('aria-expanded', live)                                                                 # keyboard users hear the state
        self.assertIn("#scene.spread .lbl.inact{font-size:11px", live)                                       # legible labels when spread

    def test_live_architect_uses_the_pm_circle_viz(self):
        live = self._live_src()
        self.assertIn('paint(hn, hub.st || "ok", "circle", hub.r, spin, null)', live)   # the PMs' own paint()
        self.assertNotIn('hn.body.setAttribute("fill", "var(--hub)")', live)                          # no separate orange disc
        self.assertIn("const hub = { r: 40", live)
        self.assertIn("hub.node = mkNode(", live)                                                     # same node factory (body, ring, pie, icon slot)
        for hook in ("addGauge(hub.node)", "paintGauge(hn, hub.r,", "hub.ctx = ar.ctx_k == null ? null : ar.ctx_k", "hub.bar = ar.bar_k || CTX_FULL_K"):
            self.assertIn(hook, live)                                                                 # Architect context band from snap.architect


class TestArchitectCtx(Fx):
    """The snapshot carries the Architect's own context band (ctx_k from the newest interactive transcript, thresholds from il-schedule.json)."""

    def setup_arch(self, with_usage=True, sched=True):
        root = os.path.join(self._tmp.name, "arch.root")
        os.makedirs(os.path.join(root, "local"))
        if sched:
            json.dump({"thresholds": {"approach_ctx": 220000, "bar_ctx": 250000}}, open(os.path.join(root, "local", "il-schedule.json"), "w"))
        d = os.path.join(self.proj, dh.encode_cwd(root))
        os.makedirs(d)
        u = {"input_tokens": 5, "cache_read_input_tokens": 190000, "cache_creation_input_tokens": 10000} if with_usage else None
        rows = [{"type": "assistant", "entrypoint": "cli", "message": {"role": "assistant", **({"usage": u} if u else {})}}]
        jl(os.path.join(d, "s.jsonl"), rows)
        return root

    def build(self, root):
        dh._agent_cache.clear()
        return dh.build(self.inbox, self.proj, self.sdocs, [], now=NOW, roots={"architect": root})

    def test_ctx_and_thresholds_reach_the_snapshot(self):
        self.assertEqual(self.build(self.setup_arch())["architect"], {"state": "ok", "ctx_k": 200.0, "approach_k": 220.0, "bar_k": 250.0, "tasks": []})

    def test_unmeasurable_is_null_not_an_error(self):
        self.assertEqual(self.build(self.setup_arch(with_usage=False, sched=False))["architect"], {"state": "ok", "ctx_k": None, "approach_k": None, "bar_k": None, "tasks": []})
        self.assertEqual(self.snap()["architect"]["ctx_k"], None)    # no architect root at all

    def test_il_open_and_running_tasks_feed_the_hub_pie(self):
        root = self.setup_arch()
        d = os.path.join(root, "local", "il", ".trusty-mpm", "datg")
        os.makedirs(d)
        rows = [{"id": "ar-T01", "title": "Open  one", "status": "open"}, {"id": "ar-T02", "title": "x" * 200, "status": "running"},
                {"id": "ar-T03", "title": "closed", "status": "done"}, "junk"]
        json.dump({"tasks": rows}, open(os.path.join(d, "state.json"), "w"))
        self.assertEqual(self.build(root)["architect"]["tasks"],
                         [{"id": "ar-T01", "title": "Open one", "state": "open"}, {"id": "ar-T02", "title": "x" * 80, "state": "open"}])

    def test_malformed_il_state_gives_no_tasks(self):
        root = self.setup_arch()
        d = os.path.join(root, "local", "il", ".trusty-mpm", "datg")
        os.makedirs(d)
        open(os.path.join(d, "state.json"), "w").write("{nope")
        self.assertEqual(self.build(root)["architect"]["tasks"], [])

    def test_live_hub_paints_the_shared_pie(self):
        live = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "live.html"), encoding="utf-8").read()
        for hook in ("function paintPie(n, tasks, r)", "paintProgress(p)", "paintPie(hn, hub.tasks, hub.r)", "hub.tasks = ar.tasks || []"):
            self.assertIn(hook, live)


class FileLocTest(unittest.TestCase):
    def test_file_loc_semantics(self):
        with tempfile.TemporaryDirectory() as d:
            def w(n, b):
                open(os.path.join(d, n), "wb").write(b)
                return os.path.join(d, n)
            self.assertEqual(dh.file_loc(w("a", b"")), 0)
            self.assertEqual(dh.file_loc(w("b", b"x\n\n# c\n")), 3)   # blank and comment lines count
            self.assertEqual(dh.file_loc(w("c", b"x\ny")), 2)          # no trailing newline
            self.assertIsNone(dh.file_loc(w("d", b"ab\0cd\n")))        # binary
            self.assertIsNone(dh.file_loc(os.path.join(d, "missing")))
            self.assertIsNone(dh.file_loc(d))                           # a directory


if __name__ == "__main__":
    unittest.main()
