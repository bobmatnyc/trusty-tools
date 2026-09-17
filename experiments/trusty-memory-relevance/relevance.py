"""Independent candidate ablations, selector-only validation, and group-safe packets."""
from __future__ import annotations
from time import perf_counter_ns
from typing import cast
import tiktoken
from legacy import Evidence, RustHelper, JSON, IntegrityError, graph_lookup, array, obj, string, Retrieval, Packet, pack
from records import Policy
from contracts import Task, Demand, SelectorPolicy, CandidateSet, Selection, Support, ARMS, consolidate
from relevance_index import RelevanceIndex
from query_policy import parse_intents, words, STOP
from relation_support import relation_lookup


def retrieve(task: Task, arm: str, index: RelevanceIndex, helper: RustHelper, policy: SelectorPolicy) -> CandidateSet:
    if arm not in ARMS:
        raise IntegrityError('unknown experiment arm')
    started = perf_counter_ns()
    claim = arm in {'claim_index', 'combined'}
    tick = perf_counter_ns()
    response = helper.request({'op': 'search', 'projection': index.claim_id if claim else index.source_id,
        'text': task.prompt, 'limit': 20})
    lexical: list[Evidence] = []
    for value in array(response['hits']):
        identity = string(obj(value)['id'])
        lexical.extend((index.evidence[identity],) if claim else index.by_source[identity])
    fallback_count = 0
    if claim and index.missing_sources:
        fallback = helper.request({'op': 'search', 'projection': index.fallback_id, 'text': task.prompt, 'limit': 20})
        for value in array(fallback['hits']):
            identity = string(obj(value)['id'])
            lexical.extend(index.by_source[identity])
            fallback_count += 1
    lexical_ns = perf_counter_ns() - tick
    tick = perf_counter_ns()
    if arm in {'relation_graph', 'combined'}:
        graph = relation_lookup(task, parse_intents(task, index), index)
    else:
        old, counters = graph_lookup(task.legacy_query(), index.old, Policy())
        graph = CandidateSet(tuple(old), counters={k: v for k, v in counters.items() if type(v) is int})
    graph_ns = perf_counter_ns() - tick
    scores: dict[str, float] = {}
    mapping: dict[str, Evidence] = {}
    tick = perf_counter_ns()
    for lane in (lexical, graph.evidence):
        for rank, e in enumerate(lane, 1):
            scores[e.id] = scores.get(e.id, 0) + 1/(60+rank)
            mapping[e.id] = e
    order = sorted(mapping, key=lambda identity: (-scores[identity], identity))
    return CandidateSet(tuple(mapping[i] for i in order), graph.supports,
        {'bm25_ns': lexical_ns, 'graph_ns': graph_ns, 'fusion_ns': perf_counter_ns()-tick,
         'retrieval_ns': perf_counter_ns()-started},
        {'lexical_fact_candidates': len(lexical), 'graph_fact_candidates': len(graph.evidence),
         'missing_derived_sources': len(index.missing_sources), 'fallback_source_hits': fallback_count,
         **graph.counters}, graph.rejections)


def select_claims(task: Task, demands: tuple[Demand, ...], candidates: CandidateSet,
                  policy: SelectorPolicy, index: RelevanceIndex) -> Selection:
    """Known relations need complete candidate-only support; unknowns use frozen overlap."""
    task_candidates = tuple(e for e in candidates.evidence if not e.fact.standing)
    allowed = {e.id for e in task_candidates}
    support_result = relation_lookup(task, demands, index, allowed)
    groups = list(support_result.supports)
    reasons = list(support_result.rejections)
    for demand_index, demand in enumerate(demands):
        if demand.intent != 'unknown' or demand.status != 'unknown_intent':
            continue
        entity_words = {w for entity in demand.entities for w in words(entity)}
        query_words = set(words(demand.clause)) - STOP - entity_words
        for e in task_candidates:
            if e.fact.subject not in demand.entities:
                continue
            overlap = len(query_words & (set(words(e.fact.claim))-STOP-entity_words))
            if overlap >= policy.unknown_overlap:
                alias_path = tuple(identity for identity in demand.aliases if
                    index.evidence[identity].fact.object_entity == e.fact.subject and
                    len(index.aliases.get(index.evidence[identity].fact.subject, ())) == 1)
                if not set(alias_path) <= allowed:
                    reasons.append((e.id, 'no_complete_path'))
                    continue
                groups.append(Support(e.id, tuple(dict.fromkeys((*alias_path, e.id))), demand_index, e.fact.subject))
            else:
                reasons.append((e.id, 'below_threshold'))
    ranks = {e.id: i for i, e in enumerate(task_candidates)}
    groups.sort(key=lambda s: (min(ranks.get(i, len(ranks)) for i in s.members), s.demand_index, s.members))
    chosen: set[str] = set()
    selected_groups = []
    for group in groups:
        if len(chosen | set(group.members)) > policy.max_task_facts:
            reasons.append((group.endpoint, 'fact_limit'))
            continue
        chosen.update(group.members)
        selected_groups.append(group)
    for e in task_candidates:
        if e.id not in chosen and not any(identity == e.id for identity, _ in reasons):
            reasons.append((e.id, 'wrong_relation'))
    return Selection(tuple(e for e in task_candidates if e.id in chosen), tuple(selected_groups), tuple(reasons))


def intervention(task: Task, arm: str, candidates: CandidateSet, index: RelevanceIndex,
                 policy: SelectorPolicy) -> tuple[Selection, dict[str, tuple[str, ...]]]:
    selection = (select_claims(task, parse_intents(task, index), candidates, policy, index)
        if arm in {'selector', 'combined'} else Selection(candidates.evidence, candidates.supports, candidates.rejections))
    provenance: dict[str, tuple[str, ...]] = {e.id: (e.id,) for e in selection.evidence}
    if arm in {'cleanup', 'combined'}:
        collapsed = consolidate(selection.evidence)
        mapping = {member: representative for representative, members in collapsed.members.items() for member in members}
        supports = tuple(Support(mapping[s.endpoint], tuple(dict.fromkeys(mapping[i] for i in s.members)), s.demand_index, s.entity)
            for s in selection.supports if all(i in mapping for i in s.members))
        rejected = tuple((member, 'duplicate_assertion') for representative, members in collapsed.members.items()
            for member in members if member != representative)
        selection = Selection(collapsed.representatives, supports, selection.rejections + rejected)
        provenance = collapsed.members
    return selection, provenance


def packet(task: Task, selection: Selection, index: RelevanceIndex, budget: int,
           helper: RustHelper, encoding: tiktoken.Encoding) -> tuple[Packet, tuple[tuple[str, str], ...]]:
    """Reuse complete-claim packing, then remove detached endpoints from grouped support."""
    evidence = list(selection.evidence)
    rejected = list(selection.rejections)
    while True:
        result = pack(task.legacy_query(), Retrieval(evidence, {}, {}), index.old, budget, helper, encoding, 'bm25')
        included = {e.id for e in result.evidence}
        incomplete = {s.endpoint for s in selection.supports if s.endpoint in included and not set(s.members) <= included}
        # An independently complete path to the same endpoint is sufficient.
        incomplete -= {s.endpoint for s in selection.supports if set(s.members) <= included}
        if not incomplete:
            return result, tuple(rejected)
        rejected.extend((identity, 'incomplete_path_budget') for identity in sorted(incomplete))
        evidence = [e for e in evidence if e.id not in incomplete]
