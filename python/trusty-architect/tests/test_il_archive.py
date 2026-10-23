"""il-archive.py: archive Intent Ledger items to memory, then delete (dashboard) or move (per-PM ledger).

Why: owner 2026-10-17 03:56Z: "Decision archiving (any IL archiving) should be scripted."
Covers: retire and prune selection, memory-before-delete order, a failed memory write deletes nothing and
exits non-zero, dry-run writes nothing, note batching, palace mapping (the Architect's palace from the config key
memory_palace, its session from ARCHITECT_SESSION; an unusable config writes nothing), and pm-ledger archive.jsonl
+ log event.
The store lives in a temp dir; the memory CLI is a stub named by IL_ARCHIVE_MEMORY_CMD.
Run: python3 -m pytest scripts/test_il_archive.py
Test: scripts/test_il_archive.py
"""
import json
import os
import subprocess
import sys
import tempfile
import textwrap

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import datg  # noqa: E402
import dashboard_store as ds  # noqa: E402

SCRIPT = os.path.join(SCRIPTS, "il-archive.py")

STUB = textwrap.dedent("""\
    #!/usr/bin/env python3
    import json, os, sys
    if os.environ.get("STUB_FAIL"):
        sys.stderr.write("stub failure\\n"); sys.exit(1)
    with open(os.environ["STUB_LOG"], "a") as f:
        f.write(json.dumps(sys.argv[1:]) + "\\n")
    print("drawer abcd1234")
""")


@pytest.fixture
def env(tmp_path):
    stub = tmp_path / "stub.py"
    stub.write_text(STUB)
    stub.chmod(0o755)
    projects = tmp_path / "projects"
    projects.mkdir()
    config = tmp_path / "architect.json"
    config.write_text("")   # blank: the defaults, whatever the instance's local/architect.json says
    return {"tmp": tmp_path, "root": str(tmp_path / "owner-queue"), "log": tmp_path / "mem.log",
            "projects": str(projects), "stub": str(stub), "config": str(config)}


def run(env, *args, fail=False, extra=None):
    # #10161: the caller's ARCHITECT_SESSION / ARCHITECT_MEMORY_PALACE never reach the script under test
    e = {k: v for k, v in os.environ.items() if k not in ("ARCHITECT_SESSION", "ARCHITECT_MEMORY_PALACE")}
    e.update({"IL_ARCHIVE_MEMORY_CMD": env["stub"], "STUB_LOG": str(env["log"]), "ARCHITECT_CONFIG": env["config"],
              **(extra or {})})
    if fail:
        e["STUB_FAIL"] = "1"
    p = subprocess.run([sys.executable, SCRIPT, "--root", env["root"], "--projects-dir", env["projects"], *args],
                       capture_output=True, text=True, env=e)
    return p.returncode, p.stdout, p.stderr


def mem_calls(env):
    if not env["log"].exists():
        return []
    return [json.loads(x) for x in env["log"].read_text().splitlines()]


def seed(env, docs):
    st = ds.Store(env["root"])
    for i, d in docs.items():
        st.set("items", i, d)
    return st


def done(day, session="tm-architect", **kw):
    return {"status": "done", "kind": "question", "title": f"t{day}", "session": session, "created": f"2026-10-0{day}T00:00:00Z",
            "done_at": f"2026-10-0{day}T12:00:00Z", "answer": "A", "outcome": "ok", **kw}


def ids(st):
    return sorted(d["id"] for d in st.list_docs("items"))


def test_retire_archives_to_memory_then_deletes_oldest_done(env):
    st = seed(env, {"D01": done(3), "D02": done(1), "D03": done(2), "D04": {"status": "open", "title": "x"},
                    "D05": done(4)})
    # 5 used -> 251 free; keep_free 254 -> retire 3 oldest done (D02, D03, D01)
    rc, out, err = run(env, "dashboard", "retire", "--prefix", "D", "--keep-free", "254")
    assert rc == 0, err
    assert ids(st) == ["D04", "D05"]
    calls = mem_calls(env)
    assert len(calls) == 1 and calls[0][0] == "note"
    body = calls[0][1]
    assert "D02 2026-10-01 | tm-architect | t1 | answer: A | outcome: ok" in body.splitlines()
    assert calls[0][calls[0].index("--palace") + 1] == "supervisor"
    tags = [calls[0][i + 1] for i, a in enumerate(calls[0]) if a == "--tag"]
    assert {"archive", "dashboard", "retired-ids"} <= set(tags) and any(t[:2] == "20" for t in tags)
    assert "D01" in out and "supervisor" in out and "abcd1234" in out
    pruned = [json.loads(x) for x in open(os.path.join(env["tmp"], "owner-queue-archive", "pruned.jsonl"))]
    assert sorted(p["id"] for p in pruned) == ["D01", "D02", "D03"]


def test_memory_failure_deletes_nothing_and_exits_nonzero(env):
    st = seed(env, {"D01": done(1), "D02": done(2)})
    rc, out, err = run(env, "dashboard", "retire", "--prefix", "D", "--keep-free", "256", fail=True)
    assert rc != 0
    assert ids(st) == ["D01", "D02"]
    assert not os.path.exists(os.path.join(env["tmp"], "owner-queue-archive", "pruned.jsonl"))


def test_dry_run_writes_nothing(env):
    st = seed(env, {"D01": done(1), "D02": done(2)})
    rc, out, err = run(env, "dashboard", "retire", "--prefix", "D", "--keep-free", "256", "--dry-run")
    assert rc == 0 and "D01" in out
    assert ids(st) == ["D01", "D02"] and mem_calls(env) == []


def test_notes_are_batched_under_9000_chars(env):
    docs = {f"D{n:02x}": done(1, title="x" * 300) for n in range(1, 41)}
    st = seed(env, docs)
    rc, out, err = run(env, "dashboard", "retire", "--prefix", "D", "--keep-free", "256")
    assert rc == 0, err
    calls = mem_calls(env)
    assert len(calls) >= 2 and all(len(c[1]) <= 9000 for c in calls)
    assert sum(len(c[1].splitlines()) for c in calls) == 40
    assert ids(st) == []


def test_version_conflict_keeps_item_and_exits_nonzero(env, monkeypatch):
    seed(env, {"D01": done(1)})
    import importlib.util
    spec = importlib.util.spec_from_file_location("il_archive", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    store = ds.Store(env["root"])
    real = store.delete

    def racing(coll, doc_id, if_version=None, actor=None):
        store.update(coll, doc_id, {"title": "changed"})
        return real(coll, doc_id, if_version, actor)
    monkeypatch.setattr(store, "delete", racing)
    res = mod.delete_items(store, [("items", store.get("items", "D01"))])
    assert res["deleted"] == [] and [f[0] for f in res["failed"]] == ["D01"]
    assert ids(store) == ["D01"]


def test_prune_selects_by_age_status_and_depends_on(env):
    old = "2026-09-01T00:00:00Z"
    st = seed(env, {
        "D01": done(1, done_at=old),
        "D02": {"status": "cancelled", "title": "c", "session": "", "created": old, "cancelled_at": old},
        "D03": {"status": "wont_do", "title": "w", "session": "", "created": old, "wont_do": {"at": old, "note": "n"}},
        "D04": done(1, done_at="2099-01-01T00:00:00Z"),
        "D05": done(1, done_at=old, depends_on=["D06"]),
        "D06": {"status": "open", "title": "dep", "created": old},
        "D07": done(1, done_at=old, depends_on=["D01"]),
        "D08": {"status": "open", "title": "o", "created": old},
    })
    rc, out, err = run(env, "dashboard", "prune", "--days", "7", "--dry-run")
    assert rc == 0, err
    listed = out.splitlines()[0]
    assert "dry-run" in listed and all(i in listed for i in ("D01", "D02", "D03", "D07"))
    assert not any(i in listed for i in ("D04", "D05", "D06", "D08"))
    rc, out, err = run(env, "dashboard", "prune", "--days", "7")
    assert rc == 0, err
    assert ids(st) == ["D04", "D05", "D06", "D08"]
    assert "pruned" in {t for c in mem_calls(env) for t in c}


def test_project_session_uses_palace_from_project_file(env):
    with open(os.path.join(env["projects"], "mcp-services.md"), "w") as f:
        f.write("# tm-mcp-services\n- trusty-memory palace: mcp-services\n")
    with open(os.path.join(env["projects"], "mpm-utils.md"), "w") as f:
        f.write("- trusty-memory palace: none listed (rulings in palace supervisor)\n")
    st = seed(env, {"D01": done(1, session="tm-mcp-services-02"), "D02": done(1, session="tm-mpm-utils-01"),
                    "D03": done(1, session="")})
    rc, out, err = run(env, "dashboard", "retire", "--prefix", "D", "--keep-free", "256")
    assert rc == 0, err
    by_palace = {c[c.index("--palace") + 1]: c[1] for c in mem_calls(env)}
    assert by_palace["mcp-services"].startswith("D01 ")
    assert by_palace["supervisor"].count("\n") == 1   # D02 and D03 both fall back
    assert "mcp-services" in out


def test_architect_palace_comes_from_config_and_session_from_env(env):
    # #10161: neither the palace nor the Architect's session name is built into the script
    with open(env["config"], "w") as f:
        json.dump({"memory_palace": "arch-palace"}, f)
    with open(os.path.join(env["projects"], "boss.md"), "w") as f:
        f.write("- trusty-memory palace: boss-proj\n")
    st = seed(env, {"D01": done(1, session="tm-boss"), "D02": done(1, session=""),
                    "D03": done(1, session="tm-nofile-01")})
    rc, out, err = run(env, "dashboard", "retire", "--prefix", "D", "--keep-free", "256",
                       extra={"ARCHITECT_SESSION": "tm-boss"})
    assert rc == 0, err
    calls = mem_calls(env)
    assert [c[c.index("--palace") + 1] for c in calls] == ["arch-palace"]   # tm-boss is the Architect here
    assert len(calls[0][1].splitlines()) == 3 and ids(st) == []


def test_unusable_config_archives_and_deletes_nothing(env):
    with open(env["config"], "w") as f:
        f.write("{not json")
    st = seed(env, {"D01": done(1)})
    rc, out, err = run(env, "dashboard", "retire", "--prefix", "D", "--keep-free", "256")
    assert rc == 2 and "not valid JSON" in err, (rc, err)
    assert mem_calls(env) == [] and ids(st) == ["D01"]


def make_ledger(tmp_path):
    """th ledger whose archive.jsonl holds th-D01 and th-T01 closed 2026-09-01 and th-D02 closed just now."""
    proj = str(tmp_path / "proj")
    os.makedirs(proj)
    datg.op_init(proj, "th")
    datg.write_ops(proj, [
        {"op": "add", "kind": "G", "title": "goal one"},
        {"op": "add", "kind": "D", "title": "old decision", "goal": "th-G01"},
        {"op": "add", "kind": "D", "title": "fresh decision", "goal": "th-G01"},
        {"op": "add", "kind": "T", "title": "old task", "goal": "th-G01"},
        {"op": "add", "kind": "D", "title": "still open", "goal": "th-G01"},
    ])
    datg.write_ops(proj, [{"op": "update", "id": "th-D01", "answer": "yes"},
                          {"op": "update", "id": "th-D02", "answer": "no"},
                          {"op": "close", "id": "th-D01", "outcome": "done"},
                          {"op": "close", "id": "th-T01", "outcome": "shipped"},
                          {"op": "close", "id": "th-D02", "outcome": "done"}])
    ap = os.path.join(datg.datg_dir(proj), "archive.jsonl")
    lines = [json.loads(x) for x in open(ap)]
    for rec in lines:   # age D01 and T01 to 30 days ago, leave D02 fresh
        if rec["item"]["id"] in ("th-D01", "th-T01"):
            rec["closed_at"] = "2026-09-01T00:00Z"
    with open(ap, "w") as f:
        f.write("".join(json.dumps(r) + "\n" for r in lines))
    return proj


def archive_ids(proj):
    return sorted(json.loads(x)["item"]["id"] for x in open(os.path.join(datg.datg_dir(proj), "archive.jsonl")))


def test_pm_ledger_prune_reads_archive_jsonl_and_removes_old_lines(env):
    proj = make_ledger(env["tmp"])
    assert archive_ids(proj) == ["th-D01", "th-D02", "th-T01"]
    with open(os.path.join(env["projects"], "proj.md"), "w") as f:
        f.write("- trusty-memory palace: proj-palace\n")
    sp = datg.paths(proj)[1]
    state_before = open(sp).read()
    rc, out, err = run(env, "pm-ledger", "prune", "--project", proj, "--days", "7")
    assert rc == 0, err
    assert archive_ids(proj) == ["th-D02"]            # old lines left archive.jsonl, the fresh one stayed
    assert open(sp).read() == state_before             # state.json is not touched
    assert json.loads(open(datg.paths(proj)[2]).read().splitlines()[-1])["ev"] == "update"
    calls = mem_calls(env)
    assert calls and calls[0][calls[0].index("--palace") + 1] == "proj-palace"
    assert "th-D01" in calls[0][1] and "th-T01" in calls[0][1] and "th-D02" not in calls[0][1]
    kept = [f for f in os.listdir(datg.datg_dir(proj)) if f.startswith("archive-") and f.endswith(".jsonl")]
    assert len(kept) == 1                              # removed lines are appended, not dropped (owner D2b)
    kept_ids = sorted(json.loads(x)["item"]["id"] for x in open(os.path.join(datg.datg_dir(proj), kept[0])))
    assert kept_ids == ["th-D01", "th-T01"]


def test_pm_ledger_without_palace_keeps_archive_jsonl(env):
    proj = make_ledger(env["tmp"])
    rc, out, err = run(env, "pm-ledger", "prune", "--project", proj)
    assert rc == 0, err
    assert mem_calls(env) == [] and "no palace" in out
    assert archive_ids(proj) == ["th-D01", "th-D02", "th-T01"]


def test_pm_ledger_memory_failure_leaves_archive_intact(env):
    proj = make_ledger(env["tmp"])
    with open(os.path.join(env["projects"], "proj.md"), "w") as f:
        f.write("- trusty-memory palace: proj-palace\n")
    ap = os.path.join(datg.datg_dir(proj), "archive.jsonl")
    before = open(ap).read()
    rc, out, err = run(env, "pm-ledger", "prune", "--project", proj, fail=True)
    assert rc != 0 and open(ap).read() == before


def test_pm_ledger_dry_run_writes_nothing(env):
    proj = make_ledger(env["tmp"])
    ap = os.path.join(datg.datg_dir(proj), "archive.jsonl")
    before = open(ap).read()
    rc, out, err = run(env, "pm-ledger", "prune", "--project", proj, "--dry-run")
    assert rc == 0 and open(ap).read() == before and "th-D01" in out and mem_calls(env) == []


def test_pm_ledger_keep_file_failure_leaves_archive_intact(env):
    """Fail closed: if archive-<YYYYMM>.jsonl cannot be written, archive.jsonl is not rewritten."""
    proj = make_ledger(env["tmp"])
    with open(os.path.join(env["projects"], "proj.md"), "w") as f:
        f.write("- trusty-memory palace: proj-palace\n")
    d = datg.datg_dir(proj)
    ap = os.path.join(d, "archive.jsonl")
    before = open(ap).read()
    from datetime import datetime, timezone
    os.mkdir(os.path.join(d, f"archive-{datetime.now(timezone.utc).strftime('%Y%m')}.jsonl"))  # a dir blocks the append
    rc, out, err = run(env, "pm-ledger", "prune", "--project", proj)
    assert rc == 1 and open(ap).read() == before
