"""Source-labelled retrieval metrics; unknown relevance never becomes a success."""
from __future__ import annotations

from typing import Any

Json = dict[str, Any]


def score(query: Json, hits: list[Json]) -> Json:
    """Separate source recall, temporal errors, abstention, and stale excerpts."""
    ids = [str(hit['id']) for hit in hits]
    expected = set(query['relevant_ids'])
    invalid = set(query.get('invalid_ids', []))
    forbidden = set(query.get('forbidden_ids', []))
    found = expected.intersection(ids)
    ranks = [i + 1 for i, ident in enumerate(ids) if ident in expected]
    required = query.get('required_evidence', {})
    prohibited = query.get('forbidden_evidence', {})
    evidence_errors = []
    for hit in hits:
        excerpt = str(hit['excerpt']).casefold()
        ident = str(hit['id'])
        if any(str(term).casefold() not in excerpt for term in required.get(ident, [])):
            evidence_errors.append(ident)
        if any(str(term).casefold() in excerpt for term in prohibited.get(ident, [])):
            evidence_errors.append(ident)
    return {
        'id': query['id'], 'split': query['split'], 'scenario': query['scenario'],
        'category': query['category'], 'mode': query['mode'], 'returned_ids': ids,
        'answerable': bool(expected), 'hit_at_5': bool(found),
        'recall_at_5': len(found) / len(expected) if expected else None,
        'reciprocal_rank': 1 / min(ranks) if ranks else 0.0,
        'invalid_hits': sorted(invalid.intersection(ids)),
        'forbidden_hits': sorted(forbidden.intersection(ids)),
        'scope_errors': [h['id'] for h in hits if h['scope'] != query['scope']],
        'evidence_errors': sorted(set(evidence_errors)),
        'duplicate_hits': len(ids) - len(set(ids)),
        'abstained': not hits if query.get('expected_empty', False) else None,
        'index_stale_hits': sum(not h['index_fresh'] for h in hits),
    }


def summarize(rows: list[Json]) -> Json:
    """Use answerable queries only for recall/MRR; report empty-label cases separately."""
    answerable = [r for r in rows if r['answerable']]
    empty = [r for r in rows if r['abstained'] is not None]
    current = [r for r in rows if r['mode'] == 'current']
    history = [r for r in rows if r['mode'] == 'asof']
    return {
        'queries': len(rows), 'answerable': len(answerable),
        'hits_at_5': sum(r['hit_at_5'] for r in answerable),
        'mean_recall_at_5': sum(r['recall_at_5'] for r in answerable) / len(answerable) if answerable else None,
        'mrr': sum(r['reciprocal_rank'] for r in answerable) / len(answerable) if answerable else None,
        'invalid_queries': sum(bool(r['invalid_hits']) for r in rows),
        'current_stale_queries': sum(bool(r['invalid_hits']) for r in current),
        'current_queries': len(current),
        'historical_error_queries': sum(bool(r['invalid_hits']) for r in history),
        'historical_queries': len(history),
        'scope_errors': sum(len(set(r['scope_errors']) | set(r['forbidden_hits'])) for r in rows),
        'scope_error_queries': sum(bool(r['scope_errors'] or r['forbidden_hits']) for r in rows),
        'evidence_errors': sum(len(r['evidence_errors']) for r in rows),
        'duplicate_hits': sum(r['duplicate_hits'] for r in rows),
        'empty_label_queries': len(empty), 'correct_abstentions': sum(r['abstained'] for r in empty),
        'index_stale_hits': sum(r['index_stale_hits'] for r in rows),
    }
