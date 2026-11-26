#!/usr/bin/env python3
"""Read-only accuracy and latency drift test for trusty-search and trusty-memory.

Why: an every-two-weeks check is needed that retrieval has not regressed. This runs a fixed golden query set through the
live MCP surfaces (the same stdio servers Claude Code uses) and compares the
result with a stored baseline and with the previous run.

What: for each lane it computes recall@5, recall@10, MRR, p50/p95 latency,
the degraded share and the embed cold share, appends one JSONL row to the
results file, and prints findings when a metric crosses a threshold. Search
has two lanes: `search:as-called` (no `mode`, as agents call it) and
`search:best-mode` (`mode` by target type, see best_mode). The memory lane
never recalls a live palace, because recall raises `access_count` (#11348):
it seeds a guarded test palace per source palace, recalls there scoped to
the `default` wing so the daemon's user-scope rulings leg stays off, and
deletes it in a `finally` block (see run_memory). A drawer that fails to seed
or a recall hit from another palace aborts the run unrecorded, exit 3
(#11815). `--lanes` runs one lane alone. Search uses only read-only tools;
there is no reindex or daemon-restart path in this file. An absent lane, a
missing golden file, a failed seed or an unverified delete is a finding, so
"no findings" means every expected lane ran (#11351).

Test: `python3 -I scripts/test_drift_test_mem_search.py` (also `--selftest`;
no daemon). A live run is `python3 -I scripts/drift_test_mem_search.py`.
"""
import argparse, hashlib, json, os, runpy, sys, time
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)  # `-I` drops the script dir from sys.path; the split modules live beside this file

from drift_mem_core import *  # noqa: E402,F401,F403  (re-exported for the test module)
from drift_mem_core import (DREAM_TOOLS, RULINGS_DEGRADED, SEARCH_LANES, TH_COLD_TOL, TH_DEGRADED,  # noqa: E402
                            TH_P95_X, TH_RECALL10, TEST_NAME_RE, Mcp, PalaceGuard, RunAborted, best_mode,
                            check_own, check_scope_covers, compare, list_all, map_expected, memory_degraded,
                            path_match, recall, score_query, search_degraded, summarize)
from drift_mem_dream import *  # noqa: E402,F401,F403
from drift_mem_dream import dream_findings, dream_pass, print_dream  # noqa: E402

sys.path.insert(0, os.path.join(HERE, "lib"))
from install_paths import InstallPathsError, tools_dir  # noqa: E402

DEF_PUBLIC = os.path.join(HERE, "..", "testdata", "drift", "mem-search", "golden.public.json")
DEF_BASELINE = os.path.join(HERE, "..", "testdata", "drift", "mem-search", "baseline.json")
PRIVATE_NAME, RESULTS_NAME = "golden.private.json", "runs.jsonl"
LANES = ("search", "memory")


def drift_dir():
    """`<config_dir>/.trusty-tools/drift/mem-search`; raises InstallPathsError on a broken install.toml (#11821)."""
    return os.path.join(tools_dir(), "drift", "mem-search")
REMEMBER_TYPES = {"Ruling", "Decision", "Status", "Turn", "Reference"}
# #11902: the daemon's secret-gate refusal text (memory_remember); `force` does not bypass it.
SECRET_GATE = "likely secret/credential token"


def load_golden(paths):
    """Merge the golden files; a missing one is listed in `g["missing"]` (#11351)."""
    g, digests = {"search": [], "memory": [], "missing": []}, {}
    for p in paths:
        if not os.path.exists(p):
            g["missing"].append(p)
            continue
        raw = open(p, "rb").read()
        digests[os.path.basename(p)] = hashlib.sha256(raw).hexdigest()[:12]
        d = json.loads(raw)
        g["search"] += d.get("search", [])
        g["memory"] += d.get("memory", [])
    return g, digests


def run_search(golden, rows, info):
    """Both search lanes; as-called runs first, so best-mode reuses its warm embeds."""
    s = Mcp(["trusty-search", "serve"])
    try:
        info["versions"]["trusty-search"] = s.server.get("version")
        for ix in sorted({q["index"] for q in golden["search"]}):
            _, ms, err = s.call("search", {"query": "readme", "index_id": ix, "top_k": 1, "compact": True})
            info["warmup_ms"][f"search:{ix}"] = round(ms)
            st, _, _ = s.call("index_status", {"index_id": ix})
            if st:
                info["indexes"][ix] = {"chunks": st.get("chunk_count"), "disk_bytes": st.get("disk_bytes"),
                                       "root": st.get("root_path")}
        for q in golden["search"]:
            for lane in SEARCH_LANES:
                args = {"query": q["query"], "index_id": q["index"], "top_k": 10, "compact": True}
                if lane == "search:best-mode":
                    args["mode"] = best_mode(q["expected"])
                resp, ms, err = s.call("search", args)
                ranked = []
                for r in (resp or {}).get("results", []):
                    if r["path"] not in ranked:
                        ranked.append(r["path"])
                why = ["error"] if err or resp is None else search_degraded(resp)
                sc = score_query(ranked, q["expected"], path_match)
                emb = ((resp or {}).get("meta", {}).get("timings", {}) or {}).get("embed_ms")
                rows.append({"lane": lane, "id": q["id"], "ms": ms, "degraded": bool(why), "why": why,
                             "embed_ms": emb, "mode": args.get("mode"), **sc})
    finally:
        s.close()


def metrics(rows):
    out = {lane: summarize([r for r in rows if r["lane"] == lane]) for lane in SEARCH_LANES + ("memory",)}
    out["memory:dated-rulings"] = summarize([r for r in rows if r["lane"] == "memory" and r.get("kind") == "dated_ruling"])
    # overall = what agents get today: as-called search plus memory.
    out["overall"] = summarize([r for r in rows if r["lane"] in ("search:as-called", "memory")])
    return out


def seed_test_palace(m, guard, source, existing):
    """Create a guarded test palace and copy `source`'s drawers into it.

    Returns (palace, source drawers, live id -> test id map, info). The source
    is only listed (memory_list), never recalled. #11815: a drawer that fails
    to seed raises RunAborted naming every such drawer, so a partial seed is
    never scored. #11902: the seed is a verbatim copy of drawers the live
    palace already holds, so a drawer the secret gate refuses is re-sent once
    with `allow_secret_like` and listed in info["seed_secret_like"]; every
    other drawer keeps the gate. Test: `test_partial_seed_aborts_and_names_drawers`,
    `test_secret_like_drawer_seeds_verbatim_and_is_listed`."""
    pal, t0 = guard.new_name(), time.perf_counter()
    guard.claim(pal, existing)
    _, _, err = m.call("palace_create", {"name": pal, "force": True,
                                         "description": "drift-test memory/dream fixture; deleted by the run"})
    if err:
        raise RuntimeError(f"palace_create: {err}")
    src = list_all(m, source)
    idmap, failed, secret_like = {}, [], []
    for d in src.values():
        a = {"palace": pal, "text": d["content"], "force": True}
        if d["_room"]:
            a["room"] = d["_room"]
        if d.get("drawer_type") in REMEMBER_TYPES:
            a["type"] = d["drawer_type"]
        resp, _, err = m.call("memory_remember", a)
        if err and SECRET_GATE in str(err):
            resp, _, err = m.call("memory_remember", dict(a, allow_secret_like=True))
            if not err:
                secret_like.append(d["drawer_id"])
        if err or not resp or not resp.get("drawer_id"):
            failed.append(f"{d['drawer_id']} ({err or 'no drawer_id'})")
            continue
        idmap[d["drawer_id"]] = resp["drawer_id"]
    if failed:
        raise RunAborted(f"{len(failed)} of {len(src)} drawers of {source} did not seed into {pal}: "
                         + "; ".join(failed))
    return pal, src, idmap, {"test_palace": pal, "source_palace": source, "seeded": len(idmap),
                             "seed_secret_like": secret_like, "seed_s": round(time.perf_counter() - t0, 1)}


def delete_test_palace(m, guard, pal):
    """Delete an owned test palace and verify: no error, not listed, no data dir."""
    guard.check(pal)
    pi, _, _ = m.call("palace_info", {"palace": pal})
    ddir = (pi or {}).get("data_dir")
    _, _, err = m.call("palace_delete", {"palace_id": pal, "force": True})
    lst, _, lerr = m.call("palace_list", {})
    still = lerr is not None or pal in set((lst or {}).get("palaces", []))
    return {"error": err, "in_palace_list": still, "data_dir_exists": os.path.exists(ddir) if ddir else None}


def safe_delete(m, guard, pal, info, key):
    """Delete `pal`, record the result in info[key] and print it to stderr, even on raise."""
    try:
        d = delete_test_palace(m, guard, pal)
    except Exception as e:
        d = {"error": f"{type(e).__name__}: {e}", "in_palace_list": None, "data_dir_exists": None}
    info.setdefault(key, {})[pal] = d
    print(f"test palace delete ({key}) {pal}: {d}", file=sys.stderr)
    return d


def sweep_stale(m, guard, existing, info, now=None):
    """Delete `drift-dream-test-*` palaces older than an hour that an earlier run
    leaked; report each in info["stale_swept"]. A name that fails the guard's
    pattern or age check is never touched (#11351). Test: `test_stale_sweep`."""
    now = now or datetime.now(timezone.utc)
    for name in sorted(existing):
        if not isinstance(name, str) or not TEST_NAME_RE.fullmatch(name):
            continue
        try:
            guard.adopt_stale(name, now)
        except PermissionError as e:
            info.setdefault("stale_kept", {})[name] = str(e)
            continue
        safe_delete(m, guard, name, info, "stale_swept")
        guard.owned.discard(name)


def run_memory(golden, rows, info, private_path, dream_source=None):
    """Memory lane on seeded test palaces (one per source palace), never a live one.

    Why: memory_recall raises a served drawer's access_count (#11348), so a
    recall on a live palace changes the data it measures. What: sweep stale
    test palaces, then per source palace seed a guarded test palace, warm it
    up, recall the golden queries over the full expected set through the live
    -> test id map, and (when `dream_source` matches) run the dream pass on
    the same seeding. Every owned palace is deleted in `finally`, each result
    recorded and printed even when the lane raised, and the client always
    closed. #11815: every recall is wing-scoped (`recall`), which skips the
    daemon's user-scope rulings leg; a served hit whose id is not in the test
    palace raises RunAborted before any further recall. Test:
    `test_recall_leaves_live_palaces_untouched`, `test_foreign_hit_aborts_the_lane`,
    `test_memory_delete_on_raise`, `test_stale_sweep`.
    """
    guard = PalaceGuard()
    m = Mcp(["trusty-memory", "serve", "--stdio"])
    m.allowed, m.guard = DREAM_TOOLS, guard
    info["memory_palaces"], dream = {}, None
    try:
        info["versions"]["trusty-memory"] = m.server.get("version")
        lst, _, err = m.call("palace_list", {})
        existing = set((lst or {}).get("palaces", []))
        if err or not existing:
            raise RuntimeError(f"palace_list failed: {err}")
        sweep_stale(m, guard, existing, info)
        for source in sorted({q["palace"] for q in golden["memory"]}):
            pi, _, _ = m.call("palace_info", {"palace": source})
            info["palaces"][source] = {"drawers": (pi or {}).get("drawer_count")}
            pal, src, idmap, pinfo = seed_test_palace(m, guard, source, existing)
            info["memory_palaces"][source] = pinfo
            own = set(list_all(m, pal))
            check_scope_covers(m, pal, own | set(idmap.values()))
            resp, ms, _ = recall(m, pal, "warm-up", 1)
            check_own(resp, own, pal)
            info["warmup_ms"][f"memory:{source}"] = round(ms)
            unmapped = partial = 0
            queries = [q for q in golden["memory"] if q["palace"] == source]
            for q in queries:
                exp, missing = map_expected(q["expected"], idmap)
                if missing == len(exp):
                    unmapped += 1
                    continue
                partial += bool(missing)
                resp, ms, err = recall(m, pal, q["query"], 10)
                check_own(resp, own, pal)
                ranked = [r["drawer_id"] for r in (resp or {}).get("results", [])]
                why = ["error"] if err or resp is None else memory_degraded(resp)
                rows.append({"lane": "memory", "id": q["id"], "ms": ms, "degraded": bool(why), "why": why,
                             "rulings_degraded": bool(resp and resp.get(RULINGS_DEGRADED)),
                             "kind": q.get("kind"), **score_query(ranked, exp, lambda a, b: a == b)})
            targets = {e for q in queries for e in q["expected"]}
            pinfo.update(unmapped_queries=unmapped, partial_queries=partial,
                         golden_targets_missing=len(targets - set(idmap)))
            if source == dream_source:
                dream = dream_pass(m, pal, src, idmap, golden, private_path, source)
                dream.update(pinfo)
        return dream
    finally:
        try:
            for pal in sorted(guard.owned):
                d = safe_delete(m, guard, pal, info, "deletes")
                if dream and dream.get("test_palace") == pal:
                    dream["delete"] = d
        finally:
            m.close()


def baseline_payload(row):
    """The committed baseline: index ids and counts only. Why: the repo is public, and an index
    `root` is an absolute home path (#df-T7c). runs.jsonl lives outside the repo and keeps root.
    Test: `test_baseline_payload`."""
    b = {k: row[k] for k in ("ts", "versions", "indexes", "palaces", "golden_sha256", "thresholds",
                             "embed_cold_share", "metrics")}
    b["indexes"] = {ix: {k: v for k, v in st.items() if k != "root"} for ix, st in row["indexes"].items()}
    return b


def memory_findings(info):
    """Memory-lane findings: shrunk targets, unverified deletes. Seed failures and
    foreign hits abort the run instead (#11815)."""
    out = []
    for src, p in info.get("memory_palaces", {}).items():
        if p.get("golden_targets_missing"):
            out.append(f"memory {src}: {p['golden_targets_missing']} golden targets are missing from the seeded palace")
        if p.get("partial_queries"):
            out.append(f"memory {src}: {p['partial_queries']} golden queries have only part of their targets "
                       "seeded (scored over the full set, unseeded targets count as misses)")
        if p.get("unmapped_queries"):
            out.append(f"memory {src}: {p['unmapped_queries']} golden queries have no seeded target")
    for pal, d in info.get("stale_swept", {}).items():
        out.append(f"memory: stale test palace {pal} from an earlier run was found and swept: {d}")
    for key in ("stale_swept", "deletes"):
        for pal, d in info.get(key, {}).items():
            if d.get("error") or d.get("in_palace_list") or d.get("data_dir_exists") is not False:
                out.append(f"memory: test palace delete not verified: {pal} {d}")
    return out


def coverage_findings(golden, mt, dream_source, dream, lanes=LANES):
    """Findings for a lane that did not run: a missing golden file, a selected lane
    the golden set expects with n = 0, or a requested dream that did not run (#11351).
    Test: `test_coverage_findings`, `test_lane_selector_runs_one_lane`."""
    out = [f"golden file missing: {p}" for p in golden.get("missing", [])]
    expect = {lane: "search" in lanes and bool(golden["search"]) for lane in SEARCH_LANES}
    expect["memory"] = "memory" in lanes and bool(golden["memory"])
    for lane, want in expect.items():
        if want and not (mt.get(lane) or {}).get("n"):
            out.append(f"lane {lane}: golden set has queries but the lane ran none (n=0)")
    if dream_source and dream is None:
        srcs = sorted({q["palace"] for q in golden["memory"]})
        why = "not a golden source palace" if dream_source not in srcs else "the dream pass did not complete"
        out.append(f"dream: requested on {dream_source!r} but did not run ({why}; golden palaces: {srcs})")
    return out


def parse_lanes(spec):
    """`--lanes search,memory` -> a tuple in run order; raises ValueError on an unknown lane (#11815)."""
    got = {x.strip() for x in spec.split(",") if x.strip()}
    if not got or got - set(LANES):
        raise ValueError(f"--lanes takes a comma list of {', '.join(LANES)}; got {spec!r}")
    return tuple(x for x in LANES if x in got)


def lane_metrics(mt, lanes):
    """The metric keys a run over `lanes` produces; `overall` needs both lanes (#11815)."""
    keep = (set(SEARCH_LANES) | {"search"} if "search" in lanes else set()) | (
        {"memory", "memory:dated-rulings"} if "memory" in lanes else set())
    if set(LANES) <= set(lanes):
        keep.add("overall")
    return {k: v for k, v in mt.items() if k in keep}


def print_table(mt):
    print(f"{'lane':22} {'n':>3} {'R@5':>6} {'R@10':>6} {'MRR':>6} {'p50ms':>6} {'p95ms':>6} {'degr%':>6} {'cold%':>6}")
    for lane, x in mt.items():
        if not x.get("n"):
            print(f"{lane:22} {0:>3} (no queries ran)")
            continue
        cold = "-" if x["embed_cold_share"] is None else f"{x['embed_cold_share'] * 100:.1f}"
        print(f"{lane:22} {x['n']:>3} {x['recall5']:>6} {x['recall10']:>6} {x['mrr']:>6} {x['p50_ms']:>6} "
              f"{x['p95_ms']:>6} {x['degraded_share'] * 100:>6.1f} {cold:>6}")
    for lane, x in mt.items():
        if x.get("rulings_degraded_share") is not None:
            print(f"rulings_degraded % {lane}: {x['rulings_degraded_share'] * 100:.1f}")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--golden-public", default=DEF_PUBLIC)
    ap.add_argument("--golden-private", default=None, help="default: <drift dir>/golden.private.json")
    ap.add_argument("--baseline", default=DEF_BASELINE)
    ap.add_argument("--results", default=None, help="default: <drift dir>/runs.jsonl")
    ap.add_argument("--write-baseline", action="store_true", help="store this run as the baseline")
    ap.add_argument("--no-record", action="store_true", help="do not append to the results file")
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--dream", action="store_true", help="also run the dream lane on the memory lane's seeding")
    ap.add_argument("--dream-source", default="supervisor", help="palace whose test copy is dreamed")
    ap.add_argument("--lanes", default=",".join(LANES), help="comma list of lanes to run (default: search,memory)")
    a = ap.parse_args(argv)
    if a.selftest:
        runpy.run_path(os.path.join(HERE, "test_drift_test_mem_search.py"), run_name="__main__")
        return 0
    try:
        lanes = parse_lanes(a.lanes)
    except ValueError as e:
        ap.error(str(e))
    if a.dream and "memory" not in lanes:
        ap.error("--dream runs on the memory lane's seeding; add memory to --lanes")
    if a.write_baseline and lanes != LANES:
        ap.error("--write-baseline needs every lane; drop --lanes")
    if a.golden_private is None or a.results is None:
        # #11821 P6a: the results file is appended to, so a broken install.toml refuses the run.
        try:
            ddir = drift_dir()
        except InstallPathsError as e:
            print(f"drift_test_mem_search: refused: {e}", file=sys.stderr)
            return 2
        a.golden_private = a.golden_private or os.path.join(ddir, PRIVATE_NAME)
        a.results = a.results or os.path.join(ddir, RESULTS_NAME)
    golden, digests = load_golden([a.golden_public, a.golden_private])
    rows, info, dream, lane_errors = [], {"versions": {}, "indexes": {}, "palaces": {}, "warmup_ms": {}}, None, []
    if "search" in lanes:
        try:
            run_search(golden, rows, info)
        except Exception as e:
            lane_errors.append(f"search lane failed: {type(e).__name__}: {e}")
    if "memory" in lanes:
        try:
            dream = run_memory(golden, rows, info, a.golden_private, a.dream_source if a.dream else None)
        except RunAborted as e:
            # #11815: a partial seed or a foreign hit is never scored or recorded.
            print(f"drift_test_mem_search: ABORTED, nothing scored or recorded: {e}", file=sys.stderr)
            print("test palace deletes:", info.get("deletes"), file=sys.stderr)
            return 3
        except Exception as e:
            lane_errors.append(f"memory lane failed: {type(e).__name__}: {e}")
    mt = lane_metrics(metrics(rows), lanes)
    prev = None
    if os.path.exists(a.results):
        lines = []
        for n, l in enumerate(open(a.results), 1):
            if not l.strip() or '"lane": "dream"' in l:
                continue
            try:
                lines.append(json.loads(l))
            except ValueError as e:  # #11815: one torn line must not crash a run whose lanes already ran
                print(f"drift_test_mem_search: skipping unparseable {a.results}:{n}: {e}", file=sys.stderr)
        # #11815: the previous run is the last one that covered every selected lane.
        lines = [r for r in lines if set(lanes) <= set(r.get("lanes", LANES))]
        prev = lines[-1] if lines else None
    base = json.load(open(a.baseline)) if os.path.exists(a.baseline) else {}
    findings, skipped = lane_errors + coverage_findings(golden, mt, a.dream_source if a.dream else None, dream,
                                                        lanes), []
    findings += memory_findings(info)
    if base.get("metrics"):
        findings += compare(mt, lane_metrics(base["metrics"], lanes), "vs-baseline", skipped)
    if prev:
        findings += compare(mt, lane_metrics(prev["metrics"], lanes), "vs-previous", skipped)
    ts = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    row = {"ts": ts, "lanes": list(lanes), "golden_sha256": digests, **info,
           "thresholds": {"recall10_drop": TH_RECALL10, "p95_x": TH_P95_X, "degraded_rise": TH_DEGRADED,
                          "cold_share_tol": TH_COLD_TOL},
           "embed_cold_share": {k: v.get("embed_cold_share") for k, v in mt.items()},
           "metrics": mt, "findings": findings, "checks_skipped": skipped,
           "queries": [{k: r.get(k) for k in ("lane", "id", "r5", "r10", "rr", "degraded", "why", "embed_ms", "mode",
                                                   "rulings_degraded")}
                       | {"ms": round(r["ms"])} for r in rows]}
    dfind = []
    if dream:
        dfind = dream_findings(dream, base.get("dream"))
    if not a.no_record:
        os.makedirs(os.path.dirname(a.results), exist_ok=True)
        with open(a.results, "a") as f:
            f.write(json.dumps(row) + "\n")
            if dream:
                f.write(json.dumps({"ts": ts, "lane": "dream", "golden_sha256": digests,
                                    "thresholds": {"recall10_drop": TH_RECALL10}, "dream": dream,
                                    "findings": dfind}) + "\n")
    if a.write_baseline:
        b = baseline_payload(row)
        b["dream"] = base.get("dream")
        if dream:
            b["dream"] = {"ts": ts, **{k: dream.get(k) for k in ("source_palace", "seeded", "before", "after",
                          "drawers", "dream_ms", "golden", "verdict", "max_age_days", "noop_pass")}}
            b["dream"]["rulings"] = {k: v for k, v in dream["rulings"].items() if k != "ids"}  # ids stay private
        os.makedirs(os.path.dirname(a.baseline), exist_ok=True)
        json.dump(b, open(a.baseline, "w"), indent=1)
    print_table(mt)
    print("versions:", info["versions"], "| warmup_ms:", info["warmup_ms"])
    print("golden sha256:", digests)
    print("memory palaces:", {s: {k: p.get(k) for k in ("seeded", "golden_targets_missing",
                                                        "partial_queries", "unmapped_queries")}
                              for s, p in info.get("memory_palaces", {}).items()})
    print("test palace deletes:", info.get("deletes"), "| stale swept:", info.get("stale_swept"))
    for s in skipped:
        print(" skipped:", s)
    if dream:
        print_dream(dream)
    findings += dfind
    print("FINDINGS:" if findings else "no findings" + ("" if base or prev else " (no baseline or previous run to compare)"))
    for f in findings:
        print(" -", f)
    return 2 if findings else 0


if __name__ == "__main__":
    sys.exit(main() or 0)
