"""Three fixed treatments reuse native lexical retrieval and the immutable packer.

Why: #8246 needs a matched candidate pool between flat and bound selection.
What: Keep the old arm unchanged; share only candidate IDs, never structured supports.
Test: test_inverse_composition_partial_and_flat_ablation.
"""
from __future__ import annotations
from dataclasses import replace
from time import perf_counter_ns
import plan_bridge
from contracts import Task, CandidateSet, SelectorPolicy
from relevance_index import RelevanceIndex
from relevance import retrieve, intervention
from legacy import RustHelper, Evidence, array, obj, string, IntegrityError
from plan_contracts import Arm, ARMS, Plan, Execution, Request, RunResult
from plan_policy import parse_plan
from plan_execute import execute_plan, select_plan, selection_status
from plan_index import validate_context

def retrieve_shared(task: Task, plan: Plan, index: RelevanceIndex, helper: RustHelper) -> CandidateSet:
    """Return lexical plus plan-directed candidates with identical ordering for both repairs."""
    validate_context(task, index)
    started = perf_counter_ns()
    response = helper.request({'op': 'search', 'projection': index.claim_id, 'text': task.prompt, 'limit': 20})
    lexical = [index.evidence[string(obj(value)['id'])] for value in array(response['hits'])]
    fallback_count = 0
    if index.missing_sources:
        fallback = helper.request({'op': 'search', 'projection': index.fallback_id, 'text': task.prompt, 'limit': 20})
        for value in array(fallback['hits']):
            lexical.extend(index.by_source[string(obj(value)['id'])])
            fallback_count += 1
    lexical_ns = perf_counter_ns()-started
    tick = perf_counter_ns()
    execution = execute_plan(plan, index)
    graph = [index.evidence[i] for i in dict.fromkeys(m for p in execution.paths for m in p.members)]
    graph_ns = perf_counter_ns()-tick
    scores: dict[str, float] = {}
    evidence: dict[str, Evidence] = {}
    for lane in (lexical, graph):
        for rank, item in enumerate(lane, 1):
            scores[item.id] = scores.get(item.id, 0)+1/(60+rank)
            evidence[item.id] = item
    ordered = tuple(evidence[i] for i in sorted(evidence, key=lambda i: (-scores[i], i)))
    return CandidateSet(ordered, (), {'bm25_ns': lexical_ns, 'graph_ns': graph_ns,
        'retrieval_ns': perf_counter_ns()-started}, {'lexical_fact_candidates': len(lexical),
        'graph_fact_candidates': len(graph), 'fallback_source_hits': fallback_count,
        **execution.counters}, execution.reasons)

def flat_plan(plan: Plan) -> Plan:
    """Flatten every relation to the explicit root; retain only the old inverse-dependency template."""
    requests: list[Request] = []
    for request in plan.requests:
        for step in request.steps or (None,):
            if step is None:
                requests.append(request)
            else:
                direction = step.direction if step.predicate == 'depends_on' else 'out'
                requests.append(replace(request, steps=(replace(step, direction=direction),)))
    return Plan(tuple(requests), plan.diagnostics)

def run_arm(task: Task, arm: Arm, index: RelevanceIndex, helper: RustHelper) -> RunResult:
    """Pre: known arm and scoped index. Post: evidence is eligible and fully supported."""
    validate_context(task, index)
    if arm not in ARMS:
        raise IntegrityError('unknown arm')
    if arm == 'old_combined':
        candidates = retrieve(task, 'combined', index, helper, SelectorPolicy(1, 4))
        selection, old_provenance = intervention(task, 'combined', candidates, index, SelectorPolicy(1, 4))
        return RunResult(Plan((), ()), Execution((), (), {}, ()), candidates, selection, old_provenance)
    tick = perf_counter_ns()
    plan = parse_plan(task, index)
    parse_ns = perf_counter_ns()-tick
    candidates = retrieve_shared(task, plan, index, helper)
    tick = perf_counter_ns()
    selected_plan = flat_plan(plan) if arm == 'defect_fixes' else plan
    execution = execute_plan(selected_plan, index, frozenset(e.id for e in candidates.evidence))
    selection, provenance = select_plan(selected_plan, execution, candidates, index)
    execution = selection_status(execution, selection)
    candidates.timings.update(parse_ns=parse_ns, selector_ns=perf_counter_ns()-tick)
    return RunResult(selected_plan, execution, candidates, selection, provenance)
