"""Unique graph-name probe with lexical fallback and checked source seeks."""
from __future__ import annotations

import time
from typing import Any
from urllib.parse import quote, urlencode

from benchmark import request
import kg_seek_adapter
from kg_seek_adapter import chunk_path, predecessor, query_intent


def execute(base: str, index: str, text: str) -> dict[str, Any] | None:
    """Probe unambiguous names; preserve full fallback cost and trace."""
    intent = query_intent(text)
    if intent is None:
        return None
    started = time.perf_counter()
    symbol, direction = intent
    prefix = f"/indexes/{quote(index, safe='')}"
    trace: list[dict[str, Any]] = []

    def call(path: str) -> dict[str, Any]:
        response = request(base, path)
        trace.append({"method": "GET", "path": path, "body": None, "response": response})
        return response

    probe = call(prefix + "/graph/neighbors?" + urlencode({
        "node": symbol, "direction": direction, "edge_kinds": "CallsFunction", "max_hops": 1}))
    selected = sorted({neighbor['chunk_id'] for neighbor in probe.get('neighbors', [])
                       if neighbor.get('chunk_id') and neighbor.get('edge') == 'CallsFunction'})[:10]
    if not selected:
        fallback = kg_seek_adapter.execute(base, index, text)
        if fallback is None:
            raise ValueError("Recognized query unexpectedly refused by fallback")
        elapsed_ms = (time.perf_counter() - started) * 1000
        return {**fallback, "trace": trace + fallback['trace'], "elapsed_ms": elapsed_ms,
                "latency_ms": elapsed_ms, "name_probe": "fallback"}

    results = []
    for rank, cid in enumerate(selected, 1):
        page = call(prefix + '/chunks?' + urlencode({'after': predecessor(cid), 'limit': 1}))
        chunks = page.get('chunks')
        if not isinstance(chunks, list) or len(chunks) != 1 or chunks[0].get('id') != cid:
            raise ValueError(f"Indexed chunk seek did not return exact ID: {cid}")
        chunk = chunks[0]
        if not isinstance(chunk.get('file'), str) or not chunk['file']:
            raise ValueError('Materialized CodeChunk requires file')
        content = chunk['content']
        lines = content.split('\n')
        if lines and lines[-1] == '':
            lines.pop()
        lines = [line[:-1] if line.endswith('\r') else line for line in lines]
        snippet = content if len(lines) <= 7 else '\n'.join(lines[:7])
        results.append({**chunk, 'id': cid, 'path': chunk_path(cid),
                        'compact_snippet': snippet, 'score': float(11-rank),
                        'match_reason': 'explicit_kg'})
    elapsed_ms = (time.perf_counter() - started) * 1000
    return {'results': results, 'trace': trace, 'elapsed_ms': elapsed_ms,
            'latency_ms': elapsed_ms, 'intent': 'explicit_kg', 'seed_symbol': symbol,
            'direction': direction, 'seed_count': 1, 'name_probe': 'direct'}
