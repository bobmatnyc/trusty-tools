"""Query-only adapters, frozen before held-out evaluation."""
from __future__ import annotations
import re
from typing import Any


def normalize(query: str) -> str:
    match = re.fullmatch(r'\s*where\s+is\s+([A-Za-z_][A-Za-z0-9_]*)\s+defined\??\s*', query, re.I)
    return f'fn {match.group(1)}' if match else query


def adapted_queries(queries: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [{**q, 'original_query': q['query'], 'query': normalize(q['query'])} for q in queries]
