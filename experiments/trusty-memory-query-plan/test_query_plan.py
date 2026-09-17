"""Contract tests use only private, tiny synthetic sources, never evaluation fixtures."""
from __future__ import annotations
from dataclasses import replace
from pathlib import Path
from typing import Iterator, cast
import pytest
import plan_bridge
from legacy import Source, Fact, RustHelper, IntegrityError, load_encoding, Query, Gold, JSON
from contracts import Task, CandidateSet
from maintenance import DerivedStore, maintain
from relevance_index import RelevanceIndex, build_index
from relevance import packet
from plan_policy import parse_plan
from plan_execute import execute_plan, select_plan
from plan_experiment import run_arm, retrieve_shared
from plan_evaluate import measured_case, summary, verify_manifest
from plan_contracts import Span, Request, Step, Arm
from plan_index import build_scoped_index

CLOCK = '2026-09-01T00:00:00Z'
HELPER = Path('/Users/masa/trusty-search-experiment/target/debug/examples/memory_prompt_probe')

def source(key: str, subject: str = 'Alpha', predicate: str = 'owned_by', value: str = 'Ada',
           entity: bool = False) -> Source:
    claim = f'{subject} {predicate} {value}.'
    fact = Fact('f', subject, predicate, value, value if entity else None, claim, 0,
        len(claim.encode()), '2026-01-01T00:00:00Z', None, None, False)
    return Source(key, 'test', 1, 'Note', claim, '2026-01-01T00:00:00Z', None, None, False, (fact,))

def task(prompt: str = 'owner of Alpha') -> Task:
    return Task(prompt, 'test', CLOCK, CLOCK)

@pytest.fixture
def helper() -> Iterator[RustHelper]:
    handle = RustHelper(HELPER)
    yield handle
    handle.close()

@pytest.fixture
def indexes() -> Iterator[list[RelevanceIndex]]:
    result: list[RelevanceIndex] = []
    yield result
    for index in result:
        index.close()

def index_for(sources: tuple[Source, ...], helper: RustHelper,
              indexes: list[RelevanceIndex]) -> RelevanceIndex:
    store = DerivedStore({s.key: s for s in sources})
    maintain(store, sources, (), max(1, len(sources)))
    index = build_scoped_index(sources, task(), store, helper)
    indexes.append(index)
    return index

@pytest.mark.parametrize('prompt,predicates', [
    ('owner of Alpha, not maintainer', ['owned_by']),
    ('location of Alpha; omit owner', ['located_at']),
    ('owner and location of Alpha', ['owned_by', 'located_at']),
    ('Show Alpha\'s owner once without duplicates', ['owned_by']),
    ('why was Alpha created', ['rationale']),
])
def test_positive_requests_and_exclusions(prompt: str, predicates: list[str], helper: RustHelper,
                                         indexes: list[RelevanceIndex]) -> None:
    index = index_for((source('a'),), helper, indexes)
    plan = parse_plan(task(prompt), index)
    assert not plan.diagnostics
    assert [r.steps[-1].predicate for r in plan.requests] == predicates
    assert all(r.status == 'ready' for r in plan.requests)

def test_reference_boundaries_and_independent_binding(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    index = index_for((source('a'), source('b', 'Beta'), source('t', 'Team')), helper, indexes)
    plan = parse_plan(task('owner of Alpha and location of Beta'), index)
    assert [(r.roots[0].targets, r.steps[0].predicate) for r in plan.requests] == [
        (('Alpha',), 'owned_by'), (('Beta',), 'located_at')]
    for prompt in ('owner of Alpha Observatory', 'owner of "Alpha Observatory"', 'owner of named Alpha Observatory'):
        request = parse_plan(task(prompt), index).requests[0]
        assert request.status == 'unresolved_entity' and not request.roots[0].targets
    assert parse_plan(task('owner of Alpha; its location'), index).requests[1].roots[0].targets == ('Alpha',)
    unknown = parse_plan(task('owner of Team Observatory'), index).requests[0]
    assert unknown.roots[0].span.text == 'Team Observatory' and unknown.status == 'unresolved_entity'

def test_inverse_composition_partial_and_flat_ablation(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    sources = (source('a', 'Alpha', 'maintained_by', 'Team', True),
        source('b', 'Beta', 'maintained_by', 'Team', True), source('c', 'Alpha', 'located_at', 'Paris'))
    index = index_for(sources, helper, indexes)
    query = task('location of services maintained by Team')
    structured = run_arm(query, 'structured_plan', index, helper)
    flat = run_arm(query, 'defect_fixes', index, helper)
    assert [e.id for e in flat.candidates.evidence] == [e.id for e in structured.candidates.evidence]
    assert len(structured.selection.evidence) == 2 and not flat.selection.evidence
    assert structured.execution.per_request == ('partial',)
    assert structured.execution.paths[0].bindings == ('Team', 'Alpha')

def test_alias_paths_exclusions_and_ambiguity(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    sources = (source('a', 'Pair', 'is_alias_for', 'Alpha', True),
        source('b', 'Pair', 'is_alias_for', 'Beta', True), source('c'), source('d', 'Beta'))
    index = index_for(sources, helper, indexes)
    assert parse_plan(task('owner of Pair'), index).requests[0].status == 'ambiguous_entity'
    result = run_arm(task('owner of all alias meanings of Pair excluding Beta'), 'structured_plan', index, helper)
    assert {e.source.source_id for e in result.selection.evidence} == {'a', 'c'}
    q = task('Do not pick a single referent; list both interpretations of Pair')
    assert len(run_arm(q, 'structured_plan', index, helper).selection.evidence) == 2

def test_prohibition_qualifier_and_no_approval_substitution(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    index = index_for((source('a', predicate='release_rule', value='approval after 17:00 UTC'),
        source('b', predicate='release_prohibition', value='after 18:00 UTC')), helper, indexes)
    for prompt, count in [('release prohibition of Alpha after 17:00 UTC', 0),
                          ('release prohibition of Alpha after 18:00 UTC', 1),
                          ('release prohibition of Alpha before noon', 0)]:
        assert len(run_arm(task(prompt), 'structured_plan', index, helper).selection.evidence) == count

def test_dedup_before_cap_conflicts_and_support_budget(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    sources = tuple(source(str(i)) for i in range(6)) + (source('x', predicate='located_at', value='Rome'),
        source('y', value='Ben'))
    index = index_for(sources, helper, indexes)
    result = run_arm(task('owner and location of Alpha'), 'structured_plan', index, helper)
    assert len(result.selection.evidence) == 3
    assert sorted(len(v) for v in result.provenance.values()) == [1, 1, 6]
    packed, _ = packet(task(), result.selection, index, 128, helper, load_encoding())
    assert packed.tokens <= 128

def test_eligibility_allowed_members_and_caps(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    foreign = replace(source('foreign'), scope='other')
    expired = replace(source('expired'), expires_at='2026-08-01T00:00:00Z')
    sources = (foreign, expired) + tuple(source(str(i), 'Alpha', 'depends_on', f'Target{i}', True) for i in range(140))
    index = index_for(sources, helper, indexes)
    plan = parse_plan(task('dependencies of Alpha'), index)
    execution = execute_plan(plan, index)
    assert execution.per_request == ('bounded',)
    assert execution.counters['examined_assertions'] <= 128
    assert len({m for p in execution.paths for m in p.members}) <= 32
    assert not execute_plan(plan, index, frozenset()).paths
    selected, _ = select_plan(plan, execution, CandidateSet(tuple(index.evidence.values())), index)
    assert len(selected.evidence) == 4
    assert all(e.source.scope == 'test' and e.source.source_id != 'expired' for e in selected.evidence)

def test_unsupported_positive_and_contract_violation(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    index = index_for((source('a'),), helper, indexes)
    plan = parse_plan(task('Explain the duties associated with Alpha'), index)
    assert plan.requests[0].status == 'unsupported' and plan.diagnostics
    with pytest.raises(IntegrityError):
        parse_plan(task(''), index)
    with pytest.raises(IntegrityError):
        Span(0, 1, 'wrong')
    with pytest.raises(IntegrityError):
        Request(Span(0, 1, 'x'), (), (Step('owned_by', 'out'),)*3)

def test_measured_case_and_manifest_fail_closed(helper: RustHelper, indexes: list[RelevanceIndex],
                                               tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    import plan_evaluate
    sources = (source('a'),)
    index = index_for(sources, helper, indexes)
    query = Query('unit', 'heldout', 'initial', 'unit', 'owner of Alpha', 'test', CLOCK, CLOCK, None)
    identity = next(iter(index.evidence))
    gold = Gold('unit', ((identity,),), (identity,), (), False)
    case = measured_case(query, 'structured_plan', index, sources, gold, 128, helper, load_encoding())
    assert len(cast(list[int], case['latency_samples_ns'])) == 3
    assert summary([case])['positive_coverage'] == 1.0
    assert 'requested_demands' not in cast(dict[str, JSON], case['metrics'])
    monkeypatch.setattr(plan_evaluate, 'ROOT', tmp_path)
    (tmp_path/'manifest.sha256').write_text('')
    with pytest.raises(IntegrityError, match='missing frozen'):
        verify_manifest(HELPER)

def test_multiple_request_limits_and_dropped_support(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    sources = (source('a'), source('b', 'Beta'), source('c', 'Gamma'), source('d', 'Delta'))
    index = index_for(sources, helper, indexes)
    plan = parse_plan(task('owner of Alpha; owner of Beta; owner of Gamma; owner of Delta; owner of Alpha'), index)
    execution = execute_plan(plan, index)
    assert len(plan.requests) == 5 and execution.per_request[-2:] == ('bounded', 'bounded')
    subsequent = parse_plan(task('owner of Alpha; owner of Unknown Asset; its location'), index)
    assert subsequent.requests[1].status == 'unresolved_entity'
    assert subsequent.requests[2].status == 'unsupported' and not subsequent.requests[2].roots

def test_shared_clause_exclusions_apply_to_every_output(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    index = index_for((source('a'), source('b', predicate='located_at', value='Rome')), helper, indexes)
    for arm in ('defect_fixes', 'structured_plan'):
        for suffix in ('omit owner', 'owner would not answer this'):
            result = run_arm(task('owner and location of Alpha; '+suffix), cast('Arm', arm), index, helper)
            assert [e.fact.predicate for e in result.selection.evidence] == ['located_at']
            packed, _ = packet(task(), result.selection, index, 256, helper, load_encoding())
            assert [e.fact.predicate for e in packed.evidence] == ['located_at']

def test_public_task_boundaries_reject_foreign_context(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    index = index_for((source('a'),), helper, indexes)
    plan = parse_plan(task(), index)
    raw = build_index((source('raw'),), task(), DerivedStore({'test|raw': source('raw')}), helper)
    indexes.append(raw)
    with pytest.raises(IntegrityError, match='context'):
        parse_plan(task(), raw)
    for wrong in (replace(task(), scope='foreign'), replace(task(), as_of='2026-08-01T00:00:00Z'),
                  replace(task(), knowledge_cutoff='2026-08-01T00:00:00Z')):
        with pytest.raises(IntegrityError, match='context'):
            parse_plan(wrong, index)
        with pytest.raises(IntegrityError, match='context'):
            retrieve_shared(wrong, plan, index, helper)
        for arm in ('old_combined', 'defect_fixes', 'structured_plan'):
            with pytest.raises(IntegrityError, match='context'):
                run_arm(wrong, cast('Arm', arm), index, helper)

def test_candidate_loss_preserves_partial_reason(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    sources = (source('a', 'Alpha', 'depends_on', 'First', True),
        source('b', 'Alpha', 'depends_on', 'Second', True),
        source('c', 'First', 'located_at', 'Rome'), source('d', 'Second', 'located_at', 'Paris'))
    index = index_for(sources, helper, indexes)
    plan = parse_plan(task('location of dependencies of Alpha'), index)
    allowed = frozenset(e.id for e in index.evidence.values() if e.source.source_id in ('a', 'c'))
    result = execute_plan(plan, index, allowed)
    assert result.per_request == ('partial',)
    assert any(reason == 'candidate_path_loss' for _, reason in result.reasons)

@pytest.mark.parametrize('prompt', ['owner of @0', 'owner of Alpha excluding @99'])
def test_literal_internal_markers_are_unsupported(prompt: str, helper: RustHelper,
                                                  indexes: list[RelevanceIndex]) -> None:
    index = index_for((source('a'),), helper, indexes)
    plan = parse_plan(task(prompt), index)
    assert plan.requests[0].status == 'unsupported'
    assert not execute_plan(plan, index).paths
