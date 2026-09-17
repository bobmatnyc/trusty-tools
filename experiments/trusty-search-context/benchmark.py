"""Frozen-corpus, zero-vector retrieval evaluation. Never discovers a daemon."""
from __future__ import annotations

import argparse
import hashlib
import json
import random
import statistics
import time
import urllib.request
import urllib.error
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

import tiktoken

Json = dict[str, Any]
ENCODING = tiktoken.get_encoding("cl100k_base")
COMPACT_FIELDS = ("path", "start_line", "end_line", "compact_snippet", "score", "match_reason")


def request(base: str, path: str, body: Json | None = None) -> Json:
    """Call only an explicit nondefault loopback fixture."""
    url = urlparse(base)
    if url.scheme != "http" or url.hostname != "127.0.0.1" or url.port in (None, 0, 7878):
        raise ValueError("A dedicated loopback daemon is required")
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(base + path, data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=180) as response:
            result = json.load(response)
    except urllib.error.HTTPError as error:
        detail = error.read().decode('utf-8', errors='replace')
        raise RuntimeError(f'HTTP {error.code} at {path}: {detail}') from error
    if not isinstance(result, dict):
        raise ValueError("Expected JSON object")
    return result


def validate_complete(status: Json) -> None:
    """Reject partial corpora even when the two stage flags say ready."""
    if status.get("status") != "ready" or status.get("chunk_count", 0) <= 0:
        raise ValueError("Index is not ready and nonempty")
    for key in ("chunks_dropped_by_cap", "walk_truncated_by_budget", "last_walk_error",
                "promotion_deferred", "migration_error", "stuck_mid_walk", "corpus_open_failure"):
        if status.get(key):
            raise ValueError(f"Incomplete index: {key}={status[key]}")
    if status.get("semantic_coverage", {}).get("vectors_present", 0) != 0:
        raise ValueError("Unexpected indexed vectors")


def verify_fixture(base: str, index: str) -> tuple[Json, Json]:
    """Failures/readiness gaps are not retrieval misses."""
    evidence = request(base, "/experiment/evidence")
    if evidence["embedding_calls"] != 0:
        raise ValueError("Embedding method was called")
    root = Path(evidence["corpus_root"]).resolve()
    data = Path(evidence["data_dir"]).resolve()
    if not (root / ".trusty-search-test-corpus").is_file() or (root / ".git").exists():
        raise ValueError("Unmarked or live corpus")
    if not (data / ".trusty-search-test-daemon").is_file():
        raise ValueError("Unmarked daemon data")
    if (data / "http_addr").read_text().strip() != base.removeprefix("http://"):
        raise ValueError("Daemon endpoint identity mismatch")
    status = request(base, f"/indexes/{index}/status")
    validate_complete(status)
    if Path(status["root_path"]).resolve() != root:
        raise ValueError("Wrong registered root")
    if status.get("skip_vector") is not True or status.get("skip_kg") is not False:
        raise ValueError("Expected BM25+KG with vectors disabled")
    for stage in ("lexical", "graph"):
        if status["stages"][stage]["status"] != "ready":
            raise ValueError(f"Stage {stage} is not ready")
    return evidence, status


def relevant(hit: Json, expected: Json) -> bool:
    """Require source path AND declaration/range overlap, not fuzzy text."""
    if hit.get("path") != expected["path"]:
        return False
    start, end = int(hit["start_line"]), int(hit["end_line"])
    return start <= int(expected["end_line"]) and end >= int(expected["start_line"])


def quality(hits: list[Json], expected: list[Json]) -> Json:
    ranks = [i + 1 for i, hit in enumerate(hits) if any(relevant(hit, e) for e in expected)]
    rank = min(ranks) if ranks else 0
    recalled = sum(any(relevant(hit, e) for hit in hits[:10]) for e in expected)
    duplicate = 0
    for i, hit in enumerate(hits):
        for prior in hits[:i]:
            if hit.get("path") == prior.get("path"):
                overlap = min(hit["end_line"], prior["end_line"]) - max(hit["start_line"], prior["start_line"]) + 1
                shorter = min(hit["end_line"] - hit["start_line"] + 1, prior["end_line"] - prior["start_line"] + 1)
                if overlap > 0 and overlap / max(shorter, 1) >= 0.5:
                    duplicate += 1
                    break
    return {"success_at_1": int(0 < rank <= 1), "success_at_5": int(0 < rank <= 5),
            "success_at_10": int(0 < rank <= 10), "mrr_at_10": 1 / rank if 0 < rank <= 10 else 0,
            "recall_at_10": recalled / len(expected), "duplicate_fraction": duplicate / max(len(hits), 1)}


def compact(hits: list[Json]) -> list[Json]:
    """Match current MCP field compaction for primary fields."""
    return [{k: hit[k] for k in COMPACT_FIELDS if k in hit} for hit in hits]


def size(value: Any) -> Json:
    serialized = json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    return {"bytes": len(serialized.encode()), "tokens": len(ENCODING.encode(serialized, disallowed_special=()))}


def percentile(values: list[float], pct: float) -> float:
    ordered = sorted(values)
    if not ordered:
        return 0.0
    position = (len(ordered) - 1) * pct
    lower = int(position)
    return ordered[lower] + (ordered[min(lower + 1, len(ordered) - 1)] - ordered[lower]) * (position - lower)


def body_for(query: Json, lane: str) -> Json:
    return {"text": query["query"], "top_k": 10, "compact": True,
            "mode": "text" if query["type"] == "documentation" else "code",
            "stage": lane, "expand_graph": lane == "graph"}


def summarize(rows: list[Json]) -> Json:
    out: Json = {}
    for lane in ("lexical", "graph"):
        groups = {"all": [r for r in rows if r["lane"] == lane]}
        for kind in sorted({r["type"] for r in rows}):
            groups[kind] = [r for r in rows if r["lane"] == lane and r["type"] == kind]
        out[lane] = {}
        for name, group in groups.items():
            if not group:
                continue
            metrics = {k: statistics.mean(r["quality"][k] for r in group) for k in group[0]["quality"]}
            latencies = [v for r in group for v in r["warm_ms"]]
            metrics.update({"queries": len(group), "warm_p50_ms": percentile(latencies, .5),
                            "warm_p95_ms": percentile(latencies, .95)})
            for fmt in ("full", "compact", "cards"):
                for unit in ("bytes", "tokens"):
                    metrics[f"mean_{fmt}_{unit}"] = statistics.mean(r["sizes"][fmt][unit] for r in group)
            out[lane][name] = metrics
    return out


def evaluate(base_url: str, index: str, queries: list[Json], split: str, repeats: int) -> Json:
    """Natural-language lexical/KG comparison; seed diagnostics stay separate."""
    if repeats < 1:
        raise ValueError("repeats must be positive")
    evidence, status = verify_fixture(base_url, index)
    selected = [q for q in queries if q["split"] == split]
    if not selected:
        raise ValueError("No queries in selected split")
    jobs = [(q, lane) for q in selected for lane in ("lexical", "graph")]
    random.Random(1789).shuffle(jobs)
    rows: list[Json] = []
    route = f"/indexes/{index}/search"
    for q, lane in jobs:
        start = time.perf_counter()
        response = request(base_url, route, body_for(q, lane))
        first_ms = (time.perf_counter() - start) * 1000
        meta = response.get("meta", {})
        if meta.get("bm25_lane_degraded") or meta.get("stale_index_root"):
            raise ValueError(f"Degraded measurement: {q['id']}: {meta}")
        hits = response["results"]
        cards = request(base_url, "/experiment/cards", {"index_id": index, "results": hits})["cards"]
        if len(cards) != len(hits):
            raise ValueError("Card count changed")
        for hit, card in zip(hits, cards):
            if any(hit[key] != card[key] for key in ("path", "start_line", "end_line", "score")):
                raise ValueError("Card transform changed hit identity/order")
        rows.append({"id": q["id"], "type": q["type"], "query": q["query"], "lane": lane,
                     "expected": q["expected"], "first_ms": first_ms, "warm_ms": [], "repeat_responses": [],
                     "quality": quality(hits, q["expected"]),
                     "sizes": {"full": size(hits), "compact": size(compact(hits)), "cards": size(cards)},
                     "cards": cards, "response": response})
    lookup = {(r["id"], r["lane"]): r for r in rows}
    for repetition in range(repeats):
        random.Random(1900 + repetition).shuffle(jobs)
        for q, lane in jobs:
            start = time.perf_counter()
            response = request(base_url, route, body_for(q, lane))
            duration = (time.perf_counter() - start) * 1000
            row = lookup[(q["id"], lane)]
            meta = response.get("meta", {})
            if meta.get("bm25_lane_degraded") or meta.get("stale_index_root"):
                raise ValueError(f"Degraded repeat measurement: {q['id']}: {meta}")
            if [h["id"] for h in response["results"]] != [h["id"] for h in row["response"]["results"]]:
                raise ValueError(f"Nondeterministic result order {q['id']} {lane}")
            row["warm_ms"].append(duration)
            row["repeat_responses"].append(response)
    final_evidence, _ = verify_fixture(base_url, index)
    return {"split": split, "repeats": repeats, "tokenizer": "cl100k_base",
            "evidence": evidence, "final_evidence": final_evidence, "status": status,
            "summary": summarize(rows), "rows": rows}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", required=True)
    parser.add_argument("--index", default="experiment")
    parser.add_argument("--queries", type=Path, required=True)
    parser.add_argument("--split", choices=("tuning", "held_out"), default="tuning")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    result = evaluate(args.url, args.index, json.loads(args.queries.read_text()), args.split, args.repeats)
    result["queries_sha256"] = hashlib.sha256(args.queries.read_bytes()).hexdigest()
    args.out.write_text(json.dumps(result, indent=2))
    print(json.dumps(result["summary"], indent=2))


if __name__ == "__main__":
    main()
