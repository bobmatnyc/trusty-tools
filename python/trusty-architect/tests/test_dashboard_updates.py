"""Steers section and the deterministic update ingester (the owner; see #10571).

Why: the expanded task view lists Steers above Updates, and tagged relays and PM handbacks fill Updates without a model.
What: the page's pure steers region under node (newest first, acknowledged flag, empty gives no rows) plus markup checks (Steers
built before Updates, scrolling list); dashboard_updates.ingest on a temp store and temp inbox: tagged relay ingested once, re-run
adds nothing, untagged line skipped, handback parsed for done and pct, unknown task id skipped, one paragraph truncation.
Test: this file.
"""
import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from datetime import datetime, timezone

import dashboard_store as ds
import dashboard_updates as du

HERE = os.path.dirname(os.path.abspath(__file__))
# #10571 S2: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
NOW = datetime(2026, 11, 12, 17, 30, tzinfo=timezone.utc)


class TestIngest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.root, self.inbox = os.path.join(self.tmp, "q"), os.path.join(self.tmp, "inbox")
        os.makedirs(self.inbox)
        self.st = ds.Store(self.root)
        self.st.set("items", "Ta8", {"kind": "task", "status": "open", "title": "t", "task": "ar-Tc3k9"}, if_version=0)

    def write(self, name, rows):
        with open(os.path.join(self.inbox, name), "w") as f:
            f.write("\n".join(json.dumps(r) for r in rows) + "\n")

    def run_ingest(self):
        return du.ingest(self.root, self.inbox, now=NOW)

    def updates(self):
        return self.st.get("items", "Ta8")["data"].get("updates", [])

    def relay(self, text, ts="2026-11-12T16:50:00Z"):
        return {"ts": ts, "session": "tm-x", "kind": "relay", "text": text}

    def test_tagged_relay_ingested_once_and_rerun_adds_nothing(self):
        self.write("relays.jsonl", [self.relay("Do the thing.\nTask: ar-Tc3k9@1112")])
        self.assertEqual(self.run_ingest()["added"], {"Ta8": 1})
        v = self.st.get("items", "Ta8")["version"]
        self.assertEqual(self.run_ingest()["added"], {})
        self.assertEqual(len(self.updates()), 1)
        self.assertEqual(self.st.get("items", "Ta8")["version"], v)
        self.assertEqual(self.updates()[0]["at"], "2026-11-12T16:50:00Z")
        self.assertEqual(self.updates()[0]["by"], "architect relay to tm-x")

    def test_untagged_line_skipped(self):
        self.write("relays.jsonl", [self.relay("no tag here"), self.relay("Task: research + plan only")])
        self.assertEqual(self.run_ingest()["added"], {})
        self.assertEqual(self.updates(), [])

    def test_unknown_task_id_skipped(self):
        self.write("relays.jsonl", [self.relay("x\nTask: ar-Tffk9")])
        self.assertEqual(self.run_ingest()["added"], {})

    def test_handback_parsed_for_done_and_pct(self):
        self.write("mcp-messages.jsonl", [
            {"id": "1", "ts": "2026-11-12T17:10:00Z", "sender": "tm-pm", "kind": "done",
             "text": "Report body.\n\nTask: ar-Tc3k9@1112 | done: gate 2 green | est: 30m | remaining: gate 3 | pct: 60"},
            {"id": "2", "ts": "2026-11-12T17:11:00Z", "sender": "tm-pm", "kind": "fyi", "text": "chat\nTask: ar-Tc3k9 mention not a handback"}])
        self.assertEqual(du.parse_handback("a\nTask: ar-T63k9@1112 | done: x y | est: 1h | remaining: z | pct: 40"), ("ar-T63k9", "x y", 40))
        self.assertEqual(self.run_ingest()["added"], {"Ta8": 1})
        u = self.updates()[0]
        self.assertEqual((u["text"], u["by"]), ("done: gate 2 green (pct 60)", "tm-pm"))

    def test_one_paragraph_and_truncation(self):
        text = "first para " + "w" * 600 + "\n\nsecond para\nTask: ar-Tc3k9"
        self.write("relays.jsonl", [self.relay(text)])
        self.run_ingest()
        t = self.updates()[0]["text"]
        self.assertLessEqual(len(t), du.MAX_CHARS)
        self.assertNotIn("second", t)

    def test_old_lines_outside_window_skipped(self):
        self.write("relays.jsonl", [self.relay("old\nTask: ar-Tc3k9", ts="2026-11-01T00:00:00Z")])
        self.assertEqual(self.run_ingest()["added"], {})

    def test_bare_ar_id_matches_and_bare_dashboard_id_does_not(self):
        self.write("relays.jsonl", [
            self.relay("nothing", ts="2026-11-12T16:40:00Z"),
            self.relay("see (owner, 11-12 13:00Z, ar-Tc3k9) ok", ts="2026-11-12T16:41:00Z"),
            self.relay("Tc3k9 is closed", ts="2026-11-12T16:42:00Z"),
            self.relay("xar-Tc3k9 and ar-Tc3k9x", ts="2026-11-12T16:43:00Z")])
        self.assertEqual(self.run_ingest()["added"], {"Ta8": 1})
        self.assertEqual([u["at"] for u in self.updates()], ["2026-11-12T16:41:00Z"])

    def test_notes_source_and_cli(self):
        self.write("task-notes.jsonl", [{"ts": "2026-11-12T17:00:00Z", "task": "ar-Tc3k9", "by": "Architect", "text": "verified\n\nmore"}])
        self.assertEqual(self.run_ingest()["added"], {"Ta8": 1})
        self.assertEqual((self.updates()[0]["by"], self.updates()[0]["text"]), ("Architect", "verified"))
        env = dict(os.environ, DASHBOARD_ROOT=self.root, DASHBOARD_INBOX=self.inbox)
        script = os.path.join(SCRIPTS, "dashboard_updates.py")
        r = subprocess.run(["python3", script, "note", "Tc3k9", "cli note", "--by", "eng"], capture_output=True, text=True,
                           env=env, cwd=self.tmp)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.loads(r.stdout)["added"], {"Ta8": 1})
        self.assertEqual(len(open(os.path.join(self.inbox, "task-notes.jsonl")).read().splitlines()), 2)

    def test_cli_note_appends_line_and_ingests(self):
        os.makedirs(self.inbox, exist_ok=True)
        row = du.add_note("ar-Tc3k9", "hello", by="eng", inbox=self.inbox, now=datetime.now(timezone.utc))
        self.assertEqual(row["task"], "ar-Tc3k9")
        self.assertEqual(du.ingest(self.root, self.inbox)["added"], {"Ta8": 1})
        self.assertEqual(du.main(["note", "x"]), 2)

    def test_shared_task_goes_to_open_task_item_only(self):
        self.st.set("items", "Ta9", {"kind": "task", "status": "done", "created": "2026-11-12T09:00:00Z", "task": "ar-Tc3k9"}, if_version=0)
        # #10571 S2: the store stamps `created` itself (#10678), so each write gets its own clock here.
        at = lambda ts: ds.Store(self.root, clock=lambda: ts)  # noqa: E731
        at("2026-11-12T12:00:00Z").set("items", "Tb0", {"kind": "task", "status": "open", "created": "x", "task": "ar-Tc3k9"}, if_version=0)
        at("2026-11-12T11:00:00Z").set("items", "Tb1", {"kind": "task", "status": "open", "created": "x", "task": "ar-Tc3k9"}, if_version=0)
        self.st.set("items", "Tz1", {"kind": "action", "status": "open", "created": "2026-11-12T17:00:00Z", "task": "ar-Tc3k9"}, if_version=0)
        self.write("relays.jsonl", [self.relay("old\nTask: ar-Tc3k9", ts="2026-11-12T10:00:00Z")])
        self.assertEqual(self.run_ingest()["added"], {"Tb1": 1})
        self.assertEqual(self.st.get("items", "Tz1")["data"].get("updates"), None)

    def put(self, i, **kw):
        self.st.set("items", i, dict({"kind": "question", "status": "answered", "task": "ar-Tc3k9", "session": "tm-x"}, **kw), if_version=0)

    def test_answer_relay_done_entries_land_on_task_item(self):
        self.put("Dq1", answered_at="2026-11-12T17:41:27.914Z", answer="Changes first", answer_note="see preview",
                 relayed_at="2026-11-12T17:44:47Z")
        self.put("Dq2", status="done", done_at="2026-11-12T17:50:00Z", outcome="Shipped.\n\nmore")
        self.assertEqual(self.run_ingest()["added"], {"Ta8": 3})
        got = {(u["at"], u["by"], u["text"]) for u in self.updates()}
        self.assertIn(("2026-11-12T17:41:27.914Z", "owner", "Owner answered Dq1: Changes first: see preview"), got)
        self.assertIn(("2026-11-12T17:44:47Z", "architect", "Dq1 relayed to tm-x"), got)
        self.assertIn(("2026-11-12T17:50:00Z", "architect", "Dq2 done: Shipped."), got)
        self.assertEqual(self.run_ingest()["added"], {})

    def test_changed_answer_adds_entry_and_keeps_old(self):
        self.put("Dq1", answered_at="2026-11-12T17:00:00Z", answer="A")
        self.run_ingest()
        self.st.update("items", "Dq1", {"answer": "B", "answered_at": "2026-11-12T17:20:00Z"}, if_version=self.st.get("items", "Dq1")["version"])
        self.assertEqual(self.run_ingest()["added"], {"Ta8": 1})
        self.assertEqual([u["text"] for u in self.updates()], ["Owner answered Dq1: A", "Owner answered Dq1: B"])

    def test_note_stale_when_answer_after_status_note(self):
        self.st.update("items", "Ta8", {"status_at": "2026-11-12T17:00:00Z"}, if_version=self.st.get("items", "Ta8")["version"])
        self.put("Dq1", answered_at="2026-11-12T17:41:00Z", answer="A")
        self.put("Dq2", answered_at="2026-11-12T16:00:00Z", answer="old")
        docs = du.annotate_stale(self.st.list_docs("items"))
        ta8 = next(d for d in docs if d["id"] == "Ta8")["data"]
        self.assertEqual((ta8["note_stale"], ta8["note_stale_by"]), (True, ["Dq1"]))
        self.assertNotIn("note_stale", next(d for d in docs if d["id"] == "Dq1")["data"])
        self.st.update("items", "Ta8", {"status_at": "2026-11-12T18:00:00Z"}, if_version=self.st.get("items", "Ta8")["version"])
        self.assertEqual(du.stale_notes(self.st.list_docs("items")), {})


if __name__ == "__main__":
    unittest.main()

    def test_owner_label_comes_from_the_config(self):
        # #10571 S2: the owner's name is local/architect.json data (ARCHITECT_OWNER_NAME overrides it), never a constant.
        self.put("Dq1", answered_at="2026-11-12T17:41:00Z", answer="A")
        old = os.environ.get("ARCHITECT_OWNER_NAME")
        os.environ["ARCHITECT_OWNER_NAME"] = "Ann"
        try:
            self.run_ingest()
        finally:
            os.environ.pop("ARCHITECT_OWNER_NAME") if old is None else os.environ.update(ARCHITECT_OWNER_NAME=old)
        self.assertIn("Ann answered Dq1: A", [u["text"] for u in self.updates()])
