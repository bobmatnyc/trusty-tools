#!/usr/bin/env python3
"""Read-only accuracy and latency drift test for trusty-search and trusty-memory.

Why: the Architect's standing goal ar-G05 needs an every-two-weeks check that
retrieval has not regressed. This runs a fixed golden query set through the
live MCP surfaces (the same stdio servers Claude Code uses) and compares the
result with a stored baseline and with the previous run.

What: for each lane it computes recall@5, recall@10, MRR, p50/p95 latency,
the degraded share and the embed cold share, appends one JSONL row to the
results file, and prints findings when a metric crosses a threshold. Search
has two lanes: `search:as-called` (no `mode`, as agents call it) and
`search:best-mode` (`mode` by target type, see best_mode). The memory lane
never recalls a live palace, because recall raises `access_count` (#11348):
it seeds a guarded test palace per source palace, recalls there, and deletes
it in a `finally` block (see run_memory). Search uses only read-only tools;
there is no reindex or daemon-restart path in this file.

Test: `python3 -I scripts/drift_test_mem_search.py --selftest` (pure-function
checks, no daemon). A live run is `python3 -I scripts/drift_test_mem_search.py`.
"""
import argparse, hashlib, json, math, os, queue, re, secrets, subprocess, sys, threading, time
from datetime import datetime, timezone

ALLOWED_TOOLS = {"list_indexes", "index_status", "search", "palace_info"}
HOME = os.path.expanduser("~")
HERE = os.path.dirname(os.path.abspath(__file__))
DEF_PUBLIC = os.path.join(HERE, "..", "testdata", "drift", "mem-search", "golden.public.json")
DEF_PRIVATE = os.path.join(HOME, ".trusty-tools", "drift", "mem-search", "golden.private.json")
DEF_BASELINE = os.path.join(HERE, "..", "testdata", "drift", "mem-search", "baseline.json")
DEF_RESULTS = os.path.join(HOME, ".trusty-tools", "drift", "mem-search", "runs.jsonl")
# Thresholds (Architect brief 2026-11-16): a finding when recall@10 drops more
# than 5 points, p95 grows past 1.5x, or the degraded share rises 10 points.
TH_RECALL10, TH_P95_X, TH_DEGRADED = 0.05, 1.5, 0.10
# The p95 check runs only when |cold share - reference cold share| <= this.
# A cold query embeds (~tens of ms or more); a warm one is a cache hit, so a
# swing of more than a quarter of the queries moves p95 on its own. A lane
# that reports no embed timing (memory: no query-embed cache) records None and
# compares against None; None against a number is skipped.
TH_COLD_TOL = 0.25
CALL_TIMEOUT = 90.0
SEARCH_LANES = ("search:as-called", "search:best-mode")
PROSE_EXT = (".md", ".mdx", ".txt", ".rst")


class Mcp:
    """Minimal stdio JSON-RPC client; refuses any tool not in ALLOWED_TOOLS."""

    def __init__(self, cmd):
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True, cwd="/tmp")
        self.q, self.n = queue.Queue(), 0
        threading.Thread(target=self._pump, daemon=True).start()
        r = self._rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                                     "clientInfo": {"name": "drift-test", "version": "1"}})
        self.server = r.get("result", {}).get("serverInfo", {})
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _pump(self):
        for line in self.p.stdout:
            self.q.put(line)
        self.q.put(None)

    def _send(self, o):
        self.p.stdin.write(json.dumps(o) + "\n")
        self.p.stdin.flush()

    def _rpc(self, method, params, timeout=CALL_TIMEOUT):
        self.n += 1
        self._send({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params})
        while True:
            line = self.q.get(timeout=timeout)
            if line is None:
                raise RuntimeError("server closed stdout")
            r = json.loads(line)
            if r.get("id") == self.n:
                return r

    def call(self, name, args, timeout=CALL_TIMEOUT):
        """Return (parsed_json_or_None, wall_ms, error_or_None).

        A write tool (dream lane only) also needs a PalaceGuard that owns the
        target palace; see PalaceGuard."""
        if name not in getattr(self, "allowed", ALLOWED_TOOLS):
            raise PermissionError(f"tool {name} is not allow-listed for this client")
        if name in GUARDED_TOOLS:
            guard = getattr(self, "guard", None)
            if guard is None:
                raise PermissionError(f"tool {name} needs a palace guard")
            guard.check(args.get("palace") or args.get("palace_id") or args.get("name"))
        t = time.perf_counter()
        try:
            r = self._rpc("tools/call", {"name": name, "arguments": args}, timeout)
        except Exception as e:  # timeout or closed pipe: a failed answer, not a crash
            return None, (time.perf_counter() - t) * 1000, f"{type(e).__name__}: {e}"
        ms = (time.perf_counter() - t) * 1000
        if "error" in r:
            return None, ms, str(r["error"])[:200]
        text = r["result"]["content"][0]["text"]
        try:
            return json.loads(text), ms, None
        except ValueError:
            return None, ms, text[:200]  # plain-text answer (e.g. index parked notice)

    def close(self):
        self.p.terminate()


def pct(vals, p):
    """Nearest-rank percentile."""
    if not vals:
        return None
    s = sorted(vals)
    return s[max(0, math.ceil(p / 100 * len(s)) - 1)]


def path_match(result_path, expected):
    return result_path == expected or result_path.endswith("/" + expected)


def rank_of(ranked, expected, match):
    """1-based rank of each expected item in `ranked` (None if absent)."""
    out = []
    for e in expected:
        r = next((i + 1 for i, x in enumerate(ranked) if match(x, e)), None)
        out.append(r)
    return out


def score_query(ranked, expected, match):
    ranks = rank_of(ranked, expected, match)
    hits = lambda k: sum(1 for r in ranks if r and r <= k) / len(expected)
    found = [r for r in ranks if r]
    return {"r5": hits(5), "r10": hits(10), "rr": (1.0 / min(found)) if found else 0.0}


def search_degraded(resp):
    """Reasons an answer is degraded: flagged meta, fallback or lexical-only rows."""
    m, why = resp.get("meta", {}), []
    if m.get("embedder_error"):
        why.append("embedder_error")
    for k in ("vector_unavailable", "vector_disabled_by_config", "bm25_lane_degraded", "exact_match_degraded"):
        if m.get(k):
            why.append(k)
    emb = m.get("embedder", {})
    if emb.get("active") != emb.get("intended"):
        why.append("embedder_switched")
    if (m.get("timings", {}).get("embed_ms") or 0) >= 5000:
        why.append("embed_deadline")
    rows = resp.get("results", [])
    if any("fallback" in str(r.get("match_reason", "")) for r in rows):
        why.append("fallback_rows")
    if rows and not any(r.get("match_reason") in ("vector", "hybrid", "semantic") for r in rows):
        why.append("lexical_only")
    return why


RULINGS_DEGRADED = "rulings_degraded"


def memory_degraded(resp):
    """Degraded reasons of a memory answer, `rulings_degraded` excluded (own line)."""
    return [k for k, v in resp.items() if v and k != RULINGS_DEGRADED
            and any(w in k.lower() for w in ("degraded", "fallback", "lexical_only", "unavailable"))]


def summarize(rows):
    n = len(rows)
    if not n:
        return {"n": 0}
    lat = [r["ms"] for r in rows]
    return {"n": n, "recall5": round(sum(r["r5"] for r in rows) / n, 4),
            "recall10": round(sum(r["r10"] for r in rows) / n, 4),
            "mrr": round(sum(r["rr"] for r in rows) / n, 4),
            "p50_ms": round(pct(lat, 50)), "p95_ms": round(pct(lat, 95)),
            "degraded_share": round(sum(1 for r in rows if r["degraded"]) / n, 4),
            # Share of queries whose embedding was computed (not served from the
            # daemon's query-embed cache); None when no row reports embed timing.
            "embed_cold_share": cold_share(rows),
            # Share of memory answers flagged `rulings_degraded`; reported only, never
            # compared (ruling df-T7c). None unless every row carries the flag.
            "rulings_degraded_share": rulings_share(rows)}


def rulings_share(rows):
    if not rows or not all("rulings_degraded" in r for r in rows):
        return None  # search lanes and the mixed `overall` lane carry no such line
    return round(sum(1 for r in rows if r["rulings_degraded"]) / len(rows), 4)


def cold_share(rows):
    timed = [r for r in rows if r.get("embed_ms") is not None]
    if not timed:
        return None
    return round(sum(1 for r in timed if r["embed_ms"] > 0) / len(rows), 4)


def best_mode(expected):
    """`text` when every target is prose, else `code` (the search default)."""
    return "text" if expected and all(e.lower().endswith(PROSE_EXT) for e in expected) else "code"


def cold_comparable(c, b):
    """(comparable, reason) for the p95 check of two lanes' cold shares."""
    cc, bc = c.get("embed_cold_share"), b.get("embed_cold_share")
    if cc is None and bc is None:
        return True, None
    if cc is None or bc is None:
        return False, f"cold share {bc} vs {cc}: one side has no embed timing"
    if abs(cc - bc) > TH_COLD_TOL:
        return False, f"cold share {bc} -> {cc} differs by more than {TH_COLD_TOL}"
    return True, None


def compare(cur, ref, label, skipped=None):
    """Findings of `cur` metrics against `ref`; skipped p95 checks go to `skipped`."""
    out = []
    # Rows before the lane split call the as-called lane plain `search`.
    if "search" in ref and "search:as-called" not in ref:
        ref = {("search:as-called" if k == "search" else k): v for k, v in ref.items()}
    if "search" in cur and "search:as-called" not in cur:
        cur = {("search:as-called" if k == "search" else k): v for k, v in cur.items()}
    for lane in sorted(set(cur) & set(ref)):
        c, b = cur.get(lane, {}), ref.get(lane, {})
        if not c.get("n") or not b.get("n"):
            continue
        d = round(b["recall10"] - c["recall10"], 4)
        if d > TH_RECALL10:
            out.append(f"{label} {lane}: recall@10 {b['recall10']} -> {c['recall10']} (drop {d} > {TH_RECALL10})")
        ok, why = cold_comparable(c, b)
        if not ok:
            if skipped is not None:
                skipped.append(f"{label} {lane}: p95 check skipped, {why}")
        elif b["p95_ms"] and c["p95_ms"] > TH_P95_X * b["p95_ms"]:
            out.append(f"{label} {lane}: p95 {b['p95_ms']} ms -> {c['p95_ms']} ms (x{c['p95_ms'] / b['p95_ms']:.2f} > {TH_P95_X})")
        d = round(c["degraded_share"] - b["degraded_share"], 4)
        if d > TH_DEGRADED:
            out.append(f"{label} {lane}: degraded share {b['degraded_share']} -> {c['degraded_share']} (rise {d} > {TH_DEGRADED})")
    return out


def load_golden(paths):
    g, digests = {"search": [], "memory": []}, {}
    for p in paths:
        if not os.path.exists(p):
            print(f"note: golden file missing, skipped: {p}", file=sys.stderr)
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
    s.close()


def metrics(rows):
    out = {lane: summarize([r for r in rows if r["lane"] == lane]) for lane in SEARCH_LANES + ("memory",)}
    out["memory:dated-rulings"] = summarize([r for r in rows if r["lane"] == "memory" and r.get("kind") == "dated_ruling"])
    # overall = what agents get today: as-called search plus memory.
    out["overall"] = summarize([r for r in rows if r["lane"] in ("search:as-called", "memory")])
    return out


# ---------------------------------------------------------------- dream lane
# Brief 2026-11-16 (dreaming section): dream ONE test palace this run created,
# never a live one. Write tools are refused unless PalaceGuard owns the target.
WRITE_TOOLS = {"palace_create", "memory_remember", "palace_dream", "palace_delete"}
DREAM_TOOLS = WRITE_TOOLS | {"palace_list", "palace_info", "room_list", "memory_list", "memory_recall"}
# #11348: recall raises access_count, so it is guarded like a write.
GUARDED_TOOLS = WRITE_TOOLS | {"memory_recall"}
TEST_PREFIX = "drift-dream-test-"
TEST_NAME_RE = re.compile(re.escape(TEST_PREFIX) + r"\d{8}t\d{6}z-[0-9a-f]{6}")
DREAM_TIMEOUT = 1800.0
# max_age_days <= 0 is a no-op guard in engine/dream/semantic.rs, and a seeded
# fixture's drawers are all created "now", so a fixture pass changes nothing
# until the daemon can seed old drawers; dream_findings flags a no-op pass.
DREAM_MAX_AGE_DAYS = 1
DATE_RE = re.compile(r"\b20\d\d-\d\d-\d\d\b")
QUOTE_RE = re.compile(r"\"([^\"\n]{12,200})\"")
REMEMBER_TYPES = {"Ruling", "Decision", "Status", "Turn", "Reference"}


class PalaceGuard:
    """Owns the palaces this run created; every write tool names one of them.

    Why: a dream pass on a live palace (supervisor, cto) rewrites real memory.
    What: `claim` admits only a fresh, test-pattern name absent from the
    palace list; `check` refuses any other name, so a live palace is never a
    write target. Test: `selftest` (guard block).
    """

    def __init__(self):
        self.owned = set()

    @staticmethod
    def new_name():
        return TEST_PREFIX + datetime.now(timezone.utc).strftime("%Y%m%dt%H%M%Sz") + "-" + secrets.token_hex(3)

    def claim(self, name, existing):
        if not isinstance(name, str) or not TEST_NAME_RE.fullmatch(name):
            raise PermissionError(f"refused: {name!r} is not a test palace name")
        if name in existing:
            raise PermissionError(f"refused: {name!r} already exists, so this run did not create it")
        self.owned.add(name)

    def check(self, name):
        if name not in self.owned:
            raise PermissionError(f"refused: palace {name!r} was not created by this run")


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


def list_all(m, pal):
    rooms, _, err = m.call("room_list", {"palace": pal})
    if err:
        raise RuntimeError(f"room_list {pal}: {err}")
    names = [r["label"] for r in rooms.get("rooms", [])]
    out = {}
    for room in names:
        resp, _, err = m.call("memory_list", {"palace": pal, "room": room, "limit": 100000, "full": True})
        if err or resp is None:
            raise RuntimeError(f"memory_list {pal}/{room}: {err}")
        for d in resp.get("drawers", []):
            out[d["drawer_id"]] = dict(d, _room=room)
    resp, _, err = m.call("memory_list", {"palace": pal, "limit": 100000, "full": True})
    if err or resp is None or resp.get("truncated"):
        raise RuntimeError(f"memory_list {pal}: {err or 'truncated'}")
    for d in resp.get("drawers", []):
        out.setdefault(d["drawer_id"], dict(d, _room=None))
    return out


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
    """Recall of the mapped golden set on `pal`; a merged drawer's survivor counts."""
    rows = []
    for q in queries:
        resp, ms, err = m.call("memory_recall", {"palace": pal, "query": q["query"], "top_k": 10})
        ranked = [survivor.get(r["drawer_id"], r["drawer_id"]) for r in (resp or {}).get("results", [])]
        exp = [survivor.get(idmap[e], idmap[e]) for e in q["expected"] if e in idmap]
        if not exp:
            continue
        rows.append({"id": q["id"], "ms": ms, "degraded": bool(err), "top10": ranked[:10],
                     **score_query(ranked, exp, lambda a, b: a == b)})
    return rows


def seed_test_palace(m, guard, source, existing):
    """Create a guarded test palace and copy `source`'s drawers into it.

    Returns (palace, source drawers, live id -> test id map, info). The source
    is only listed (memory_list), never recalled."""
    pal, t0 = guard.new_name(), time.perf_counter()
    guard.claim(pal, existing)
    _, _, err = m.call("palace_create", {"name": pal, "force": True,
                                         "description": "drift-test memory/dream fixture; deleted by the run"})
    if err:
        raise RuntimeError(f"palace_create: {err}")
    src = list_all(m, source)
    idmap, skipped = {}, 0
    for d in src.values():
        a = {"palace": pal, "text": d["content"], "force": True}
        if d["_room"]:
            a["room"] = d["_room"]
        if d.get("drawer_type") in REMEMBER_TYPES:
            a["type"] = d["drawer_type"]
        resp, _, err = m.call("memory_remember", a)
        if err or not resp or not resp.get("drawer_id"):
            skipped += 1
            continue
        idmap[d["drawer_id"]] = resp["drawer_id"]
    return pal, src, idmap, {"test_palace": pal, "source_palace": source, "seeded": len(idmap), "seed_skipped": skipped,
                             "seed_s": round(time.perf_counter() - t0, 1)}


def delete_test_palace(m, guard, pal):
    """Delete an owned test palace and verify: no error, not listed, no data dir."""
    guard.check(pal)
    pi, _, _ = m.call("palace_info", {"palace": pal})
    ddir = (pi or {}).get("data_dir")
    _, _, err = m.call("palace_delete", {"palace_id": pal, "force": True})
    lst, _, lerr = m.call("palace_list", {})
    still = lerr is not None or pal in set((lst or {}).get("palaces", []))
    return {"error": err, "in_palace_list": still, "data_dir_exists": os.path.exists(ddir) if ddir else None}


def run_memory(golden, rows, info, private_path, dream_source=None):
    """Memory lane on seeded test palaces (one per source palace), never a live one.

    Why: memory_recall raises a served drawer's access_count (#11348), so a
    recall on a live palace changes the data it measures. What: per source
    palace, seed a guarded test palace, warm it up, recall the golden queries
    through the live -> test id map, and (when `dream_source` matches) run the
    dream pass on the same seeding. Every owned palace is deleted in `finally`
    and the delete verified. A served hit whose id is not in the test palace
    came from another palace (the daemon's user-scope rulings leg) and is
    counted in `foreign_hits`. Test: `selftest` (guard, cold-share, best-mode
    blocks); a live run's `memory_palaces[*].delete`.
    """
    guard = PalaceGuard()
    m = Mcp(["trusty-memory", "serve", "--stdio"])
    m.allowed, m.guard = DREAM_TOOLS, guard
    info["versions"]["trusty-memory"] = m.server.get("version")
    info["memory_palaces"], dream = {}, None
    try:
        lst, _, err = m.call("palace_list", {})
        existing = set((lst or {}).get("palaces", []))
        if err or not existing:
            raise RuntimeError(f"palace_list failed: {err}")
        for source in sorted({q["palace"] for q in golden["memory"]}):
            pi, _, _ = m.call("palace_info", {"palace": source})
            info["palaces"][source] = {"drawers": (pi or {}).get("drawer_count")}
            pal, src, idmap, pinfo = seed_test_palace(m, guard, source, existing)
            info["memory_palaces"][source] = pinfo
            own = set(list_all(m, pal))
            _, ms, _ = m.call("memory_recall", {"palace": pal, "query": "warm-up", "top_k": 1})
            info["warmup_ms"][f"memory:{source}"] = round(ms)
            foreign = unmapped = 0
            for q in [q for q in golden["memory"] if q["palace"] == source]:
                exp = [idmap[e] for e in q["expected"] if e in idmap]
                if not exp:
                    unmapped += 1
                    continue
                resp, ms, err = m.call("memory_recall", {"palace": pal, "query": q["query"], "top_k": 10})
                ranked = [r["drawer_id"] for r in (resp or {}).get("results", [])]
                foreign += sum(1 for x in ranked if x not in own)
                why = ["error"] if err or resp is None else memory_degraded(resp)
                rows.append({"lane": "memory", "id": q["id"], "ms": ms, "degraded": bool(why), "why": why,
                             "rulings_degraded": bool(resp and resp.get(RULINGS_DEGRADED)),
                             "kind": q.get("kind"), **score_query(ranked, exp, lambda a, b: a == b)})
            pinfo.update(foreign_hits=foreign, unmapped_queries=unmapped)
            if source == dream_source:
                dream = dream_pass(m, pal, src, idmap, golden, private_path, source)
                dream.update(pinfo)
        return dream
    finally:
        for pal in sorted(guard.owned):
            d = delete_test_palace(m, guard, pal)
            info.setdefault("deletes", {})[pal] = d
            if dream and dream.get("test_palace") == pal:
                dream["delete"] = d
        m.close()


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


def selftest():
    assert pct([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 50) == 5 and pct([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 95) == 10
    assert pct([7], 95) == 7 and pct([], 50) is None
    sc = score_query(["a", "x/b.rs", "c"], ["b.rs", "zzz"], path_match)
    assert sc == {"r5": 0.5, "r10": 0.5, "rr": 0.5}, sc
    assert score_query(["q"], ["z"], path_match) == {"r5": 0.0, "r10": 0.0, "rr": 0.0}
    assert search_degraded({"meta": {"embedder_error": "x"}, "results": []}) == ["embedder_error"]
    assert "lexical_only" in search_degraded({"meta": {}, "results": [{"match_reason": "bm25"}]})
    assert search_degraded({"meta": {}, "results": [{"match_reason": "hybrid"}]}) == []
    base = {"search": {"n": 5, "recall10": 0.9, "p95_ms": 100, "degraded_share": 0.0}}
    bad = {"search": {"n": 5, "recall10": 0.8, "p95_ms": 200, "degraded_share": 0.2}}
    assert len(compare(bad, base, "t")) == 3 and compare(base, base, "t") == []
    # cold-share gate: p95 compared only at comparable cold shares; a skip says why
    warm = {"search": dict(base["search"], embed_cold_share=0.0)}
    cold = {"search": dict(bad["search"], embed_cold_share=0.5)}
    sk = []
    assert len(compare(cold, warm, "t", sk)) == 2 and len(sk) == 1 and "differs by more than" in sk[0], sk
    near = {"search": dict(bad["search"], embed_cold_share=0.2)}
    assert len(compare(near, warm, "t", [])) == 3
    sk = []
    compare({"search": dict(bad["search"], embed_cold_share=None)}, warm, "t", sk)
    assert "no embed timing" in sk[0], sk
    assert cold_share([{"embed_ms": 0}, {"embed_ms": 12}]) == 0.5 and cold_share([{}]) is None
    assert best_mode(["docs/a.md", "b.MD"]) == "text" and best_mode(["docs/a.md", "x.rs"]) == "code"
    try:
        Mcp.call(type("X", (), {})(), "memory_recall", {"palace": "supervisor"})
    except PermissionError:
        print("client memory_recall without the test-palace client: refused")
    else:
        raise AssertionError("memory_recall on the default client was not refused")
    try:
        Mcp.call(type("X", (), {})(), "memory_forget", {})
    except PermissionError:
        pass
    else:
        raise AssertionError("write tool was not refused")
    guard_selftest()
    assert dup_count(["A b", "a  B", "c"]) == 1 and dup_count(["x", "y"]) == 0
    r = {"date": "2026-11-01", "quote": "never dream a live palace"}
    assert ruling_intact(r, ['On 2026-11-01 Bob: "never dream a live palace".'])
    assert not ruling_intact(r, ['On 2026-11-01 Bob: "never dream a live  palace".'])
    picked = pick_rulings([{"drawer_id": "1", "_src": "s", "content": 'x 2026-01-02 "a quoted ruling text"'},
                           {"drawer_id": "2", "_src": "s", "content": "no date here"}])
    assert [p["source_drawer_id"] for p in picked] == ["1"] and picked[0]["date"] == "2026-01-02"
    b, a = {"recall10": 0.9, "duplicates": 3}, {"recall10": 0.86, "duplicates": 3}
    assert all(dream_verdict(b, a, [], [True]).values())
    v = dream_verdict(b, {"recall10": 0.8, "duplicates": 4}, ["x"], [False])
    assert not any(v.values()), v
    # ruling df-T7c: rulings_degraded is reported on its own, not as degraded
    resp = {"results": [], "rulings_degraded": True, "vector_unavailable": False}
    assert memory_degraded(resp) == [] and memory_degraded(dict(resp, lexical_only=True)) == ["lexical_only"]
    mrows = [{"r5": 1, "r10": 1, "rr": 1, "ms": 1, "degraded": False, "rulings_degraded": True},
             {"r5": 1, "r10": 1, "rr": 1, "ms": 1, "degraded": False, "rulings_degraded": False}]
    sm = summarize(mrows)
    assert sm["degraded_share"] == 0.0 and sm["rulings_degraded_share"] == 0.5, sm
    assert summarize([dict(mrows[0], degraded=True)])["degraded_share"] == 1.0
    assert summarize([{k: v for k, v in mrows[0].items() if k != "rulings_degraded"}])["rulings_degraded_share"] is None
    flagged = {"memory": dict(sm, n=2, recall10=1.0, p95_ms=1, rulings_degraded_share=1.0)}
    assert compare(flagged, {"memory": dict(sm, n=2, recall10=1.0, p95_ms=1, rulings_degraded_share=0.0)}, "t") == []
    # ruling df-T7c: an old row's `search` lane is compared as `search:as-called`
    cur = {"search:as-called": bad["search"]}
    assert len(compare(cur, base, "t")) == 3, "old `search` lane skipped"
    assert compare(cur, {"search:as-called": base["search"], "search": bad["search"]}, "t") != []
    # ruling df-T7c: the committed baseline carries no `root` and no home path
    brow = {"ts": "t", "versions": {}, "palaces": {}, "golden_sha256": {}, "thresholds": {},
            "embed_cold_share": {}, "metrics": {},
            "indexes": {"ix": {"chunks": 1, "disk_bytes": 2, "root": "/" + "Users/someone/proj"}}}
    bp = baseline_payload(brow)
    assert bp["indexes"] == {"ix": {"chunks": 1, "disk_bytes": 2}}, bp
    assert "root" not in json.dumps(bp) and ("/" + "Users/") not in json.dumps(bp)
    assert "root" in brow["indexes"]["ix"], "run row must keep root"
    print("selftest ok")


def guard_selftest():
    """The guard refuses every palace this run did not create (printed per case)."""
    g, live = PalaceGuard(), {"supervisor", "cto", "trusty-tools"}
    fresh = g.new_name()
    stale = TEST_PREFIX + "20260101t000000z-abcdef"
    cases = [("claim", "supervisor", live), ("claim", "cto", live), ("claim", "drift-dream-test-x", live),
             ("claim", stale, live | {stale}), ("check", "supervisor", None), ("check", stale, None)]
    for op, name, ex in cases:
        try:
            g.claim(name, ex) if op == "claim" else g.check(name)
        except PermissionError as e:
            print(f"guard {op} {name!r}: refused ({e})")
        else:
            raise AssertionError(f"guard {op} {name!r} was not refused")
    g.claim(fresh, live)
    g.check(fresh)
    print(f"guard claim+check {fresh!r}: allowed (created by this run)")
    fake = type("X", (), {"allowed": DREAM_TOOLS, "guard": g})()
    for tool, args in [("palace_dream", {"palace": "supervisor"}), ("palace_delete", {"palace_id": "cto"}),
                       ("memory_remember", {"palace": "supervisor", "text": "x"}),
                       ("memory_recall", {"palace": "cto", "query": "x"})]:
        try:
            Mcp.call(fake, tool, args)
        except PermissionError as e:
            print(f"client {tool} on {list(args.values())[0]!r}: refused ({e})")
        else:
            raise AssertionError(f"{tool} on a live palace was not refused")
    try:
        Mcp.call(type("Y", (), {"allowed": DREAM_TOOLS})(), "palace_dream", {"palace": fresh})
    except PermissionError as e:
        print(f"client palace_dream without guard: refused ({e})")
    else:
        raise AssertionError("palace_dream without a guard was not refused")


def baseline_payload(row):
    """The committed baseline: index ids and counts only. Why: the repo is public, and an index
    `root` is an absolute home path (#df-T7c). runs.jsonl lives outside the repo and keeps root.
    Test: `selftest` (baseline has no root key and no home path)."""
    b = {k: row[k] for k in ("ts", "versions", "indexes", "palaces", "golden_sha256", "thresholds",
                             "embed_cold_share", "metrics")}
    b["indexes"] = {ix: {k: v for k, v in st.items() if k != "root"} for ix, st in row["indexes"].items()}
    return b


def memory_findings(info):
    out = []
    for src, p in info.get("memory_palaces", {}).items():
        if p.get("foreign_hits"):
            out.append(f"memory {src}: {p['foreign_hits']} served hits came from a palace other than the test "
                       "palace (user-scope rulings leg), so a live drawer's access_count rose")
        if p.get("unmapped_queries"):
            out.append(f"memory {src}: {p['unmapped_queries']} golden queries have no seeded target")
    for pal, d in info.get("deletes", {}).items():
        if d.get("error") or d.get("in_palace_list") or d.get("data_dir_exists") is not False:
            out.append(f"memory: test palace delete not verified: {pal} {d}")
    return out


def print_table(mt):
    print(f"{'lane':22} {'n':>3} {'R@5':>6} {'R@10':>6} {'MRR':>6} {'p50ms':>6} {'p95ms':>6} {'degr%':>6} {'cold%':>6}")
    for lane, x in mt.items():
        if not x.get("n"):
            continue
        cold = "-" if x["embed_cold_share"] is None else f"{x['embed_cold_share'] * 100:.1f}"
        print(f"{lane:22} {x['n']:>3} {x['recall5']:>6} {x['recall10']:>6} {x['mrr']:>6} {x['p50_ms']:>6} "
              f"{x['p95_ms']:>6} {x['degraded_share'] * 100:>6.1f} {cold:>6}")
    for lane, x in mt.items():
        if x.get("rulings_degraded_share") is not None:
            print(f"rulings_degraded % {lane}: {x['rulings_degraded_share'] * 100:.1f}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--golden-public", default=DEF_PUBLIC)
    ap.add_argument("--golden-private", default=DEF_PRIVATE)
    ap.add_argument("--baseline", default=DEF_BASELINE)
    ap.add_argument("--results", default=DEF_RESULTS)
    ap.add_argument("--write-baseline", action="store_true", help="store this run as the baseline")
    ap.add_argument("--no-record", action="store_true", help="do not append to the results file")
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--dream", action="store_true", help="also run the dream lane on the memory lane's seeding")
    ap.add_argument("--dream-source", default="supervisor", help="palace whose test copy is dreamed")
    a = ap.parse_args()
    if a.selftest:
        return selftest()
    golden, digests = load_golden([a.golden_public, a.golden_private])
    rows, info = [], {"versions": {}, "indexes": {}, "palaces": {}, "warmup_ms": {}}
    run_search(golden, rows, info)
    dream = run_memory(golden, rows, info, a.golden_private, a.dream_source if a.dream else None)
    mt = metrics(rows)
    prev = None
    if os.path.exists(a.results):
        lines = [l for l in open(a.results) if l.strip() and '"lane": "dream"' not in l]
        prev = json.loads(lines[-1]) if lines else None
    base = json.load(open(a.baseline)) if os.path.exists(a.baseline) else {}
    findings, skipped = memory_findings(info), []
    if base.get("metrics"):
        findings += compare(mt, base["metrics"], "vs-baseline", skipped)
    if prev:
        findings += compare(mt, prev["metrics"], "vs-previous", skipped)
    ts = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    row = {"ts": ts, "golden_sha256": digests, **info,
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
    print("memory palaces:", {s: {k: p[k] for k in ("seeded", "seed_skipped", "foreign_hits", "unmapped_queries")}
                              for s, p in info["memory_palaces"].items()})
    print("test palace deletes:", info.get("deletes"))
    for s in skipped:
        print(" skipped:", s)
    if dream:
        print_dream(dream)
    findings += dfind
    print("FINDINGS:" if findings else "no findings" + ("" if base or prev else " (no baseline or previous run to compare)"))
    for f in findings:
        print(" -", f)
    return 2 if findings else 0


def print_dream(info):
    print(f"{'dream':8} {'n':>3} {'R@5':>6} {'R@10':>6} {'MRR':>6} {'drawers':>8} {'dups':>5} {'kg':>7}")
    for k in ("before", "after"):
        x = info[k]
        print(f"{k:8} {x['n']:>3} {x['recall5']:>6} {x['recall10']:>6} {x['mrr']:>6} {x['drawers']:>8} {x['duplicates']:>5} {x['kg_triples']:>7}")
    print("drawers:", info["drawers"], "| dream_ms:", info["dream_ms"], "| seeded:", info["seeded"],
          "| rulings:", info["rulings"]["intact"], "/", info["rulings"]["checked"])
    print("verdict:", info["verdict"])


if __name__ == "__main__":
    sys.exit(main() or 0)
