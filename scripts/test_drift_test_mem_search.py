#!/usr/bin/env python3
"""Selftest of the memory/search drift test; no daemon, no live palace.

Why: the drift script talks to live MCP servers; these cases pin its pure
math, its palace guard and its failure paths against fakes. What: every
`test_*` function runs, each prints PASS or FAIL, and any FAIL exits 1. Set
DRIFT_SCRIPT to run the cases against another copy of the script.
Test: `python3 -I scripts/test_drift_test_mem_search.py`.
"""
import importlib.util, io, json, os, subprocess, sys, time, traceback
from contextlib import redirect_stderr, redirect_stdout
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.environ.get("DRIFT_SCRIPT", os.path.join(HERE, "drift_test_mem_search.py"))
_spec = importlib.util.spec_from_file_location("drift_under_test", SCRIPT)
M = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(M)

STALE = M.TEST_PREFIX + "20260101t000000z-abcdef"


def fake_mcp(cfg):
    """A stand-in for Mcp: in-memory palaces, guard enforced like Mcp.call."""

    class Fake:
        def __init__(self, cmd):
            self.server, self.closed = {"version": "t"}, False
            self.palaces = {k: dict(v) for k, v in cfg.get("palaces", {}).items()}
            cfg.setdefault("inst", []).append(self)

        def call(self, name, args, timeout=None):
            if name in cfg.get("raise", {}):
                raise cfg["raise"][name]
            pal = args.get("palace") or args.get("palace_id") or args.get("name")
            if name in M.GUARDED_TOOLS and pal not in getattr(self, "guard").owned:
                raise PermissionError(f"fake: {name} on unowned {pal!r}")
            if name == "palace_list":
                return {"palaces": sorted(self.palaces)}, 1.0, None
            if name == "palace_create":
                self.palaces[pal] = {}
            if name == "room_list":
                return {"rooms": []}, 1.0, None
            if name == "memory_list":
                return {"drawers": [{"drawer_id": k, "content": v} for k, v in self.palaces[pal].items()]}, 1.0, None
            if name == "memory_remember":
                if args["text"] in cfg.get("fail_texts", ()):
                    return None, 1.0, "seed boom"
                did = f"t{len(self.palaces[pal])}"
                self.palaces[pal][did] = args["text"]
                return {"drawer_id": did}, 1.0, None
            if name == "memory_recall":
                return {"results": [{"drawer_id": k} for k in self.palaces.get(pal, {})]}, 1.0, None
            if name == "palace_delete":
                self.palaces.pop(pal, None)
            return {}, 1.0, None

        def close(self):
            self.closed = True

    return Fake


def with_fake(cfg, fn):
    old, M.Mcp = M.Mcp, fake_mcp(cfg)
    err = io.StringIO()
    try:
        with redirect_stderr(err):
            return fn(), err.getvalue()
    finally:
        M.Mcp = old


def new_info():
    return {"versions": {}, "indexes": {}, "palaces": {}, "warmup_ms": {}}


def test_pure():
    assert M.pct([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 50) == 5 and M.pct([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 95) == 10
    assert M.pct([7], 95) == 7 and M.pct([], 50) is None
    sc = M.score_query(["a", "x/b.rs", "c"], ["b.rs", "zzz"], M.path_match)
    assert sc == {"r5": 0.5, "r10": 0.5, "rr": 0.5}, sc
    assert M.search_degraded({"meta": {"embedder_error": "x"}, "results": []}) == ["embedder_error"]
    assert "lexical_only" in M.search_degraded({"meta": {}, "results": [{"match_reason": "bm25"}]})
    base = {"search": {"n": 5, "recall10": 0.9, "p95_ms": 100, "degraded_share": 0.0}}
    bad = {"search": {"n": 5, "recall10": 0.8, "p95_ms": 200, "degraded_share": 0.2}}
    assert len(M.compare(bad, base, "t")) == 3 and M.compare(base, base, "t") == []
    warm = {"search": dict(base["search"], embed_cold_share=0.0)}
    sk = []
    assert len(M.compare({"search": dict(bad["search"], embed_cold_share=0.5)}, warm, "t", sk)) == 2
    assert "differs by more than" in sk[0], sk
    assert M.cold_share([{"embed_ms": 0}, {"embed_ms": 12}]) == 0.5 and M.cold_share([{}]) is None
    assert M.best_mode(["docs/a.md", "b.MD"]) == "text" and M.best_mode(["docs/a.md", "x.rs"]) == "code"
    assert M.dup_count(["A b", "a  B", "c"]) == 1
    r = {"date": "2000-01-01", "quote": "fixture text alpha"}
    assert M.ruling_intact(r, ['On 2000-01-01 Alice: "fixture text alpha".'])
    resp = {"results": [], "rulings_degraded": True, "vector_unavailable": False}
    assert M.memory_degraded(resp) == [] and M.memory_degraded(dict(resp, lexical_only=True)) == ["lexical_only"]
    assert len(M.compare({"search:as-called": bad["search"]}, base, "t")) == 3, "old `search` lane skipped"


def test_guard():
    g, live = M.PalaceGuard(), {"supervisor", "cto"}
    for op, name, ex in [("claim", "supervisor", live), ("claim", "drift-dream-test-x", live),
                         ("claim", STALE, live | {STALE}), ("check", "supervisor", None)]:
        try:
            g.claim(name, ex) if op == "claim" else g.check(name)
        except PermissionError:
            continue
        raise AssertionError(f"guard {op} {name!r} was not refused")
    fresh = g.new_name()
    g.claim(fresh, live)
    g.check(fresh)
    for obj, tool in [(type("X", (), {})(), "memory_recall"), (type("Y", (), {"allowed": M.DREAM_TOOLS})(), "palace_dream"),
                      (type("Z", (), {"allowed": M.DREAM_TOOLS, "guard": g})(), "palace_delete")]:
        try:
            M.Mcp.call(obj, tool, {"palace": "cto", "palace_id": "cto"})
        except PermissionError:
            continue
        raise AssertionError(f"{tool} on a live palace was not refused")


def test_baseline_payload():
    brow = {"ts": "t", "versions": {}, "palaces": {}, "golden_sha256": {}, "thresholds": {},
            "embed_cold_share": {}, "metrics": {}, "indexes": {"ix": {"chunks": 1, "root": "/" + "Users/x/p"}}}
    bp = M.baseline_payload(brow)
    assert bp["indexes"] == {"ix": {"chunks": 1}} and "root" in brow["indexes"]["ix"], bp


# ---- #11351 HIGH 1: no silent absent lane


def test_load_golden_missing():
    p = "/nonexistent/golden.private.json"
    with redirect_stderr(io.StringIO()):
        g, _ = M.load_golden([p])
    assert g.get("missing") == [p], f"missing golden file not recorded: {g}"


def test_compare_absent_lane():
    ref = {"memory": {"n": 4, "recall10": 1.0, "p95_ms": 10, "degraded_share": 0.0}}
    out = M.compare({"memory": {"n": 0}}, ref, "vs-baseline")
    assert len(out) == 1 and "n=0" in out[0], f"n=0 lane the baseline expects is not a finding: {out}"
    assert len(M.compare({}, ref, "vs-baseline")) == 1, "absent lane the baseline expects is not a finding"


def test_coverage_findings():
    golden = {"search": [{"id": "s"}], "memory": [{"palace": "supervisor"}], "missing": ["/x/golden.private.json"]}
    mt = {"search:as-called": {"n": 0}, "search:best-mode": {"n": 1}, "memory": {"n": 1}}
    out = M.coverage_findings(golden, mt, "cto", None)
    assert any("golden file missing" in f for f in out), out
    assert any("search:as-called" in f and "n=0" in f for f in out), out
    assert any("dream" in f and "not a golden source palace" in f for f in out), out
    full = {"search:as-called": {"n": 1}, "search:best-mode": {"n": 1}, "memory": {"n": 1}}
    assert M.coverage_findings(dict(golden, missing=[]), full, None, None) == []


# ---- #11351 HIGH 2: seed failures reported, recall over the full set


def test_memory_partial_seed():
    cfg = {"palaces": {"supervisor": {"a": "drawer A", "b": "drawer B"}}, "fail_texts": {"drawer B"}}
    golden = {"search": [], "memory": [{"id": "q1", "palace": "supervisor", "query": "q", "expected": ["a", "b"]}]}
    rows, info = [], new_info()
    with_fake(cfg, lambda: M.run_memory(golden, rows, info, "/nonexistent"))
    assert len(rows) == 1 and rows[0]["r10"] == 0.5, f"recall computed over a shrunk set: {rows}"
    f = M.memory_findings(info)
    assert any("failed to seed" in x for x in f), f"seed_skipped not a finding: {f}"
    assert any("missing from the seeded palace" in x for x in f), f"missing targets not a finding: {f}"


# ---- #11351 HIGH 3: no silent test-palace or process leak


def test_memory_delete_on_raise():
    golden = {"search": [], "memory": [{"id": "q1", "palace": "supervisor", "query": "q", "expected": ["a"]}]}
    cfg = {"palaces": {"supervisor": {"a": "A"}}, "raise": {"room_list": RuntimeError("room_list boom")}}
    info = new_info()
    try:
        _, err = with_fake(cfg, lambda: M.run_memory(golden, [], info, "/nonexistent"))
    except RuntimeError:
        err = None
    assert cfg["inst"][0].closed, "client not closed after run_memory raised"
    assert info.get("deletes"), "delete not recorded after run_memory raised"
    cfg2 = {"palaces": {"supervisor": {"a": "A"}}, "raise": {"palace_delete": RuntimeError("delete boom")}}
    info2, err2 = new_info(), ""
    try:
        _, err2 = with_fake(cfg2, lambda: M.run_memory(golden, [], info2, "/nonexistent"))
    except RuntimeError as e:
        raise AssertionError(f"a failing delete aborted the run: {e}; closed={cfg2['inst'][0].closed}")
    assert cfg2["inst"][0].closed, "client not closed after a delete raised"
    pal = next(iter(info2["deletes"]))
    assert "delete boom" in info2["deletes"][pal]["error"] and pal in err2, (info2, err2)
    assert any("not verified" in x for x in M.memory_findings(info2))


def test_stale_sweep():
    now = datetime.now(timezone.utc)
    young = M.TEST_PREFIX + (now - timedelta(minutes=10)).strftime("%Y%m%dt%H%M%Sz") + "-abc123"
    keep = {"supervisor", young, "drift-dream-test-x", M.TEST_PREFIX + "20260101t000000z-zzzzzz", "cto"}
    cfg = {"palaces": {n: {} for n in keep | {STALE}}}
    info = new_info()
    with_fake(cfg, lambda: M.run_memory({"search": [], "memory": []}, [], info, "/nonexistent"))
    left = set(cfg["inst"][0].palaces)
    assert STALE not in left, f"stale test palace not swept: {left}"
    assert keep <= left, f"sweep touched a palace outside the guard: {keep - left}"
    assert list(info.get("stale_swept", {})) == [STALE], info.get("stale_swept")
    assert any("stale test palace" in x for x in M.memory_findings(info)), "sweep not reported"


def test_run_search_closes():
    cfg = {"raise": {"search": RuntimeError("search boom")}}
    try:
        with_fake(cfg, lambda: M.run_search({"search": [{"index": "ix"}], "memory": []}, [], new_info()))
    except RuntimeError:
        pass
    assert cfg["inst"][0].closed, "run_search left its client open after a raise"


def test_mcp_init_kills_child():
    made, real = [], subprocess.Popen

    def rec(*a, **k):
        p = real(*a, **k)
        made.append(p)
        return p

    subprocess.Popen = rec
    try:
        M.Mcp(["sh", "-c", "exec 1>&-; sleep 30"])
    except RuntimeError:
        pass
    finally:
        subprocess.Popen = real
    time.sleep(0.2)
    alive = made[0].poll() is None
    if alive:
        made[0].kill()
    assert not alive, "initialize failure left the server child running"


def test_close_kills():
    calls = []

    class P:
        def poll(self):
            return None

        def terminate(self):
            calls.append("terminate")

        def wait(self, timeout=None):
            calls.append("wait")
            if "kill" not in calls:
                raise subprocess.TimeoutExpired("srv", timeout)

        def kill(self):
            calls.append("kill")

    M.Mcp.close(type("C", (), {"p": P()})())
    assert calls[:3] == ["terminate", "wait", "kill"], f"close did not kill a child that ignored terminate: {calls}"


def main():
    fails = 0
    for name, fn in [(k, v) for k, v in globals().items() if k.startswith("test_") and callable(v)]:
        try:
            with redirect_stdout(io.StringIO()):
                fn()
            print(f"PASS {name}")
        except Exception as e:
            fails += 1
            print(f"FAIL {name}: {type(e).__name__}: {e}")
            if os.environ.get("DRIFT_TRACE"):
                traceback.print_exc()
    print("selftest ok" if not fails else f"selftest FAILED: {fails} case(s)")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
