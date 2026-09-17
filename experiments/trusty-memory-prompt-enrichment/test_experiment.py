"""Contract tests use explicit source fixtures and the real local helper/model."""
from __future__ import annotations
from dataclasses import asdict, replace
import os
import json
from pathlib import Path
from typing import Iterator, cast
import numpy as np
import pytest
from adapters import LocalEncoder, RustHelper
from records import IntegrityError, Fact, Source, Evidence, Query, Event, Policy, Gold, JSON, FixtureError, ModelArtifactError, parse_source, eligible_facts
from projection import backfill_vectors, SourceIndex, build_source_index, build_projection, graph_lookup, update_projection
from retrieval import Retrieval, pack
from scoring import evaluate
from offline_encoding import load_encoding

HELPER = Path(os.environ.get('MEMORY_PROMPT_HELPER', '/Users/masa/trusty-search-experiment/target/debug/examples/memory_prompt_probe'))
MODEL = Path(os.environ.get('MEMORY_PROMPT_MODEL', '/Users/masa/.cache/fastembed/models--Qdrant--all-MiniLM-L6-v2-onnx/snapshots/5f1b8cd78bc4fb444dd171e59b18f3a3af89a079'))
CLOCK = '2026-09-01T00:00:00Z'


def source(name: str, subject: str = 'Alpha', target: str | None = None,
           predicate: str = 'is_fact', claim: str = 'Alpha is useful.') -> Source:
    fact = Fact('f', subject, predicate, target or claim, target, claim, 0, len(claim.encode()),
        '2026-01-01T00:00:00Z', None, None, False)
    return Source(name, 'test', 1, name, claim, '2026-01-01T00:00:00Z', None, None, False, (fact,))


def query(text: str = 'Alpha', hint: str | None = 'Alpha') -> Query:
    return Query('q', 'tune', 'initial', 'relation', text, 'test', CLOCK, CLOCK, hint)

@pytest.fixture(scope='module')
def encoder() -> LocalEncoder:
    return LocalEncoder(MODEL)

@pytest.fixture
def helper() -> Iterator[RustHelper]:
    instance = RustHelper(HELPER)
    yield instance
    instance.close()


def test_shape_span_and_revision_validation() -> None:
    s = source('a', claim='Alpha uses café.')
    assert parse_source(cast(JSON, json.loads(json.dumps(asdict(s))))) == s
    bad = json.loads(json.dumps(asdict(s)))
    bad['facts'][0]['end_byte'] -= 1
    with pytest.raises(FixtureError, match='span'):
        parse_source(cast(JSON, bad))
    bad = json.loads(json.dumps(asdict(s)))
    bad['unknown'] = 1
    with pytest.raises(FixtureError, match='keys'):
        parse_source(cast(JSON, bad))


def test_temporal_scope_supersession_and_exact_ties() -> None:
    base = source('old')
    old = replace(base, facts=(replace(base.facts[0], single_value_slot='owner'),))
    newer = replace(source('new'), facts=(replace(base.facts[0], valid_from='2026-08-01T00:00:00Z', single_value_slot='owner'),))
    conflict = replace(newer, source_id='conflict')
    future = replace(source('future'), facts=(replace(base.facts[0], valid_from='2027-01-01T00:00:00Z'),))
    expired = replace(source('expired'), expires_at='2026-08-01T00:00:00Z')
    other = replace(source('other'), scope='private')
    found = eligible_facts((old, newer, conflict, future, expired, other), query())
    assert {e.source.source_id for e in found} == {'new', 'conflict'}
    historical = replace(query(), as_of='2026-06-01T00:00:00Z')
    assert {e.source.source_id for e in eligible_facts((old, newer), historical)} == {'old'}


def test_resident_protocol_native_direction_and_unknown_keys(helper: RustHelper, tmp_path: Path) -> None:
    helper.request({'op': 'load', 'projection': 'small', 'scratch_dir': str(tmp_path),
        'documents': [{'id': 'a', 'text': 'Alpha uses Beta'}],
        'triples': [{'id': 'edge', 'subject': 'Alpha', 'predicate': 'uses', 'object': 'Beta', 'valid_from': CLOCK, 'valid_to': None}]})
    pid = helper.process.pid
    for _ in range(3):
        assert helper.request({'op': 'search', 'projection': 'small', 'text': 'Alpha', 'limit': 20})['hits']
    assert helper.process.pid == pid
    response = helper.request({'op': 'graph', 'projection': 'small', 'method': 'expand_neighbors', 'entity': 'Beta', 'hops': 1})
    assert cast(list[dict[str, JSON]], response['triples'])[0]['subject'] == 'Alpha'
    from records import ProtocolError
    with pytest.raises(ProtocolError):
        helper.request({'op': 'format', 'triples': [], 'unknown': 1})


def test_graph_inverse_two_hops_cycles_and_ambiguous_alias(encoder: LocalEncoder, helper: RustHelper, tmp_path: Path) -> None:
    sources = (source('ab', 'Alpha', 'Beta', 'uses'), source('bc', 'Beta', 'Contact', 'calls'),
        source('ca', 'Contact', 'Alpha', 'cycles'), source('literal', 'Contact', claim='Contact accepts calls at noon.'),
        source('alias-a', 'Shortcut', 'Alpha', 'is_alias_for'), source('alias-b', 'Shortcut', 'Beta', 'is_alias_for'))
    index = build_projection(build_source_index(sources, encoder), query(), helper, str(tmp_path))
    two, _ = graph_lookup(query(), index, Policy(max_hops=2))
    assert any(e.source.source_id == 'literal' for e in two)
    assert len({e.id for e in two}) == len(two)
    inverse, _ = graph_lookup(query('Beta', 'Beta'), index, Policy())
    assert any(e.source.source_id == 'ab' for e in inverse)
    ambiguous, details = graph_lookup(query('Shortcut', 'Shortcut'), index, Policy())
    assert ambiguous == [] and details['ambiguous_aliases'] == 1
    assert graph_lookup(query('Unknown', None), index, Policy())[0] == []


def test_bounded_hub_and_insertion_determinism(encoder: LocalEncoder, helper: RustHelper, tmp_path: Path) -> None:
    sources = tuple(source(f'edge-{i:03}', 'Alpha', f'Node{i}', 'uses') for i in range(150))
    state = build_source_index(sources, encoder)
    first = build_projection(state, query(), helper, str(tmp_path))
    reverse_query = replace(query(), scenario='updated')
    reverse = build_projection(SourceIndex(dict(reversed(list(state.sources.items()))), dict(state.vectors)), reverse_query, helper, str(tmp_path))
    for policy in (Policy(), Policy(max_scanned_edges=10, max_emitted_edges=32)):
        found, details = graph_lookup(query(), first, policy)
        backwards, _ = graph_lookup(query(), reverse, policy)
        assert [e.id for e in found] == [e.id for e in backwards]
        assert details['scanned_edges'] <= policy.max_scanned_edges
        assert len(found) <= policy.max_emitted_edges and details['truncated']


def test_complete_claim_budget_scope_and_gold_isolation(encoder: LocalEncoder, helper: RustHelper, tmp_path: Path) -> None:
    small = source('small')
    huge = source('huge', claim='Very long complete assertion ' * 300)
    index = build_projection(build_source_index((small, huge), encoder), query(), helper, str(tmp_path))
    encoding = load_encoding()
    evidence = [Evidence(huge, huge.facts[0]), Evidence(small, small.facts[0])]
    packet = pack(query(), Retrieval(evidence, {}, {}), index, 128, helper, encoding, 'bm25')
    assert packet.tokens <= 128 and [e.source.source_id for e in packet.evidence] == ['small']
    gold = Gold('q', ((evidence[1].id,),), (evidence[1].id,), (), False)
    metrics = evaluate(packet, gold, (small, huge), query(), treatment='bm25', budget=128, helper=helper, encoding=encoding)
    assert metrics['coverage'] == 1 and metrics['fact_f1'] == 1
    negative = evaluate(packet, replace(gold, required=(), acceptable=(), expected_empty=True), (small, huge), query(), treatment='bm25', budget=128, helper=helper, encoding=encoding)
    assert negative['coverage'] is None and negative['fact_f1'] is None and not negative['empty_task']


def test_local_embedding_real_normalized_and_missing_artifacts(encoder: LocalEncoder, tmp_path: Path) -> None:
    first = encoder.encode(('A short preference.', 'Use concise explanations.'))
    assert first.shape == (2, 384)
    assert np.isfinite(first).all() and np.allclose(np.linalg.norm(first, axis=1), 1, atol=1e-6)
    assert np.allclose(first, encoder.encode(('A short preference.', 'Use concise explanations.')), atol=1e-6)
    with pytest.raises(ModelArtifactError):
        LocalEncoder(tmp_path)


def test_incremental_embeddings_delete_noop_and_clean_rebuild(encoder: LocalEncoder) -> None:
    a, b = source('a'), source('b')
    state = build_source_index((a, b), encoder)
    before = state.vectors['test|b']
    changed = replace(a, revision=2, title='Updated')
    events = (Event(1, changed), Event(2, replace(b, revision=2, deleted=True, facts=(), body='')))
    update_projection(state, events, 1, encoder)
    assert state.vectors['test|b'] is before
    update_projection(state, events, 1, encoder)
    expected = build_source_index((changed,), encoder)
    assert np.allclose(state.vectors['test|a'], expected.vectors['test|a'], atol=1e-6)
    fingerprint = state.semantic_hash()
    vector = state.vectors['test|a']
    result = update_projection(state, events, 1, encoder)
    assert result['changed'] == result['removed'] == 0
    assert state.semantic_hash() == fingerprint and state.vectors['test|a'] is vector
    assert state.sources['test|a'].observed_at == a.observed_at


def test_mixed_vector_coverage_backfill_keeps_lexical_sources(encoder: LocalEncoder, helper: RustHelper, tmp_path: Path) -> None:
    from retrieval import retrieve
    a, b = source('a'), source('b')
    state = SourceIndex({a.key: a, b.key: b}, {})
    missing_hash = state.semantic_hash()
    index = build_projection(state, query(), helper, str(tmp_path))
    assert index.document_ids == () and index.vectors.shape == (0, 384)
    lexical = retrieve(query(), 'bm25', index, encoder, helper, Policy())
    assert {e.source.key for e in lexical.evidence} == {a.key, b.key}
    assert retrieve(query(), 'bm25_dense', index, encoder, helper, Policy()).evidence
    first = backfill_vectors(state, encoder, 1)
    assert first['changed'] == 1 and first['pending'] == 1
    assert state.semantic_hash() != missing_hash
    mixed = build_projection(state, replace(query(), scenario='updated'), helper, str(tmp_path))
    assert mixed.document_ids == (a.key,)
    assert {e.source.key for e in retrieve(query(), 'bm25', mixed, encoder, helper, Policy()).evidence} == {a.key, b.key}
    backfill_vectors(state, encoder, 1)
    assert set(state.vectors) == {a.key, b.key}
    assert state.sources == {a.key: a, b.key: b}
    assert backfill_vectors(state, encoder, 1)['changed'] == 0


def test_failed_encoding_preserves_batch_and_allows_retry(encoder: LocalEncoder, monkeypatch: pytest.MonkeyPatch) -> None:
    a, b = source('a'), source('b')
    state = build_source_index((a, b), encoder)
    original_hash = state.semantic_hash()
    events = (Event(1, replace(a, revision=2, title='changed')), Event(2, replace(b, revision=2, title='also changed')))
    real_encode = encoder.encode
    calls = 0
    def fail_second(texts: tuple[str, ...], *, corpus: bool = False) -> np.ndarray:
        nonlocal calls
        calls += 1
        if calls == 2:
            raise ModelArtifactError('injected encoding failure')
        return real_encode(texts, corpus=corpus)
    with monkeypatch.context() as patch:
        patch.setattr(encoder, 'encode', fail_second)
        with pytest.raises(ModelArtifactError):
            update_projection(state, events, 2, encoder)
    assert state.semantic_hash() == original_hash and state.sequence == 0
    assert update_projection(state, events, 2, encoder)['changed'] == 2
    assert state.sequence == 2 and state.sources[a.key].revision == 2


@pytest.mark.parametrize('corruption', ['extra_text', 'replaced_claim', 'wrong_tokens', 'source_sidecar'])
def test_packet_integrity_rejects_unregistered_or_missing_claims(corruption: str, encoder: LocalEncoder, helper: RustHelper, tmp_path: Path) -> None:
    s = source('a')
    index = build_projection(build_source_index((s,), encoder), query(), helper, str(tmp_path))
    encoding = load_encoding()
    e = Evidence(s, s.facts[0])
    treatment = 'bm25_source_packet' if corruption == 'source_sidecar' else 'bm25'
    packet = pack(query(), Retrieval([e], {}, {}), index, 128, helper, encoding, treatment)
    gold = Gold('q', ((e.id,),), (e.id,), (), False)
    if corruption == 'extra_text':
        packet.text += 'Alpha owns the moon.\n'
        packet.tokens = len(encoding.encode(packet.text))
    elif corruption == 'replaced_claim':
        packet.text = 'No factual content.'
        packet.renderings[e.id] = packet.text
        packet.tokens = len(encoding.encode(packet.text))
    elif corruption == 'source_sidecar':
        packet.evidence.clear()
    else:
        packet.tokens += 1
    with pytest.raises(IntegrityError):
        evaluate(packet, gold, (s,), query(), treatment=treatment, budget=128, helper=helper, encoding=encoding)


def test_standing_cache_negative_query_excludes_standing_tokens(encoder: LocalEncoder, helper: RustHelper, tmp_path: Path) -> None:
    base = source('standing')
    s = replace(base, facts=(replace(base.facts[0], standing=True),))
    index = build_projection(build_source_index((s,), encoder), query(), helper, str(tmp_path))
    encoding = load_encoding()
    packet = pack(query(), Retrieval(list(index.standing), {}, {}), index, 128, helper, encoding, 'standing_cache', prelude=False)
    gold = Gold('q', (), (index.standing[0].id,), (), True)
    metrics = evaluate(packet, gold, (s,), query(), treatment='standing_cache', budget=128, helper=helper, encoding=encoding)
    assert packet.tokens > 0 and metrics['unsupported_tokens'] == 0 and metrics['task_facts'] == 0
    assert metrics['standing_coverage'] == 1 and metrics['standing_expected_facts'] == 1
