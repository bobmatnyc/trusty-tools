"""Independent contract fixtures; no official queries/gold or ranking evaluation reads."""
from __future__ import annotations
from dataclasses import replace, asdict
import json
import os
from pathlib import Path
from typing import Iterator, cast
import pytest
from legacy import Source, Fact, Evidence, Event, Gold, JSON, Query, RustHelper, IntegrityError, FixtureError, load_encoding
from contracts import Task, SelectorPolicy, CandidateSet, Selection, Support, consolidate, semantic_key
from maintenance import DerivedStore, derive_source, maintain
from relevance_index import RelevanceIndex, build_index
from query_policy import parse_intents
from relation_support import relation_lookup
from relevance import retrieve, intervention, select_claims, packet
from input_data import source_record
from metrics import evaluate

CLOCK = '2026-09-01T00:00:00Z'
HELPER = Path(os.environ.get('MEMORY_PROMPT_HELPER', '/Users/masa/trusty-search-experiment/target/debug/examples/memory_prompt_probe'))


def source(key: str, subject: str = 'Alpha', predicate: str = 'owned_by', value: str = 'Ada', entity: bool = False,
           claim: str | None = None, standing: bool = False) -> Source:
    text = claim or f'{subject} {predicate} {value}.'
    f = Fact('f', subject, predicate, value, value if entity else None, text, 0, len(text.encode()),
        '2026-01-01T00:00:00Z', None, None, standing)
    return Source(key, 'test', 1, 'Note', text, '2026-01-01T00:00:00Z', None, None, False, (f,))


def task(prompt: str = 'Who is the owner of Alpha?') -> Task:
    return Task(prompt, 'test', CLOCK, CLOCK)

@pytest.fixture
def helper() -> Iterator[RustHelper]:
    handle = RustHelper(HELPER)
    yield handle
    handle.close()

@pytest.fixture
def indexes() -> Iterator[list[RelevanceIndex]]:
    all_indexes: list[RelevanceIndex] = []
    yield all_indexes
    for index in all_indexes:
        index.close()


def indexed(sources: tuple[Source, ...], helper: RustHelper, indexes: list[RelevanceIndex]) -> RelevanceIndex:
    store = DerivedStore({s.key: s for s in sources})
    maintain(store, sources, (), max(1, len(sources)))
    index = build_index(sources, task(), store, helper)
    indexes.append(index)
    return index


def test_canonical_timestamps_and_evidence_spans() -> None:
    s = source('a', claim='Alpha café is owned by Ada.')
    raw = cast(JSON, json.loads(json.dumps(asdict(s))))
    assert source_record(raw) == s
    bad = cast(dict[str, JSON], raw)
    bad['observed_at'] = '2026-01-01T00:00:00.1Z'
    with pytest.raises(FixtureError, match='canonical'):
        source_record(bad)


def test_longest_ambiguous_alias_blocks_shorter_prefix(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    sources = (source('a', 'Alpha', value='Ada'), source('b', 'Beta', value='Ben'),
        source('short', 'Harbor', 'is_alias_for', 'Alpha', True),
        source('long-a', 'Harbor Twin', 'is_alias_for', 'Alpha', True),
        source('long-b', 'Harbor Twin', 'is_alias_for', 'Beta', True))
    index = indexed(sources, helper, indexes)
    ambiguous = parse_intents(task('Who is the owner of Harbor Twin?'), index)
    assert ambiguous[0].status == 'ambiguous_entity' and not ambiguous[0].entities
    alternatives = parse_intents(task('List alternatives for Harbor Twin.'), index)
    result = relation_lookup(task(), alternatives, index)
    assert {e.source.source_id for e in result.evidence} == {'long-a', 'long-b'}


def test_multiple_demands_clauses_negation_and_rationale(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    index = indexed((source('a'), source('b', 'Beta')), helper, indexes)
    parsed = parse_intents(task('Who is the owner of Alpha and where is it; what is its access code?'), index)
    assert [d.intent for d in parsed] == ['owner', 'location', 'access']
    assert all(d.entities == ('Alpha',) for d in parsed)
    negated = parse_intents(task('Do not deploy Alpha; why is Beta configured this way?'), index)
    assert [d.status for d in negated] == ['negated_demand', 'ready']
    assert negated[1].intent == 'reason'
    hypothetical = parse_intents(task('Suppose Alpha deploys; who is the owner of Beta?'), index)
    assert hypothetical[0].status == 'hypothetical' and hypothetical[1].status == 'ready'


def test_incoming_two_hops_and_complete_support(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    sources = (source('owner', 'Alpha', 'maintained_by', 'Team', True),
        source('pager', 'Team', 'escalates_to', 'Ada', True),
        source('hours', 'Ada', 'contact_window', 'noon'),
        source('incoming', 'Beta', 'depends_on', 'Alpha', True),
        source('cycle', 'Ada', 'uses', 'Alpha', True))
    index = indexed(sources, helper, indexes)
    contact = task('What are contact hours for Alpha?')
    result = relation_lookup(contact, parse_intents(contact, index), index)
    assert {e.source.source_id for e in result.evidence} == {'owner', 'pager', 'hours'}
    assert any(len(s.members) == 3 for s in result.supports)
    reverse = task('What is dependent on Alpha?')
    assert {e.source.source_id for e in relation_lookup(reverse, parse_intents(reverse, index), index).evidence} == {'incoming', 'cycle'}


def test_relation_hub_caps_and_selector_cannot_fetch_missing_path(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    sources = tuple(source(f's{i}', 'Alpha', 'depends_on', f'Target{i}', True) for i in range(150))
    index = indexed(sources, helper, indexes)
    q = task('What dependencies does Alpha have?')
    result = relation_lookup(q, parse_intents(q, index), index)
    assert result.counters['examined_assertions'] <= 128 and len(result.evidence) <= 32
    assert result.counters.get('scan_limit') or result.counters.get('emitted_limit')
    allowed = CandidateSet(())
    assert not select_claims(q, parse_intents(q, index), allowed, SelectorPolicy(1, 4), index).evidence


def test_unknown_overlap_and_known_unsupported_abstention(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    s = source('a', predicate='is_fact', value='green preference', claim='Alpha likes green ceramic cups.')
    index = indexed((s,), helper, indexes)
    q = task('Alpha green ceramic preferences')
    candidates = CandidateSet(tuple(index.evidence.values()))
    assert select_claims(q, parse_intents(q, index), candidates, SelectorPolicy(2, 4), index).evidence
    unsupported = task('Why does Alpha like green ceramic cups?')
    assert not select_claims(unsupported, parse_intents(unsupported, index), candidates, SelectorPolicy(1, 4), index).evidence


def test_support_budget_never_emits_detached_endpoint(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    long = source('long', 'Alpha', 'maintained_by', 'Team', True, claim='Alpha is maintained by Team. ' * 200)
    endpoint = source('short', 'Team', 'contact_window', 'noon')
    index = indexed((long, endpoint), helper, indexes)
    es = {e.source.source_id: e for e in index.evidence.values()}
    support = Support(es['short'].id, (es['long'].id, es['short'].id), 0)
    selected = Selection((es['short'], es['long']), (support,))
    result, reasons = packet(task(), selected, index, 128, helper, load_encoding())
    assert es['short'].id not in {e.id for e in result.evidence}
    assert (es['short'].id, 'incomplete_path_budget') in reasons


def test_duplicate_precision_provenance_and_conflicts(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    a, b, conflict = source('a'), source('b'), source('c', value='Grace')
    index = indexed((a, b, conflict), helper, indexes)
    es = tuple(index.evidence.values())
    collapsed = consolidate(es)
    assert len(collapsed.representatives) == 2
    assert semantic_key(es[0]) == semantic_key(es[1]) != semantic_key(es[2])
    assert len(collapsed.members[es[0].id]) == 2
    selected = Selection(es[:2])
    result, _ = packet(task(), selected, index, 256, helper, load_encoding())
    gold = Gold('g', ((es[0].id, es[1].id),), (es[0].id, es[1].id), (), False)
    metrics = evaluate(result, task(), (a, b, conflict), gold, es, selected.evidence, (), 256, helper, load_encoding(), {e.id:(e.id,) for e in es}, parse_intents(task(), index))
    assert metrics['precision'] == .5 and metrics['redundant_assertions'] == 1 and metrics['coverage'] == 1


def test_legacy_mixed_full_derived_fallback(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    sources = (source('a'), source('b', value='Ben'))
    store = DerivedStore({s.key: s for s in sources})
    for expected_missing in (2, 1, 0):
        index = build_index(sources, task(), store, helper)
        indexes.append(index)
        result = retrieve(task(), 'claim_index', index, helper, SelectorPolicy(1, 4))
        assert result.counters['missing_derived_sources'] == expected_missing
        assert {e.source.source_id for e in result.evidence} == {'a', 'b'}
        if expected_missing:
            maintain(store, sources, (), 1)
    assert all(store.sources[s.key] == s for s in sources)


def test_atomic_failure_replay_delete_checkpoint_and_clean_rebuild() -> None:
    a, b = source('a'), source('b')
    store = DerivedStore({s.key: s for s in (a, b)})
    maintain(store, (a, b), (), 2)
    events = (Event(1, replace(a, revision=2, title='changed')), Event(2, replace(b, revision=2, deleted=True, body='', facts=())))
    before = store.checkpoint()
    def failing(source: Source):
        raise IntegrityError('injected publication failure')
    with pytest.raises(IntegrityError, match='injected'):
        maintain(store, (a, b), events, 2, derive=failing)
    assert store.checkpoint() == before
    maintain(store, (a, b), events, 1)
    resumed = DerivedStore.restore(store.checkpoint())
    maintain(resumed, (a, b), events, 1)
    snapshot = resumed.checkpoint()
    assert maintain(resumed, (a, b), events, 1)['changed'] == 0
    assert resumed.checkpoint() == snapshot and resumed.tombstones[b.key] == 2
    clean = DerivedStore(dict(resumed.sources))
    maintain(clean, tuple(clean.sources.values()), (), 2)
    assert clean.derived_hash() == resumed.derived_hash()
    with pytest.raises(IntegrityError, match='conflicting replay'):
        maintain(resumed, (a, b), (Event(1, replace(a, revision=3)),), 1)


def test_independent_arms_metadata_isolation_and_packet_corruption(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    sources = (source('a'), source('b', predicate='located_at', value='Floor 2'))
    index = indexed(sources, helper, indexes)
    policies = SelectorPolicy(1, 4)
    q1 = Query('first','tune','initial','owner','Who is the owner of Alpha?','test',CLOCK,CLOCK,None)
    q2 = replace(q1,id='second',split='heldout',category='unrelated')
    tasks = [Task(q.prompt,q.scope,q.as_of,q.knowledge_cutoff) for q in (q1,q2)]
    first = retrieve(tasks[0], 'baseline', index, helper, policies)
    for arm in ('baseline', 'selector', 'cleanup'):
        assert [e.id for e in retrieve(tasks[1], arm, index, helper, policies).evidence] == [e.id for e in first.evidence]
    chosen, provenance = intervention(task(), 'selector', first, index, policies)
    result, _ = packet(task(), chosen, index, 128, helper, load_encoding())
    gold = Gold('g', ((next(e.id for e in first.evidence if e.fact.predicate=='owned_by'),),), tuple(e.id for e in chosen.evidence), (), False)
    result.text += 'An unregistered assertion.'
    with pytest.raises(IntegrityError):
        evaluate(result, task(), sources, gold, first.evidence, chosen.evidence, chosen.supports, 128, helper, load_encoding(), provenance, parse_intents(task(), index))


def test_partial_current_and_old_policy_records_do_not_hide_sources(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    s = source('a')
    complete = derive_source(s)
    for record in (replace(complete, claims=()), replace(complete, policy='old-v0')):
        store = DerivedStore({s.key: s}, {s.key: record})
        assert not store.valid(s.key)
        index = build_index((s,), task(), store, helper)
        indexes.append(index)
        found = retrieve(task(), 'claim_index', index, helper, SelectorPolicy(1, 4))
        assert found.counters['missing_derived_sources'] == 1 and found.evidence
        assert maintain(store, (s,), (), 1)['changed'] == 1
        assert store.valid(s.key) and store.sources[s.key] == s


def test_unknown_alias_requires_candidate_support_and_full_budget(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    alias = source('alias', 'Nickname', 'is_alias_for', 'Alpha', True,
        claim='Nickname is an alias for Alpha. ' * 180)
    fact = source('fact', 'Alpha', 'is_fact', 'green ceramic cups', claim='Alpha likes green ceramic cups.')
    index = indexed((alias, fact), helper, indexes)
    q = task('Nickname green ceramic preferences')
    demands = parse_intents(q, index)
    endpoint = next(e for e in index.evidence.values() if e.source.source_id == 'fact')
    missing = select_claims(q, demands, CandidateSet((endpoint,)), SelectorPolicy(2, 4), index)
    assert not missing.evidence and (endpoint.id, 'no_complete_path') in missing.rejections
    complete = select_claims(q, demands, CandidateSet(tuple(index.evidence.values())), SelectorPolicy(2, 4), index)
    assert len(complete.supports[0].members) == 2
    result, reasons = packet(q, complete, index, 128, helper, load_encoding())
    assert endpoint.id not in {e.id for e in result.evidence}
    assert (endpoint.id, 'incomplete_path_budget') in reasons


def test_seed_and_visited_caps_are_independent(helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    sources = tuple(source(f'edge{i}', 'Alpha', 'maintained_by', f'Team{i}', True) for i in range(31)) + (
        source('beta', 'Beta'), source('gamma', 'Gamma'))
    index = indexed(sources, helper, indexes)
    q = task('Contact hours for Alpha; owner of Beta; owner of Gamma')
    result = relation_lookup(q, parse_intents(q, index), index)
    assert result.counters['visited_nodes'] == 32 and result.counters['visited_limit'] == 1
    assert result.counters['seeds'] <= 3
    from contracts import Demand
    manual = (Demand('contact', ('Alpha', 'Team0', 'Team1', 'Team2'), 'ready'),)
    result = relation_lookup(task(), manual, index)
    assert result.counters['seeds'] == 3 and result.counters['seed_limit'] == 1


@pytest.mark.parametrize('prompt,bindings,complete_demands', [
    ('Who is the owner of Alpha and where is Alpha?', 2, 1),
    ('Who is the owner of Alpha and Beta?', 2, 0),
])
def test_partial_requested_binding_denominator(prompt: str, bindings: int, complete_demands: int,
        helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    owner = source('owner')
    beta = source('beta-name', 'Beta', 'is_fact', 'exists')
    sources = (owner, beta)
    index = indexed(sources, helper, indexes)
    q = task(prompt)
    demands = parse_intents(q, index)
    candidates = CandidateSet(tuple(index.evidence.values()))
    selected = select_claims(q, demands, candidates, SelectorPolicy(1, 4), index)
    result, _ = packet(q, selected, index, 256, helper, load_encoding())
    identity = next(e.id for e in index.evidence.values() if e.source.source_id == 'owner')
    gold = Gold('g', ((identity,),), (identity,), (), False)
    metrics = evaluate(result, q, sources, gold, candidates.evidence, selected.evidence,
        selected.supports, 256, helper, load_encoding(), {e.id:(e.id,) for e in selected.evidence}, demands)
    assert metrics['requested_bindings'] == bindings and metrics['completed_bindings'] == 1
    assert metrics['requested_binding_completion_rate'] == .5
    assert metrics['completed_demands'] == complete_demands
    assert metrics['selected_support_path_retention'] == 1


@pytest.mark.parametrize('include_distinct_target', [True, False])
def test_alias_completion_accepts_duplicate_representatives_only(include_distinct_target: bool,
        helper: RustHelper, indexes: list[RelevanceIndex]) -> None:
    sources = (source('alias-a', 'Nickname', 'is_alias_for', 'Alpha', True),
        source('alias-b', 'Nickname', 'is_alias_for', 'Alpha', True),
        source('alias-c', 'Nickname', 'is_alias_for', 'Beta', True))
    index = indexed(sources, helper, indexes)
    q = task('List alternatives for Nickname.')
    evidence = tuple(e for e in index.evidence.values() if include_distinct_target or e.fact.object_entity == 'Alpha')
    candidates = CandidateSet(evidence)
    selected, provenance = intervention(q, 'combined', candidates, index, SelectorPolicy(1, 4))
    result, _ = packet(q, selected, index, 256, helper, load_encoding())
    alpha = tuple(e.id for e in index.evidence.values() if e.fact.object_entity == 'Alpha')
    beta = tuple(e.id for e in index.evidence.values() if e.fact.object_entity == 'Beta')
    gold = Gold('alias', (alpha, beta), tuple(index.evidence), (), False)
    metrics = evaluate(result, q, sources, gold, candidates.evidence, selected.evidence,
        selected.supports, 256, helper, load_encoding(), provenance, parse_intents(q, index))
    assert len(selected.evidence) == (2 if include_distinct_target else 1)
    assert metrics['completed_bindings'] == int(include_distinct_target)
    assert metrics['requested_binding_completion_rate'] == float(include_distinct_target)
    assert metrics['coverage'] == (1 if include_distinct_target else .5)
