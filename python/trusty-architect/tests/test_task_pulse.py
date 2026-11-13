"""task_pulse.py on temp ledgers, a temp dashboard store and a temp inbox (ar-Tf5, D09).

Covers: reported-not-progress, id@stamp orphans, rejects, idempotent reruns, cursor, version conflict (retry and
give-up), rate limit, dry-run, never closing, distrust flags, stale by signal, signals, wip, compliance.
Run: python3 -m pytest tests/test_task_pulse.py
"""
import json
import os
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
# #11101: the shipped layout keeps the modules in ../scripts, the tests in tests/.
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "scripts"))
import dashboard_store as ds  # noqa: E402
import datg  # noqa: E402
import task_pulse as tp  # noqa: E402

NOW = time.time()


class Base(unittest.TestCase):
    def setUp(self):
        global NOW
        NOW = time.time() + 30    # after every ledger task made in this setUp (their created stamp is minute-floored real time)
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        t = self.tmp.name
        self.inbox, self.root, self.proj = (os.path.join(t, d) for d in ("inbox", "dash", "proj"))
        for d in (self.inbox, self.root, self.proj):
            os.makedirs(d)
        datg.op_init(self.proj, "ar")
        datg.write_ops(self.proj, [{"op": "add", "kind": "G", "title": "goal"},
                                   {"op": "add", "kind": "T", "title": "the task", "goal": "ar-G01"}])
        self.il = datg.load_state(self.proj)["tasks"][0]
        self.stamp = datg.task_mmdd(self.il)
        self.sched = os.path.join(t, "il-schedule.json")
        with open(self.sched, "w") as f:
            json.dump({"sessions": [{"name": "tm-architect", "project": self.proj, "code": "ar"}]}, f)
        self.store = ds.Store(root=self.root, actor="test")
        self.put("T93", il="ar-T01", created=tp.iso_s(NOW - 3600))
        self.cfg = self.mkcfg()

    def mkcfg(self, **kw):
        kw.setdefault("now", NOW)
        return tp.make_cfg(inbox=self.inbox, dash_root=self.root, schedule=self.sched, **kw)

    def put(self, doc_id, **kw):
        data = {"kind": "task", "status": "open", "title": "dash task", "session": "tm-x", "goal": "G01", "rank": 1,
                "created": tp.iso_s(NOW - 3600), "updated": tp.iso_s(NOW - 3600),
                "progress": {"pct": 10, "estimated_at": tp.iso_s(NOW - 3600)}}
        data.update(kw)
        self.store.set("items", doc_id, data, if_version=0)

    def dash(self, doc_id="T93"):
        return self.store.get("items", doc_id)

    def il_task(self):
        return datg.load_state(self.proj)["tasks"][0]

    def line(self, pct=40, id_=None, extra=""):
        return f"Did work.\nTask: {id_ or 'ar-T01@' + self.stamp} | done: wired it{extra} | remaining: tests | pct: {pct}"

    def jl(self, name):
        return tp.read_jsonl(os.path.join(self.inbox, name))


class PulseCase(Base):
    def test_pulse_writes_reported_and_never_progress_status_or_closing(self):
        (r,) = tp.cmd_run(self.cfg, self.line(pct=100), "tm-x")
        self.assertEqual(r["status"], "applied")
        d = self.dash()["data"]
        self.assertEqual(d["progress"]["pct"], 10)                    # the main progress pct is untouched
        self.assertEqual((d["status"], d["reported"]["pct"], d["reported"]["by"]), ("open", 100, "tm-x"))
        self.assertEqual(d["reported"]["label"], "reported by tm-x, unverified")
        self.assertEqual(d["last_msg_at"], tp.iso_s(NOW))
        il = self.il_task()
        self.assertEqual(il["status"], "open")
        self.assertIn("done: wired it", il["pulse"])
        self.assertEqual(il["outcome"], "")

    def test_dashboard_id_and_il_id_resolve_to_the_same_pair(self):
        (r,) = tp.cmd_run(self.cfg, "Task: T93 | done: via dash id | pct: 20", "tm-x")
        self.assertEqual(r["status"], "applied")
        self.assertIn("via dash id", self.il_task()["pulse"])
        self.assertEqual(self.dash()["data"]["reported"]["pct"], 20)

    def test_rerun_of_same_text_is_a_dup_and_writes_nothing(self):
        tp.cmd_run(self.cfg, self.line(), "tm-x")
        v, il_v = self.dash()["version"], datg.load_state(self.proj)["v"]
        (r,) = tp.cmd_run(self.mkcfg(), self.line(), "tm-x")
        self.assertEqual(r["status"], "dup")
        self.assertEqual((self.dash()["version"], datg.load_state(self.proj)["v"]), (v, il_v))

    def test_cursor_reads_each_message_once(self):
        path = os.path.join(self.inbox, "pm-messages.jsonl")
        with open(path, "w") as f:
            f.write(json.dumps({"ts": tp.iso_s(NOW + 10), "session": "tm-x", "kind": "progress", "text": self.line()}) + "\n")
            f.write(json.dumps({"ts": tp.iso_s(NOW + 20), "session": "tm-x", "kind": "fyi", "text": "no tag here"}) + "\n")
            f.write('{"partial": ')                                       # no newline: waits for the next cycle
        (r,) = tp.cmd_run(self.cfg)
        self.assertEqual(r["status"], "applied")
        off = tp.load_pstate(self.cfg)["offset"]
        self.assertEqual(off, os.path.getsize(path) - len('{"partial": '))
        v = self.dash()["version"]
        self.assertEqual(tp.cmd_run(self.mkcfg()), [])
        self.assertEqual(self.dash()["version"], v)

    def test_dry_run_writes_nothing(self):
        before = (self.dash()["version"], datg.load_state(self.proj)["v"])
        (r,) = tp.cmd_run(self.mkcfg(dry=True), self.line(40), "tm-x")
        self.assertEqual(r["status"], "dry-run")
        self.assertEqual((self.dash()["version"], datg.load_state(self.proj)["v"]), before)
        self.assertFalse(os.path.exists(os.path.join(self.inbox, "pulse-state.json")))

    def test_rate_limit_one_pulse_per_task_per_2h(self):
        tp.cmd_run(self.cfg, self.line(30), "tm-x")
        (r,) = tp.cmd_run(self.mkcfg(now=NOW + 3600), self.line(50), "tm-x")
        self.assertEqual(r["status"], "rate-limited")
        (r,) = tp.cmd_run(self.mkcfg(now=NOW + 3 * 3600), self.line(50), "tm-x")
        self.assertEqual(r["status"], "applied")
        self.assertEqual(self.dash()["data"]["reported"]["pct"], 50)


class RejectOrphanCase(Base):
    def test_reused_id_stamp_mismatch_is_an_orphan_and_untouched(self):
        v = self.dash()["version"]
        bad = "0101" if self.stamp != "0101" else "0202"
        (r,) = tp.cmd_run(self.cfg, f"Task: ar-T01@{bad} | done: x | pct: 90", "tm-x")
        self.assertEqual(r["status"], "orphan")
        self.assertIn("reused id", r["detail"])
        self.assertEqual(self.dash()["version"], v)
        self.assertEqual(self.jl("task-pulse-orphans.jsonl")[0]["task"], "ar-T01")

    def test_unknown_closed_and_older_than_task_are_orphans(self):
        (r,) = tp.cmd_run(self.cfg, f"Task: ar-T7e@{self.stamp} | pct: 5", "tm-x")
        self.assertEqual(r["status"], "orphan")
        (r,) = tp.cmd_run(self.mkcfg(now=NOW - 7200), "Task: T93 | done: early", "tm-x")      # message before the task existed
        self.assertEqual((r["status"], "older" in r["detail"]), ("orphan", True))
        datg.write_ops(self.proj, [{"op": "close", "id": "ar-T01", "outcome": "done"}])
        (r,) = tp.cmd_run(self.cfg, self.line(), "tm-x")
        self.assertEqual(r["status"], "orphan")
        self.assertEqual(len(self.jl("task-pulse-orphans.jsonl")), 3)

    def test_malformed_tags_go_to_rejects(self):
        (r,) = tp.cmd_run(self.cfg, "Task: ar-T01@1110 done: no pipes", "tm-x")
        self.assertEqual(r["status"], "rejected")
        self.assertEqual(self.jl("task-pulse-rejects.jsonl")[0]["sender"], "tm-x")
        self.assertIn("1 rejects", tp.cmd_status(self.cfg))

    def test_hand_back_cannot_create_a_task(self):
        (r,) = tp.cmd_run(self.cfg, "Task: new:fire-drill | done: x", "tm-x")
        self.assertEqual(r["status"], "rejected")
        self.assertEqual(len(datg.load_state(self.proj)["tasks"]), 1)


class ConflictCase(Base):
    def run_with(self, store_cls):
        orig = tp.store_of
        tp.store_of = lambda cfg: store_cls(root=cfg["dash_root"], actor="test")
        self.addCleanup(lambda: setattr(tp, "store_of", orig))
        return tp.cmd_run(self.cfg, self.line(), "tm-x")

    def test_one_conflict_is_retried_with_a_fresh_read(self):
        class Racy(ds.Store):
            raced = False

            def update(self, coll, doc_id, data, if_version=None, actor=None):
                if not Racy.raced:
                    Racy.raced = True
                    ds.Store.update(self, coll, doc_id, {"owner_note": "raced"}, actor=actor)   # another writer bumps the version
                return ds.Store.update(self, coll, doc_id, data, if_version, actor)
        (r,) = self.run_with(Racy)
        self.assertEqual(r["status"], "applied")
        d = self.dash()["data"]
        self.assertEqual((d["owner_note"], d["reported"]["pct"]), ("raced", 40))     # neither write was lost

    def test_persistent_conflict_reports_and_keeps_the_cursor_before_the_line(self):
        class Always(ds.Store):
            def update(self, coll, doc_id, data, if_version=None, actor=None):
                ds.Store.update(self, coll, doc_id, {"owner_note": "n"}, actor=actor)
                return ds.Store.update(self, coll, doc_id, data, if_version, actor)
        orig = tp.store_of
        tp.store_of = lambda cfg: Always(root=cfg["dash_root"], actor="test")
        self.addCleanup(lambda: setattr(tp, "store_of", orig))
        path = os.path.join(self.inbox, "pm-messages.jsonl")
        with open(path, "w") as f:
            f.write(json.dumps({"ts": tp.iso_s(NOW), "session": "tm-x", "kind": "progress", "text": self.line()}) + "\n")
        (r,) = tp.cmd_run(self.cfg)
        self.assertEqual(r["status"], "conflict")
        self.assertEqual(tp.load_pstate(self.cfg)["offset"], 0)          # a rerun retries the line
        self.assertNotIn("reported", self.dash()["data"])


class FlagCase(unittest.TestCase):
    def test_no_signal_jump_and_stuck(self):
        t = NOW
        f, _ = tp.flags_for({}, 20, None, t)
        self.assertIn("no_signal", f)
        old = {"pct": 20, "at": tp.iso_s(t - 3 * 3600)}
        self.assertNotIn("no_signal", tp.flags_for(old, 30, tp.iso_s(t - 3600), t)[0])           # a signal after the last report
        self.assertIn("no_signal", tp.flags_for(old, 30, tp.iso_s(t - 4 * 3600), t)[0])
        self.assertIn("jump30", tp.flags_for(old, 50, tp.iso_s(t - 60), t)[0])
        band = {"pct": 85, "at": tp.iso_s(t - 3600), "band_since": tp.iso_s(t - 25 * 3600)}
        f, since = tp.flags_for(band, 85, tp.iso_s(t), t)
        self.assertIn("stuck", f)
        self.assertEqual(since, band["band_since"])
        self.assertNotIn("stuck", tp.flags_for({"pct": 85, "at": tp.iso_s(t - 60), "band_since": tp.iso_s(t - 3600)}, 90, tp.iso_s(t), t)[0])


class StaleSignalWipCase(Base):
    def test_stale_means_no_signal_not_no_message(self):
        self.put("T94", created=tp.iso_s(NOW - 80 * 3600), last_signal_at=tp.iso_s(NOW - 50 * 3600), last_msg_at=tp.iso_s(NOW - 60))
        self.put("T95", created=tp.iso_s(NOW - 80 * 3600), last_signal_at=tp.iso_s(NOW - 3600))
        self.put("T96", created=tp.iso_s(NOW - 80 * 3600), parked={"choice": "owner", "at": tp.iso_s(NOW)})
        rows = tp.cmd_stale(self.cfg, 24)
        self.assertEqual([r["id"] for r in rows], ["T94"])             # T93 is only 1 h old, T95 has a signal, T96 is parked

    def test_signals_sets_last_signal_at_from_refs(self):
        self.store.update("items", "T93", {"refs": ["o/r#7", "git:/x:main"]})
        seen = []

        def runner(cmd):
            seen.append(cmd[0])
            if cmd[0] == "gh":
                return json.dumps({"updatedAt": "2026-11-10T10:00:00Z", "statusCheckRollup": [{"completedAt": "2026-11-10T11:30:00Z"}]})
            return "2026-11-10T09:00:00+00:00\n"
        out = tp.cmd_signals(self.mkcfg(runner=runner))
        self.assertEqual(self.dash()["data"]["last_signal_at"], "2026-11-10T11:30:00Z")
        self.assertIn("last_signal_at 2026-11-10T11:30:00Z", out[0])
        self.assertEqual(sorted(set(seen)), ["gh", "git"])

    def test_wip_lists_active_vs_cap_and_quietest(self):
        datg.write_ops(self.proj, [{"op": "add", "kind": "T", "title": "second", "goal": "ar-G01"},
                                   {"op": "update", "id": "ar-T02", "status": "parked"}])
        (row,) = tp.cmd_wip(self.cfg)
        self.assertEqual((row["active"], row["cap"], row["parked"]), (1, 8, 1))
        self.assertEqual(row["quietest"][0]["id"], "ar-T01")


class ComplianceCase(Base):
    def msgs(self, rows):
        with open(os.path.join(self.inbox, "pm-messages.jsonl"), "w") as f:
            for age_days, who, kind, text in rows:
                f.write(json.dumps({"ts": tp.iso_s(NOW - age_days * 86400), "session": who, "kind": kind, "text": text}) + "\n")

    def test_per_pm_counts_window_and_kinds(self):
        self.msgs([(1, "tm-a", "done", "x Task: ar-T01@1110"), (1, "tm-a", "progress", "no tag"), (2, "tm-b", "done", "Task: T93"),
                   (3, "tm-a", "ack", "ok"), (30, "tm-b", "done", "too old, untagged")])
        c = tp.cmd_compliance(self.cfg, 14)
        self.assertEqual({r["pm"]: (r["tagged"], r["untagged"], r["pct"]) for r in c["per_pm"]}, {"tm-a": (1, 1, 50.0), "tm-b": (1, 0, 100.0)})
        self.assertEqual((c["tagged"], c["total"], c["pct"]), (2, 3, 66.7))

    def test_refuse_bar_needs_90_percent_and_the_earliest_date(self):
        late = self.mkcfg(now=tp.epoch("2026-11-25T00:00:00Z"))
        rows = [(1, "tm-a", "done", "Task: T93")] * 10

        def write(extra=0):
            with open(os.path.join(self.inbox, "pm-messages.jsonl"), "w") as f:
                for _, who, kind, text in rows + [(1, "tm-a", "done", "untagged")] * extra:
                    f.write(json.dumps({"ts": tp.iso_s(late["now"] - 86400), "session": who, "kind": kind, "text": text}) + "\n")
        write()
        self.assertTrue(tp.cmd_compliance(late, 14)["refuse_eligible"])
        early = self.mkcfg(now=tp.epoch("2026-11-20T00:00:00Z"))
        with open(os.path.join(self.inbox, "pm-messages.jsonl"), "w") as f:
            for _ in range(10):
                f.write(json.dumps({"ts": tp.iso_s(early["now"] - 86400), "session": "tm-a", "kind": "done", "text": "Task: T93"}) + "\n")
        c = tp.cmd_compliance(early, 14)
        self.assertFalse(c["refuse_eligible"])
        self.assertIn("before 2026-11-24", c["blockers"])
        write(extra=2)                                                    # 10/12 = 83.3%
        c = tp.cmd_compliance(late, 14)
        self.assertEqual((c["refuse_eligible"], c["pct"]), (False, 83.3))


if __name__ == "__main__":
    unittest.main()
