"""Fact metrics derive from exact emitted evidence, independently of retrieval lanes."""
from __future__ import annotations
from statistics import mean
from typing import cast
from records import Gold, Query, Source, JSON, IntegrityError, eligible_facts
from retrieval import Packet
from adapters import RustHelper
from packet_integrity import validate_packet
import tiktoken


def evaluate(packet: Packet, gold: Gold, sources: tuple[Source, ...], query: Query, *,
             treatment: str, budget: int, helper: RustHelper, encoding: tiktoken.Encoding) -> dict[str, JSON]:
    validate_packet(packet, sources, query, treatment=treatment, budget=budget, helper=helper, encoding=encoding)
    eligible = {e.id for e in eligible_facts(sources, query)}
    ids = [e.id for e in packet.evidence]
    task = {e.id for e in packet.evidence if not e.fact.standing}
    standing = {e.id for e in packet.evidence if e.fact.standing}
    standing_all = {f'{s.key}|{s.revision}|{f.fact_id}' for s in sources for f in s.facts if f.standing}
    required = [set(group) - standing_all for group in gold.required if set(group) - standing_all]
    accepted = set(gold.acceptable) - standing_all
    covered = sum(bool(group & task) for group in required)
    coverage = covered / len(required) if required else None
    precision = len(task & accepted) / len(task) if task else (1.0 if gold.expected_empty else 0.0)
    f1 = (2 * precision * coverage / (precision + coverage) if precision + coverage else 0.0) if coverage is not None else None
    standing_accept = set(gold.acceptable) & standing_all & eligible
    return {'query_id': query.id, 'category': query.category, 'task_required_groups': len(required),
        'covered_groups': covered, 'coverage': coverage, 'all_required': covered == len(required) if required else None,
        'precision': precision, 'fact_f1': f1, 'task_facts': len(task), 'standing_facts': len(standing),
        'standing_expected_facts': len(standing_accept),
        'standing_coverage': len(standing & standing_accept) / len(standing_accept) if standing_accept else None,
        'standing_precision': len(standing & standing_accept) / len(standing) if standing else (0.0 if standing_accept else None),
        'forbidden_facts': len(set(ids) & set(gold.forbidden)), 'stale_facts': len(set(ids) - eligible),
        'scope_errors': sum(e.source.scope != query.scope for e in packet.evidence),
        'expected_empty': gold.expected_empty, 'empty_task': not task,
        'unsupported_tokens': max(0, packet.tokens - packet.prelude_tokens) if gold.expected_empty else 0,
        'tokens': packet.tokens, 'prelude_tokens': packet.prelude_tokens,
        'useful_facts_per_100_tokens': 100 * len(task & accepted) / packet.tokens if packet.tokens else 0.0,
        'unique_evidence': len(ids)}


def aggregate(rows: list[dict[str, JSON]]) -> dict[str, JSON]:
    task_rows = [r for r in rows if r['category'] != 'standing']
    answerable = [r for r in task_rows if r['coverage'] is not None]
    empty = [r for r in task_rows if r['expected_empty']]
    return {'queries': len(rows), 'answerable': len(answerable), 'empty_queries': len(empty),
        'macro_fact_f1': mean(cast(float, r['fact_f1']) for r in answerable) if answerable else 0.0,
        'coverage': mean(cast(float, r['coverage']) for r in answerable) if answerable else 0.0,
        'precision': mean(cast(float, r['precision']) for r in task_rows) if task_rows else 0.0,
        'all_required_rate': mean(bool(r['all_required']) for r in answerable) if answerable else 0.0,
        'empty_rate': mean(bool(r['empty_task']) for r in empty) if empty else None,
        'mean_tokens': mean(cast(int, r['tokens']) for r in rows) if rows else 0,
        'unsupported_tokens': sum(cast(int, r['unsupported_tokens']) for r in empty),
        'scope_errors': sum(cast(int, r['scope_errors']) for r in rows),
        'stale_facts': sum(cast(int, r['stale_facts']) for r in rows),
        'forbidden_facts': sum(cast(int, r['forbidden_facts']) for r in rows)}


def objective(summary: dict[str, JSON]) -> tuple[float, ...]:
    return (float(cast(int, summary['scope_errors'])) + float(cast(int, summary['stale_facts'])),
        -float(cast(float, summary['macro_fact_f1'])), -float(cast(float, summary['coverage'])),
        float(cast(float, summary['mean_tokens'])))
