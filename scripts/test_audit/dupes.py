"""dupes.py — rule R1, duplicate and near-duplicate test bodies (#11268).

Why: a test whose body repeats another's adds run time without coverage, and
  a near copy is a table-test candidate. Both must be found without a model
  and without an all-pairs comparison over ~43k tests.
What: `duplicate_rules(tests)` returns `{key: detail}`.
  - Identical: two tests with the same `body_only_sha` (body tokens only).
  - Near-identical: Jaccard similarity of the bodies' token 3-shingle sets
    >= `threshold` (default 0.85), for bodies of at least `min_tokens`
    tokens (default 40) that are not already identical. Candidates come
    from a prefix-filter similarity join (each set ordered rarest shingle
    first; two sets reaching the threshold share a prefix shingle), then
    every candidate pair is verified exactly. Deterministic: no hashing seed.
  The detail names up to three partner keys, sorted.
Test: scripts/test_audit/tests/test_rules.py
"""
from __future__ import annotations

import math
from collections import Counter, defaultdict

from test_audit.extract import TestFn

SHINGLE = 3
THRESHOLD = 0.85
MIN_TOKENS = 40


def shingles(test: TestFn) -> frozenset[str]:
    texts = [t.text for t in test.body]
    return frozenset("\x1f".join(texts[k:k + SHINGLE]) for k in range(len(texts) - SHINGLE + 1))


def jaccard(a: frozenset[str], b: frozenset[str]) -> float:
    if not a and not b:
        return 1.0
    return len(a & b) / len(a | b)


def _partners(text: str, others: list[str]) -> str:
    shown = ", ".join(others[:3])
    more = f" (+{len(others) - 3} more)" if len(others) > 3 else ""
    return f"{text} {shown}{more}"


def near_pairs(sets: dict[str, frozenset[str]], threshold: float) -> dict[str, dict[str, float]]:
    """All pairs of keys whose sets reach `threshold` Jaccard, via a prefix filter."""
    freq: Counter[str] = Counter()
    for s in sets.values():
        freq.update(s)
    order = sorted(sets, key=lambda k: (len(sets[k]), k))
    index: dict[str, list[str]] = defaultdict(list)
    found: dict[str, dict[str, float]] = defaultdict(dict)
    for key in order:
        s = sets[key]
        ranked = sorted(s, key=lambda x: (freq[x], x))
        prefix = ranked[:len(s) - math.ceil(threshold * len(s)) + 1]
        seen: set[str] = set()
        for sh in prefix:
            for other in index[sh]:
                if other in seen:
                    continue
                seen.add(other)
                if len(sets[other]) < threshold * len(s):
                    continue
                sim = jaccard(s, sets[other])
                if sim >= threshold:
                    found[key][other] = sim
                    found[other][key] = sim
        for sh in prefix:
            index[sh].append(key)
    return found


def duplicate_rules(tests: list[TestFn], threshold: float = THRESHOLD,
                    min_tokens: int = MIN_TOKENS) -> dict[str, str]:
    by_sha: dict[str, list[str]] = defaultdict(list)
    for t in tests:
        by_sha[t.body_only_sha].append(t.key)
    out: dict[str, str] = {}
    for keys in by_sha.values():
        if len(keys) > 1:
            for k in keys:
                out[k] = _partners("identical body to", sorted(x for x in keys if x != k))
    # One representative per distinct body for the near pass.
    rep: dict[str, TestFn] = {}
    for t in sorted(tests, key=lambda t: t.key):
        if len(t.body) >= min_tokens:
            rep.setdefault(t.body_only_sha, t)
    sets = {t.key: shingles(t) for t in rep.values()}
    near = near_pairs(sets, threshold)
    sha_of = {t.key: t.body_only_sha for t in tests}
    for key, partners in near.items():
        members = [x for x in by_sha[sha_of[key]]]
        others = sorted(k for p in partners for k in by_sha[sha_of[p]])
        best = max(partners.values())
        detail = _partners(f"near-identical (Jaccard {best:.2f}) to", others)
        for m in members:
            out[m] = f"{out[m]}; {detail}" if m in out else detail
    return out
