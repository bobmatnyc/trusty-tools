"""Dream lane of the memory/search drift test.

Why: split out of drift_test_mem_search.py to keep each file under the 500-SLOC
cap. What: measure the golden set on a seeded test palace, run one dream pass,
measure again, and judge the result. Test:
`python3 -I scripts/test_drift_test_mem_search.py`.
"""
import hashlib, json, re, subprocess

from drift_mem_core import TH_RECALL10, check_own, list_all, map_expected, recall, score_query, summarize

DREAM_TIMEOUT = 1800.0
# max_age_days <= 0 is a no-op guard in engine/dream/semantic.rs, and a seeded
# fixture's drawers are all created "now", so a fixture pass changes nothing
# until the daemon can seed old drawers; dream_findings flags a no-op pass.
DREAM_MAX_AGE_DAYS = 1
DATE_RE = re.compile(r"\b20\d\d-\d\d-\d\d\b")
QUOTE_RE = re.compile(r"\"([^\"\n]{12,200})\"")


def sha(s):
    return hashlib.sha256(s.encode()).hexdigest()


def dup_count(contents):
    """Drawers whose whitespace/case-normalised content repeats another drawer."""
    keys = [sha(" ".join(c.lower().split())) for c in contents]
    return len(keys) - len(set(keys))


def pick_rulings(drawers, n=5):
    """Deterministic pick of `n` drawers carrying a date and a quoted phrase."""
    out = []
    for d in sorted(drawers, key=lambda d: sha(d["drawer_id"])):
        dm, qm = DATE_RE.search(d["content"]), QUOTE_RE.search(d["content"])
        if dm and qm and d.get("drawer_type") in ("Ruling", "Decision", "UserFact", None):
            out.append({"source_palace": d["_src"], "source_drawer_id": d["drawer_id"],
                        "date": dm.group(0), "quote": qm.group(1)})
        if len(out) == n:
            break
    return out


def ruling_intact(r, contents):
    """True when some drawer holds the ruling's date and quote byte-identical."""
    q, dt = r["quote"].encode(), r["date"].encode()
    return any(q in c.encode() and dt in c.encode() for c in contents)


def dream_verdict(before, after, lost, rulings_ok, th=TH_RECALL10):
    return {"no_golden_lost": not lost,
            "recall10_within_threshold": round(before["recall10"] - after["recall10"], 4) <= th,
            "duplicates_not_higher": after["duplicates"] <= before["duplicates"],
            "rulings_intact": all(rulings_ok)}


def kg_rows(pal):
    r = subprocess.run(["trusty-memory", "palace", "stats", pal, "--json"], capture_output=True, text=True,
                       timeout=120, cwd="/tmp")
    t = {x["name"]: x["rows"] for x in json.loads(r.stdout)["tables"]}
    return t.get("triples")


def deletions(pal):
    r = subprocess.run(["trusty-memory", "palace", "deletions", pal, "--json", "--limit", "1000000"],
                       capture_output=True, text=True, timeout=120, cwd="/tmp")
    try:
        d = json.loads(r.stdout or "[]")
    except ValueError:
        return []
    return d if isinstance(d, list) else d.get("records", d.get("deletions", []))


def measure(m, pal, queries, idmap, survivor):
    """Recall of the golden set on `pal` over the FULL expected set (#11351); a
    merged drawer's survivor counts. A query with no seeded target is skipped.
    #11815: a wing-scoped recall, and any hit from outside `pal` aborts the run."""
    rows, own = [], set(list_all(m, pal))
    for q in queries:
        mapped, missing = map_expected(q["expected"], idmap)
        if missing == len(mapped):
            continue
        resp, ms, err = recall(m, pal, q["query"], 10)
        check_own(resp, own, pal)
        ranked = [survivor.get(r["drawer_id"], r["drawer_id"]) for r in (resp or {}).get("results", [])]
        exp = [survivor.get(e, e) for e in mapped]
        rows.append({"id": q["id"], "ms": ms, "degraded": bool(err), "top10": ranked[:10],
                     **score_query(ranked, exp, lambda a, b: a == b)})
    return rows


def dream_pass(m, pal, src, idmap, golden, private_path, source):
    """Measure the golden set on seeded `pal`, dream once, measure again."""
    info = {"source_palace": source}
    priv = json.load(open(private_path))
    if not priv.get("dream_rulings"):
        priv["dream_rulings"] = pick_rulings([dict(d, _src=source) for d in src.values()])
        json.dump(priv, open(private_path, "w"), indent=1)
        info["rulings_recorded"] = True
    rulings = [r for r in priv["dream_rulings"] if r["source_drawer_id"] in idmap]
    queries = [q for q in golden["memory"] if q["palace"] == source]
    golden_ids = {idmap[e] for q in queries for e in q["expected"] if e in idmap}
    pre = list_all(m, pal)
    b_rows = measure(m, pal, queries, idmap, {})
    before = {**summarize(b_rows), "drawers": len(pre), "duplicates": dup_count([d["content"] for d in pre.values()]),
              "kg_triples": kg_rows(pal)}
    dr, dms, derr = m.call("palace_dream", {"palace": pal, "max_age_days": DREAM_MAX_AGE_DAYS}, timeout=DREAM_TIMEOUT)
    if derr:
        raise RuntimeError(f"palace_dream: {derr}")
    post = list_all(m, pal)
    recs = deletions(pal)
    survivor = {}
    for r in recs:
        rid, sid = r.get("drawer_id"), r.get("survivor_id")
        if rid and sid and sid in post:
            survivor[rid] = sid
    removed = set(pre) - set(post)
    a_rows = measure(m, pal, queries, idmap, survivor)
    after = {**summarize(a_rows), "drawers": len(post), "duplicates": dup_count([d["content"] for d in post.values()]),
             "kg_triples": kg_rows(pal)}
    lost = sorted(g for g in golden_ids if g not in post and g not in survivor)
    top_after = {r["id"]: r for r in a_rows}
    dropped = [r["id"] for r in b_rows if r["r10"] and not top_after.get(r["id"], {}).get("r10")]
    contents = [d["content"] for d in post.values()]
    rulings_ok = [ruling_intact(r, contents) for r in rulings]
    info.update(before={k: v for k, v in before.items()}, after=after, dream_ms=round(dms),
                dream_response={k: v for k, v in (dr or {}).items() if isinstance(v, (int, float, bool)) or v is None},
                drawers={"kept": len(set(pre) & set(post)), "removed": len(removed),
                         "merged": len(removed & set(survivor)), "added": len(set(post) - set(pre))},
                golden={"ids": len(golden_ids), "lost": len(lost), "lost_sha": [sha(x)[:12] for x in lost],
                        "dropped_from_top10": dropped},
                rulings={"checked": len(rulings), "intact": sum(rulings_ok),
                         "ids": [r["source_drawer_id"] for r in rulings]},
                verdict=dream_verdict(before, after, lost, rulings_ok),
                deletion_records=len(recs), max_age_days=DREAM_MAX_AGE_DAYS,
                noop_pass=not removed and not (dr or {}).get("summary_facts_created")
                and not (dr or {}).get("facts_evicted") and len(post) == len(pre))
    if len(rulings) < 5:
        info["verdict"]["rulings_intact"] = False
    return info


def dream_findings(info, base):
    out = [f"dream: {k} FAILED" for k, v in info.get("verdict", {}).items() if not v]
    if info["golden"]["dropped_from_top10"]:
        out.append(f"dream: golden queries no longer in top 10: {info['golden']['dropped_from_top10']}")
    if info.get("noop_pass"):
        out.append("dream: the pass changed nothing (0 created, 0 evicted), so the verdict is vacuous")
    d = info.get("delete", {})
    if d.get("error") or d.get("in_palace_list") or d.get("data_dir_exists"):
        out.append(f"dream: test palace delete not verified: {d}")
    if base and base.get("after", {}).get("n"):
        drop = round(base["after"]["recall10"] - info["after"]["recall10"], 4)
        if drop > TH_RECALL10:
            out.append(f"vs-baseline dream: after-recall@10 {base['after']['recall10']} -> {info['after']['recall10']}")
    return out


def print_dream(info):
    print(f"{'dream':8} {'n':>3} {'R@5':>6} {'R@10':>6} {'MRR':>6} {'drawers':>8} {'dups':>5} {'kg':>7}")
    for k in ("before", "after"):
        x = info[k]
        print(f"{k:8} {x['n']:>3} {x['recall5']:>6} {x['recall10']:>6} {x['mrr']:>6} {x['drawers']:>8} {x['duplicates']:>5} {x['kg_triples']:>7}")
    print("drawers:", info["drawers"], "| dream_ms:", info["dream_ms"], "| seeded:", info["seeded"],
          "| rulings:", info["rulings"]["intact"], "/", info["rulings"]["checked"])
    print("verdict:", info["verdict"])
