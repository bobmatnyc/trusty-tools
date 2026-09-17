"""Bound directed postings and canonical support groups before allocating fact slots.

Why: #8246 requires complete bound paths and duplicate-neutral capacity.
What: Execute eligible relation plans and select at most four semantic task facts.
Test: test_inverse_composition_partial_and_flat_ablation, test_dedup_before_cap_conflicts_and_support_budget.
"""
from __future__ import annotations
from dataclasses import replace
from typing import Mapping
import plan_bridge
from contracts import CandidateSet, Selection, Support, consolidate, semantic_key
from relevance_index import RelevanceIndex
from legacy import IntegrityError
from plan_contracts import Plan, Path, Execution, ExecStatus
from plan_policy import normalize

def execute_plan(plan: Plan, index: RelevanceIndex, allowed: frozenset[str] | None = None) -> Execution:
    """Pre: valid plan and eligible index. Post: complete paths use only allowed eligible IDs."""
    paths: list[Path] = []
    statuses: list[ExecStatus] = []
    reasons: list[tuple[str, str]] = []
    counters = {'index_probes': 0, 'examined_assertions': 0, 'candidate_membership_misses': 0,
        'visited_bindings': 0, 'emitted_semantic_facts': 0, 'seeds': 0}
    seeds: set[str] = set()
    visited: set[tuple[int, int, str]] = set()
    emitted: set[tuple[str | bool | None, ...]] = set()
    for number, request in enumerate(plan.requests):
        if request.status != 'ready':
            statuses.append('not_run')
            continue
        if not request.steps or len(request.steps) > 2:
            raise IntegrityError('ready request requires one or two steps')
        if number >= 4:
            statuses.append('bounded')
            reasons.append((str(number), 'request_limit'))
            continue
        bounded = missing = False
        before = len(paths)
        excluded = {target for ref in request.excluded_targets for target in ref.targets}
        frontier: list[tuple[str, str, tuple[str, ...], tuple[str, ...]]] = []
        for ref in request.roots:
            for root in ref.targets:
                if root not in seeds and len(seeds) >= 3:
                    bounded = True
                    reasons.append((str(number), 'seed_limit'))
                    continue
                seeds.add(root)
                if allowed is not None and not set(ref.alias_ids) <= allowed:
                    missing = True
                    counters['candidate_membership_misses'] += len(set(ref.alias_ids)-allowed)
                    reasons.extend((identity, 'candidate_path_loss') for identity in sorted(set(ref.alias_ids)-allowed))
                    continue
                frontier.append((root, root, (root,), ref.alias_ids))
        for step_number, step in enumerate(request.steps):
            next_frontier: list[tuple[str, str, tuple[str, ...], tuple[str, ...]]] = []
            final = step_number == len(request.steps)-1
            for root, entity, bindings, members in frontier:
                binding = (number, step_number, entity)
                if binding not in visited and len(visited) >= 32:
                    bounded = True
                    reasons.append((str(number), 'visited_limit'))
                    continue
                visited.add(binding)
                counters['index_probes'] += 1
                matched = False
                for evidence in index.postings.get((entity, step.predicate, step.direction), ()):
                    fact = evidence.fact
                    target = fact.subject if step.direction == 'in' else fact.object_entity
                    qualified = not fact.standing and (not step.qualifier or any(
                        step.qualifier in normalize(v) for v in (fact.object, fact.claim)))
                    excluded_path = final and ((target or entity) in excluded or step.predicate in request.excluded_outputs)
                    if allowed is not None and evidence.id not in allowed:
                        counters['candidate_membership_misses'] += 1
                        if qualified and not excluded_path:
                            missing = True
                            reasons.append((evidence.id, 'candidate_path_loss'))
                        elif excluded_path:
                            matched = True
                        continue
                    if counters['examined_assertions'] >= 128:
                        bounded = True
                        reasons.append((str(number), 'scan_limit'))
                        break
                    counters['examined_assertions'] += 1
                    if not qualified:
                        continue
                    if excluded_path:
                        matched = True
                        continue
                    if evidence.id in members:
                        continue
                    group = tuple(dict.fromkeys((*members, evidence.id)))
                    if not final:
                        if target is not None:
                            next_frontier.append((root, target, (*bindings, target), group))
                            matched = True
                        continue
                    semantic = {semantic_key(index.evidence[i]) for i in group}
                    if len(emitted | semantic) > 32:
                        bounded = True
                        reasons.append((str(number), 'emitted_limit'))
                        continue
                    emitted.update(semantic)
                    bound = (*bindings, target) if target is not None else bindings
                    paths.append(Path(number, root, bound, evidence.id, group))
                    matched = True
                if not matched:
                    missing = True
            frontier = next_frontier
        statuses.append('bounded' if bounded else ('no_evidence' if len(paths) == before else
            ('partial' if missing else 'complete')))
    counters.update(visited_bindings=len(visited), emitted_semantic_facts=len(emitted), seeds=len(seeds))
    return Execution(tuple(dict.fromkeys(paths)), tuple(statuses), counters, tuple(dict.fromkeys(reasons)))

def select_plan(plan: Plan, execution: Execution, candidates: CandidateSet,
                index: RelevanceIndex) -> tuple[Selection, Mapping[str, tuple[str, ...]]]:
    """Pre: paths eligible. Post: no partial support group and at most four semantic facts."""
    if any(e.id not in index.evidence for e in candidates.evidence):
        raise IntegrityError('ineligible candidate')
    collapsed = consolidate(tuple(e for e in candidates.evidence if not e.fact.standing))
    mapping = {member: representative for representative, members in collapsed.members.items() for member in members}
    ranks = {e.id: rank for rank, e in enumerate(collapsed.representatives)}
    groups: list[Support] = []
    reasons = list(execution.reasons)
    for path in execution.paths:
        if not set(path.members) <= mapping.keys() or path.endpoint not in mapping:
            reasons.append((path.endpoint, 'no_complete_candidate_path'))
            continue
        groups.append(Support(mapping[path.endpoint], tuple(dict.fromkeys(mapping[i] for i in path.members)),
            path.request_index, path.root))
    groups.sort(key=lambda s: (s.demand_index, min(ranks[i] for i in s.members), len(s.members), s.members))
    chosen: set[str] = set()
    supports: list[Support] = []
    for group in dict.fromkeys(groups):
        if len(chosen | set(group.members)) > 4:
            reasons.append((str(group.demand_index), 'fact_limit'))
            continue
        chosen.update(group.members)
        supports.append(group)
    evidence = tuple(e for e in collapsed.representatives if e.id in chosen)
    return Selection(evidence, tuple(supports), tuple(dict.fromkeys(reasons))), {
        identity: members for identity, members in collapsed.members.items() if identity in chosen}

def selection_status(execution: Execution, selection: Selection) -> Execution:
    statuses = list(execution.per_request)
    for identity, reason in selection.rejections:
        if reason == 'fact_limit':
            statuses[int(identity)] = 'bounded'
    return replace(execution, per_request=tuple(statuses))
