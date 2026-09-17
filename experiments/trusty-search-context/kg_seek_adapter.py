"""Explicit KG adapter with checked one-row indexed source seeks."""
from __future__ import annotations

import re
import time
from typing import Any
from urllib.parse import quote, urlencode

from benchmark import request

_SYMBOL = r"([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)"


def query_intent(text: str) -> tuple[str, str] | None:
    """Extract only explicit call-direction syntax, without labels."""
    for pattern, direction in ((rf"\bdoes\s+{_SYMBOL}\s+call\b", "out"),
                               (rf"\bcalls\s+{_SYMBOL}\b", "in")):
        match = re.search(pattern, text, re.IGNORECASE)
        if match:
            return match.group(1), direction
    return None


def chunk_path(chunk_id: str) -> str:
    """Read current named/positional ID suffixes; reject unknown shapes."""
    base = re.sub(r"(?:::dup::\d+)?(?:::sub::\d+)?$", "", chunk_id)
    named = re.fullmatch(r"(.+?)::[^:]+::.+::\d+::\d+", base)
    if named:
        return named.group(1)
    positional = re.fullmatch(r"(.+):\d+:\d+", base)
    if positional:
        return positional.group(1)
    raise ValueError(f"Unrecognized chunk ID: {chunk_id}")


def predecessor(chunk_id: str) -> str:
    """Seek below an ASCII terminal digit; verify the resulting ID afterward.

    For IDs sharing the numeric suffix prefix, P+(d-1)+U+10FFFF sorts
    after every valid earlier numeric sibling and before P+d. UTF-8 and
    Rust string ordering preserve this scalar ordering. Unusual intervening
    file names remain possible; exact-ID validation fails closed for those.
    """
    chunk_path(chunk_id)
    if not chunk_id or chunk_id[-1] not in "0123456789":
        raise ValueError("Chunk ID must end in an ASCII decimal digit")
    return chunk_id[:-1] + chr(ord(chunk_id[-1]) - 1) + "\U0010ffff"


def execute(base: str, index: str, text: str) -> dict[str, Any] | None:
    """Resolve lexical seeds, traverse direct calls, materialize exact chunks."""
    intent = query_intent(text)
    if intent is None:
        return None
    started = time.perf_counter()
    symbol, direction = intent
    prefix = f"/indexes/{quote(index, safe='')}"
    trace: list[dict[str, Any]] = []

    def call(path: str, body: dict[str, Any] | None = None) -> dict[str, Any]:
        response = request(base, path, body)
        trace.append({"method": "GET" if body is None else "POST",
                      "path": path, "body": body, "response": response})
        return response

    lexical = call(prefix + "/search", {"text": symbol, "top_k": 3,
                   "stage": "lexical", "compact": True, "expand_graph": False, "mode": "code"})
    meta = lexical.get("meta", {})
    for flag in ("bm25_lane_degraded", "stale_index_root"):
        if meta.get(flag):
            raise ValueError(f"Seed search is unusable: {flag}")
    seeds = [hit for hit in lexical.get("results", [])
             if hit.get("function_name") == symbol][:3]
    selected: list[str] = []
    seen: set[str] = set()
    seed_keys: set[str] = set()
    for seed in seeds:
        path = seed.get("path") or chunk_path(seed["id"])
        key = f"{path}::{symbol}"
        if key in seed_keys:
            continue
        seed_keys.add(key)
        neighbors = call(prefix + "/graph/neighbors?" + urlencode({
            "node": key, "direction": direction, "edge_kinds": "CallsFunction", "max_hops": 1}))
        for neighbor in sorted(neighbors.get("neighbors", []), key=lambda h: h.get("chunk_id", "")):
            cid = neighbor.get("chunk_id")
            if cid and cid not in seen and neighbor.get("edge") == "CallsFunction":
                selected.append(cid)
                seen.add(cid)
                if len(selected) == 10:
                    break
        if len(selected) == 10:
            break

    materialized: dict[str, dict[str, Any]] = {}
    for cid in selected:
        page = call(prefix + "/chunks?" + urlencode({"after": predecessor(cid), "limit": 1}))
        chunks = page.get("chunks")
        if not isinstance(chunks, list) or len(chunks) != 1 or chunks[0].get("id") != cid:
            raise ValueError(f"Indexed chunk seek did not return exact ID: {cid}")
        materialized[cid] = chunks[0]

    results = []
    for rank, cid in enumerate(selected, 1):
        chunk = materialized[cid]
        if not isinstance(chunk.get("file"), str) or not chunk["file"]:
            raise ValueError("Materialized CodeChunk requires file")
        content = chunk["content"]
        # Match Rust str::lines and build_compact_snippet: no added ellipsis.
        lines = content.split("\n")
        if lines and lines[-1] == "":
            lines.pop()
        lines = [line[:-1] if line.endswith("\r") else line for line in lines]
        snippet = content if len(lines) <= 7 else "\n".join(lines[:7])
        results.append({**chunk, "id": cid, "path": chunk_path(cid),
                        "compact_snippet": snippet,
                        "score": float(11 - rank), "match_reason": "explicit_kg"})
    elapsed_ms = (time.perf_counter() - started) * 1000
    return {"results": results, "trace": trace, "elapsed_ms": elapsed_ms,
            "latency_ms": elapsed_ms, "intent": "explicit_kg",
            "seed_symbol": symbol, "direction": direction, "seed_count": len(seed_keys)}
