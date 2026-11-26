"""Shared core of the memory/search drift test: MCP client, palace guard, metrics.

Why: split out of drift_test_mem_search.py to keep each file under the 500-SLOC
cap. What: the allow-listed stdio MCP client, the PalaceGuard that owns test
palaces, and the pure scoring/compare functions. Test:
`python3 -I scripts/test_drift_test_mem_search.py`.
"""
import json, math, queue, re, secrets, subprocess, threading, time
from datetime import datetime, timezone

ALLOWED_TOOLS = {"list_indexes", "index_status", "search", "palace_info"}
# Thresholds: a finding when recall@10 drops more
# than 5 points, p95 grows past 1.5x, or the degraded share rises 10 points.
TH_RECALL10, TH_P95_X, TH_DEGRADED = 0.05, 1.5, 0.10
# The p95 check runs only when |cold share - reference cold share| <= this.
# A cold query embeds (~tens of ms or more); a warm one is a cache hit, so a
# swing of more than a quarter of the queries moves p95 on its own. A lane
# that reports no embed timing (memory: no query-embed cache) records None and
# compares against None; None against a number is skipped.
TH_COLD_TOL = 0.25
CALL_TIMEOUT = 90.0
CLOSE_TIMEOUT = 5.0
SEARCH_LANES = ("search:as-called", "search:best-mode")
PROSE_EXT = (".md", ".mdx", ".txt", ".rst")
RULINGS_DEGRADED = "rulings_degraded"

# Dream ONE test palace this run created, never a live one. Write tools are refused unless PalaceGuard owns the target.
WRITE_TOOLS = {"palace_create", "memory_remember", "palace_dream", "palace_delete"}
DREAM_TOOLS = WRITE_TOOLS | {"palace_list", "palace_info", "room_list", "memory_list", "memory_recall"}
# #11348: recall raises access_count, so it is guarded like a write.
GUARDED_TOOLS = WRITE_TOOLS | {"memory_recall"}
TEST_PREFIX = "drift-dream-test-"
TEST_TS_FMT = "%Y%m%dt%H%M%Sz"
TEST_NAME_RE = re.compile(re.escape(TEST_PREFIX) + r"(\d{8}t\d{6}z)-[0-9a-f]{6}")
STALE_AGE_S = 3600
# #11815: an unscoped (RecallScope::All) recall also runs the daemon's user-scope
# rulings leg (TRUSTY_MEMORY_RULINGS_PALACES), which serves and access-bumps live
# drawers from other palaces. A wing-scoped recall skips that leg; a fresh test
# palace files every room under its `default` wing, so this scope is the whole palace.
RECALL_WING = "default"


class RunAborted(RuntimeError):
    """#11815: the run must not be scored or recorded (partial seed, foreign hit)."""


class Mcp:
    """Minimal stdio JSON-RPC client; refuses any tool not in ALLOWED_TOOLS."""

    def __init__(self, cmd):
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True, cwd="/tmp")
        try:
            self.q, self.n = queue.Queue(), 0
            threading.Thread(target=self._pump, daemon=True).start()
            r = self._rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                                         "clientInfo": {"name": "drift-test", "version": "1"}})
            self.server = r.get("result", {}).get("serverInfo", {})
            self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        except BaseException:
            self.close()  # #11351: a failed or timed-out initialize must not leak the child
            raise

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
        if name == "memory_recall" and (args.get("wing") != RECALL_WING or "room" in args):
            raise PermissionError(f"#11815: memory_recall must be scoped to wing {RECALL_WING!r}, "
                                  "or the daemon's rulings leg touches live palaces")
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
        """Terminate the server child, then kill it if it outlives CLOSE_TIMEOUT."""
        if self.p.poll() is not None:
            return
        self.p.terminate()
        try:
            self.p.wait(timeout=CLOSE_TIMEOUT)
        except subprocess.TimeoutExpired:
            self.p.kill()
            self.p.wait(timeout=CLOSE_TIMEOUT)


class PalaceGuard:
    """Owns the palaces this run created; every write tool names one of them.

    Why: a dream pass on a live palace (supervisor, cto) rewrites real memory.
    What: `claim` admits only a fresh, test-pattern name absent from the
    palace list; `adopt_stale` admits only a test-pattern name whose embedded
    timestamp is older than STALE_AGE_S (a leak of an earlier run); `check`
    refuses any other name, so a live palace is never a write target.
    Test: `test_guard`, `test_stale_sweep`.
    """

    def __init__(self):
        self.owned = set()

    @staticmethod
    def new_name():
        return TEST_PREFIX + datetime.now(timezone.utc).strftime(TEST_TS_FMT) + "-" + secrets.token_hex(3)

    def claim(self, name, existing):
        if not isinstance(name, str) or not TEST_NAME_RE.fullmatch(name):
            raise PermissionError(f"refused: {name!r} is not a test palace name")
        if name in existing:
            raise PermissionError(f"refused: {name!r} already exists, so this run did not create it")
        self.owned.add(name)

    def adopt_stale(self, name, now):
        m = TEST_NAME_RE.fullmatch(name) if isinstance(name, str) else None
        if not m:
            raise PermissionError(f"refused: {name!r} is not a test palace name")
        made = datetime.strptime(m.group(1), TEST_TS_FMT).replace(tzinfo=timezone.utc)
        age = (now - made).total_seconds()
        if age <= STALE_AGE_S:
            raise PermissionError(f"refused: {name!r} is {age:.0f}s old, not older than {STALE_AGE_S}s")
        self.owned.add(name)
        return age

    def check(self, name):
        if name not in self.owned:
            raise PermissionError(f"refused: palace {name!r} was not created by this run")


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


def map_expected(expected, idmap):
    """(expected mapped to test ids, count unseeded). An unseeded target keeps a
    placeholder that never matches, so recall is over the full set (#11351)."""
    exp = [idmap.get(e, "unseeded:" + e) for e in expected]
    return exp, sum(1 for e in expected if e not in idmap)


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
    """Findings of `cur` metrics against `ref`; skipped p95 checks go to `skipped`.

    A lane the reference ran (n > 0) that is absent or has n = 0 now is a
    finding, never a silent skip (#11351). Test: `test_compare_absent_lane`."""
    out = []
    # Rows before the lane split call the as-called lane plain `search`.
    if "search" in ref and "search:as-called" not in ref:
        ref = {("search:as-called" if k == "search" else k): v for k, v in ref.items()}
    if "search" in cur and "search:as-called" not in cur:
        cur = {("search:as-called" if k == "search" else k): v for k, v in cur.items()}
    for lane in sorted(ref):
        c, b = cur.get(lane) or {}, ref.get(lane) or {}
        if not b.get("n"):
            continue
        if not c.get("n"):
            out.append(f"{label} {lane}: lane did not run (n=0, reference n={b['n']})")
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


def list_all(m, pal):
    """Every drawer of `pal` (per room, then unroomed), keyed by drawer id."""
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


def recall(m, pal, query, top_k):
    """#11815: the only recall the harness issues: test palace, wing-scoped (no rulings leg)."""
    return m.call("memory_recall", {"palace": pal, "wing": RECALL_WING, "query": query, "top_k": top_k})


def check_own(resp, own, pal):
    """Raise RunAborted when a recall served a drawer that is not in test palace `pal` (#11815)."""
    foreign = [r.get("drawer_id") for r in (resp or {}).get("results", []) if r.get("drawer_id") not in own]
    if foreign:
        raise RunAborted(f"recall on {pal} served {len(foreign)} drawer(s) from another palace, so a live "
                         f"drawer's access_count rose; aborting the memory lane: {foreign[:10]}")


def check_scope_covers(m, pal, own):
    """Refuse to recall when the wing scope would hide seeded drawers (#11815)."""
    resp, _, err = m.call("memory_list", {"palace": pal, "wing": RECALL_WING, "limit": 100000, "full": True})
    if err or resp is None or resp.get("truncated"):
        raise RunAborted(f"memory_list {pal} wing {RECALL_WING}: {err or 'truncated'}")
    hidden = own - {d["drawer_id"] for d in resp.get("drawers", [])}
    if hidden:
        raise RunAborted(f"wing {RECALL_WING!r} of {pal} misses {len(hidden)} seeded drawer(s); "
                         f"a scoped recall would under-score: {sorted(hidden)[:10]}")
