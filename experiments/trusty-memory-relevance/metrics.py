"""Independent actual-packet verification and duplicate-aware semantic usefulness."""
from __future__ import annotations
from statistics import mean
from typing import cast
import tiktoken
from legacy import Evidence, Gold, Source, Packet, RustHelper, JSON, IntegrityError, eligible_facts, validate_packet
from contracts import Task, Demand, semantic_key, Support


def coverage(evidence: tuple[Evidence, ...], gold: Gold, classes: dict[str, set[str]]) -> float | None:
    represented = set().union(*(classes[e.id] for e in evidence if not e.fact.standing)) if evidence else set()
    required = [set(group) for group in gold.required]
    return sum(bool(group & represented) for group in required)/len(required) if required else None


def evaluate(packet: Packet, task: Task, sources: tuple[Source, ...], gold: Gold,
             candidates: tuple[Evidence, ...], selected: tuple[Evidence, ...], supports: tuple[Support, ...],
             budget: int, helper: RustHelper, encoding: tiktoken.Encoding,
             provenance: dict[str, tuple[str, ...]], demands: tuple[Demand, ...]) -> dict[str, JSON]:
    validate_packet(packet, sources, task.legacy_query(), treatment='bm25', budget=budget, helper=helper, encoding=encoding)
    eligible = eligible_facts(sources, task.legacy_query())
    mapping = {e.id: e for e in eligible}
    if any(e.id not in mapping for e in (*packet.evidence, *candidates, *selected)):
        raise IntegrityError('ineligible evidence reached selection or packet')
    grouped: dict[tuple[str | bool | None, ...], set[str]] = {}
    for e in eligible:
        grouped.setdefault(semantic_key(e), set()).add(e.id)
    classes = {e.id: grouped[semantic_key(e)] for e in eligible}
    for representative, members in provenance.items():
        if representative not in mapping or any(member not in classes[representative] for member in members):
            raise IntegrityError('invalid duplicate provenance class')
    task_facts = [e for e in packet.evidence if not e.fact.standing]
    unique = {semantic_key(e) for e in task_facts}
    acceptable = set(gold.acceptable)
    useful = {semantic_key(e) for e in task_facts if classes.get(e.id, {e.id}) & acceptable}
    recall = coverage(tuple(task_facts), gold, classes)
    precision = len(useful)/len(task_facts) if task_facts else (1.0 if gold.expected_empty else 0.0)
    f1 = 2*precision*recall/(precision+recall) if recall is not None and precision+recall else (0.0 if recall is not None else None)
    ids = {e.id for e in packet.evidence}
    expected_standing = {e.id for e in eligible if e.fact.standing}
    standing = {e.id for e in packet.evidence if e.fact.standing}
    requested = {(i, entity) for i, demand in enumerate(demands)
        if demand.status not in {'negated_demand', 'hypothetical'}
        for entity in (demand.entities or ('',))}
    supported = {(s.demand_index, s.entity) for s in supports} & requested
    completed = {(s.demand_index, s.entity) for s in supports if set(s.members) <= ids} & requested
    # Cleanup may replace an alias assertion with exact eligible equivalent evidence.
    represented_ids = set().union(*(classes[identity] for identity in ids)) if ids else set()
    completed = {binding for binding in completed if demands[binding[0]].intent != 'alias'
        or set(demands[binding[0]].aliases) <= represented_ids}
    requested_ids = {i for i, _ in requested}
    supported_demands = {i for i in requested_ids if {b for b in requested if b[0] == i} <= supported}
    completed_demands = {i for i in requested_ids if {b for b in requested if b[0] == i} <= completed}
    retained_paths = sum(set(s.members) <= ids for s in supports)
    return {'precision': precision, 'coverage': recall, 'fact_f1': f1,
        'all_required': recall == 1 if recall is not None else None,
        'candidate_coverage': coverage(candidates, gold, classes),
        'postselection_coverage': coverage(selected, gold, classes),
        'standing_coverage': len(standing & expected_standing)/len(expected_standing) if expected_standing else 1.0,
        'standing_expected': len(expected_standing), 'task_assertions': len(task_facts),
        'unique_task_assertions': len(unique), 'redundant_assertions': len(task_facts)-len(unique),
        'unique_useful_assertions': len(useful), 'useful_per_100_tokens': 100*len(useful)/packet.tokens if packet.tokens else 0,
        'expected_empty': gold.expected_empty, 'empty_task': not task_facts,
        'unsupported_tokens': packet.tokens-packet.prelude_tokens if gold.expected_empty else 0,
        'tokens': packet.tokens, 'prelude_tokens': packet.prelude_tokens,
        'stale': len(ids-set(mapping)), 'scope_errors': sum(e.source.scope != task.scope for e in packet.evidence),
        'forbidden': len(ids & set(gold.forbidden)), 'requested_demands': len(requested_ids),
        'supported_demands': len(supported_demands), 'completed_demands': len(completed_demands),
        'requested_bindings': len(requested), 'supported_bindings': len(supported), 'completed_bindings': len(completed),
        'requested_binding_completion_rate': len(completed)/len(requested) if requested else None,
        'requested_demand_completion_rate': len(completed_demands)/len(requested_ids) if requested_ids else None,
        'selected_support_path_retention': retained_paths/len(supports) if supports else None}


def aggregate(rows: list[dict[str, JSON]]) -> dict[str, JSON]:
    positive = [r for r in rows if r['coverage'] is not None and not r['expected_empty']]
    negative = [r for r in rows if r['expected_empty']]
    def average(items: list[dict[str, JSON]], key: str) -> float:
        return mean(float(cast(float, r[key])) for r in items) if items else 0.0
    coverage_value = average(positive, 'coverage')
    abstention = average(negative, 'empty_task')
    return {'query_budget_cases': len(rows), 'unique_queries': len({str(r['query_id']) for r in rows}),
        'positive_cases': len(positive), 'negative_cases': len(negative),
        'positive_precision': average(positive, 'precision'), 'positive_coverage': coverage_value,
        'positive_f1': average(positive, 'fact_f1'), 'all_required': average(positive, 'all_required'),
        'candidate_coverage': average(positive, 'candidate_coverage'),
        'postselection_coverage': average(positive, 'postselection_coverage'),
        'negative_abstention': abstention, 'standing_coverage': average(rows, 'standing_coverage'),
        'mean_tokens': average(rows, 'tokens'), 'redundant_assertions': sum(cast(int, r['redundant_assertions']) for r in rows),
        'unsupported_tokens': sum(cast(int, r['unsupported_tokens']) for r in negative),
        'safety_violations': sum(cast(int, r[k]) for r in rows for k in ('stale', 'forbidden', 'scope_errors')),
        'adoption_target_met': coverage_value >= 0.9 and abstention >= 0.9}


def objective(summary: dict[str, JSON]) -> tuple[float, ...]:
    return (float(cast(int, summary['safety_violations'])),
        -(float(cast(float, summary['positive_f1']))+float(cast(float, summary['negative_abstention'])))/2,
        -float(cast(float, summary['positive_coverage'])), float(cast(float, summary['mean_tokens'])))
