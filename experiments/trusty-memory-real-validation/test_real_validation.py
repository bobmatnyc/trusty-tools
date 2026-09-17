"""Toy-only tests: never read the frozen real prompt sample or private memory."""
from dataclasses import asdict, replace
import json
from pathlib import Path
import pytest
import real_evaluate
from real_adapter import (adapt_snapshot, chunks, file_digest, prepare, private_dir,
    read_frozen_sample, read_json, read_snapshot, write_private)
from real_evaluate import evaluate, run_case, score_case, validate_gold
from legacy import IntegrityError, RustHelper, load_encoding
from contracts import Task
from maintenance import DerivedStore, derive_source
from plan_index import build_scoped_index

HELPER = Path('/Users/masa/trusty-search-experiment/target/debug/examples/memory_prompt_probe')

def snapshot():
    return {'version':'memory-real-snapshot-v1', 'captured_at_ms':100000,
        'counts':{'drawers':{'read':1, 'decoded':1, 'errors':0}, 'triples':{'read':1, 'decoded':1, 'errors':0}},
        'drawers':[{'key_hex':'01', 'record':{'content':'Alpha uses Beta. The access code is blue.',
            'created_at_ms':1, 'expires_at_ms':None, 'room_id':'toy', 'importance':0.5,
            'tags':[], 'source_file':None, 'completed_at_ms':None, 'fact_key':None}}],
        'triples':[{'key_hex':'02', 'row_kind':'active', 'subject':'Alpha', 'predicate':'uses',
            'object':'Beta', 'valid_from_ms':1, 'valid_to_ms':None, 'confidence':1.0, 'provenance':'auto:remember'}]}

def test_chunks_and_snapshot_integrity(tmp_path):
    encoding = load_encoding()
    text = '日本語🙂 hello ' * 140
    spans = chunks(text, encoding)
    assert ''.join(claim for _, _, claim in spans) == text
    assert all(text.encode()[start:end].decode() == claim and len(encoding.encode(claim)) <= 80
        for start, end, claim in spans)
    path = tmp_path / 'snapshot.json'
    write_private(path, snapshot())
    data = read_snapshot(path, 2)
    data['drawers'][0]['decode_version'] = 'pre_task'
    data['drawers'][0]['absent_fields'] = ['completed_at_ms', 'fact_key']
    corpus = adapt_snapshot(data, 'toy', encoding)
    assert corpus.counts['drawer_chunks'] == corpus.counts['graph_facts'] == 1
    assert list(corpus.evidence_metadata.values())[0]['decode_version'] == 'pre_task'
    assert list(corpus.evidence_metadata.values())[0]['absent_fields'] == ['completed_at_ms', 'fact_key']
    graph = corpus.sources[-1]
    assert graph.facts[0].predicate == 'uses' and graph.facts[0].object_entity is None
    assert list(corpus.evidence_metadata.values())[-1]['record']['valid_from_ms'] == 1
    with pytest.raises(IntegrityError, match='limit'):
        read_snapshot(path, 1)
    data['counts']['drawers']['read'] = 3
    path.write_text(json.dumps(data))
    with pytest.raises(IntegrityError, match='count'):
        read_snapshot(path, 2)

def test_private_outputs_and_sample_digest(tmp_path):
    git = tmp_path / 'repo'
    git.mkdir()
    (git / '.git').write_text('gitdir: elsewhere')
    with pytest.raises(IntegrityError, match='Git'):
        private_dir(git / 'output')
    path = tmp_path / 'private.json'
    write_private(path, {'private':'toy'})
    assert path.stat().st_mode & 0o777 == 0o600
    with pytest.raises(FileExistsError):
        write_private(path, {})
    with pytest.raises(IntegrityError, match='digest'):
        read_frozen_sample(path, 'bad')

def test_prepare_and_helper(tmp_path, monkeypatch):
    snapshot_path = tmp_path / 'snapshot.json'
    sample_path = tmp_path / 'sample.json'
    write_private(snapshot_path, snapshot())
    write_private(sample_path, {'queries':[{'id':f'toy-{i}', 'palace':'toy', 'prompt':'Alpha uses Beta',
        'logged_at':'1970-01-01T00:00:01Z', 'log_file':'toy.jsonl', 'line':i+1} for i in range(32)],
        'log_hashes':{'toy.jsonl':'toyhash'}})
    encoding = load_encoding()
    def fail(*args, **kwargs):
        raise AssertionError('prepare constructed a retrieval helper')
    with monkeypatch.context() as patch:
        patch.setattr(RustHelper, '__init__', fail)
        prepare(snapshot_path, sample_path, file_digest(sample_path), tmp_path / 'prepared', 'toy', encoding, 10)
    prepared = read_json(tmp_path / 'prepared' / 'prepared.json')
    assert len(prepared['queries']) == 32
    gold_path = tmp_path / 'gold.json'
    write_private(gold_path, {'judgments':[{'query_id':f'toy-{i}', 'status':'unavailable',
        'supporting_spans':[], 'acceptable':[], 'forbidden':[], 'corroboration':[]} for i in range(32)]})
    prepared_path = tmp_path / 'prepared' / 'prepared.json'
    evaluate(prepared_path, gold_path, file_digest(prepared_path), file_digest(gold_path),
        HELPER, tmp_path / 'results', encoding)
    summary = read_json(tmp_path / 'results' / 'summary.json')
    assert len(summary) == 6
    assert all(row['negative_abstention'] is None and row['unavailable'] == 32 for row in summary.values())
    corpus = adapt_snapshot(snapshot(), 'toy', encoding)
    task = Task('Alpha uses Beta', 'toy', corpus.as_of, corpus.as_of)
    store = DerivedStore({s.key:s for s in corpus.sources})
    store.records = {s.key:derive_source(s) for s in corpus.sources}
    helper = RustHelper(HELPER)
    try:
        index = build_scoped_index(corpus.sources, task, store, helper)
        try:
            result = run_case(task, 'raw_bm25', index, helper, 128, encoding, corpus.sources)
            assert result['evidence_ids'] and result['tokens'] <= 128
            for arm in ('old_combined', 'structured_plan'):
                assert run_case(task, arm, index, helper, 128, encoding, corpus.sources)['tokens'] <= 128
            original_packet = real_evaluate.packet
            def forged(*args, **kwargs):
                packed, rejected = original_packet(*args, **kwargs)
                return replace(packed, text='forged'), rejected
            with monkeypatch.context() as patch:
                patch.setattr(real_evaluate, 'packet', forged)
                with pytest.raises(IntegrityError):
                    run_case(task, 'raw_bm25', index, helper, 128, encoding, corpus.sources)
        finally:
            index.close()
    finally:
        helper.close()

def test_gold_statuses_and_span_coverage():
    corpus = adapt_snapshot(snapshot(), 'toy', load_encoding())
    drawer = corpus.sources[0]
    drawer_id = next(iter(corpus.evidence_metadata))
    metadata = dict(corpus.evidence_metadata)
    prepared = json.loads(json.dumps({'queries':[{'id':'toy'}], 'sources':[asdict(s) for s in corpus.sources], 'metadata':metadata}))
    gold = {'query_id':'toy', 'status':'positive', 'required':[[drawer_id]], 'supporting_spans':[{'source_id':drawer.source_id,
        'start_byte':0, 'end_byte':15}], 'acceptable':[], 'forbidden':[], 'corroboration':[]}
    validated = validate_gold({'judgments':[gold]}, prepared)
    case = {'candidate_ids':[drawer_id], 'selected_ids':[drawer_id], 'evidence_ids':[drawer_id]}
    score = score_case(case, validated['toy'], metadata)
    assert score['evidence_span_coverage'] == score['evidence_note_recall'] == score['drawer_relevant_chunk_fraction'] == 1
    for status in ('negative', 'unavailable', 'ambiguous'):
        row = {**gold, 'status':status, 'required':[], 'supporting_spans':[]}
        assert validate_gold({'judgments':[row]}, prepared)['toy']['status'] == status
    graph_id = list(metadata)[-1]
    with pytest.raises(IntegrityError, match='corroboration'):
        validate_gold({'judgments':[{**gold, 'acceptable':[graph_id]}]}, prepared)

def test_current_eligibility_and_no_predicate_normalization():
    data = snapshot()
    data['triples'][0]['predicate'] = 'depends-on'
    data['drawers'][0]['record']['expires_at_ms'] = data['captured_at_ms']
    corpus = adapt_snapshot(data, 'toy', load_encoding())
    assert corpus.counts['expired_drawers'] == 1
    assert corpus.sources[0].facts[0].predicate == 'depends-on'
    data['triples'][0]['valid_to_ms'] = data['captured_at_ms']
    assert adapt_snapshot(data, 'toy', load_encoding()).sources == ()

def test_precision_groups_and_union_bytes_regression():
    metadata = {'left':{'kind':'drawer_text', 'source_id':'note', 'start_byte':0, 'end_byte':5},
        'right':{'kind':'drawer_text', 'source_id':'note', 'start_byte':7, 'end_byte':10},
        'graph':{'kind':'kg_record', 'source_id':'graph', 'start_byte':0, 'end_byte':10}}
    gold = {'status':'positive', 'required':[['left'], ['right']], 'acceptable':[], 'forbidden':[], 'corroboration':[],
        'supporting_spans':[{'source_id':'note', 'start_byte':0, 'end_byte':8},
                            {'source_id':'note', 'start_byte':3, 'end_byte':10}]}
    base = {'candidate_ids':['left','right'], 'selected_ids':['left'], 'evidence_ids':['left']}
    score = score_case(base, gold, metadata)
    with_graph = score_case({**base, 'evidence_ids':['left','graph']}, gold, metadata)
    assert score['drawer_relevant_chunk_fraction'] == with_graph['drawer_relevant_chunk_fraction'] == 1
    assert with_graph['kg_unresolved_emitted'] == 1 and with_graph['kg_judged_precision'] is None
    assert score['candidate_byte_coverage'] == 0.8
    assert score['evidence_byte_coverage'] == 0.5
    assert score['evidence_span_coverage'] == 0
    assert score['evidence_group_coverage'] == 0.5 and score['evidence_complete'] is False
    empty = score_case({key:[] for key in base}, gold, metadata)
    assert empty['evidence_complete'] is False and empty['evidence_group_coverage'] == 0

def test_required_groups_are_validated():
    corpus = adapt_snapshot(snapshot(), 'toy', load_encoding())
    prepared = json.loads(json.dumps({'queries':[{'id':'toy'}], 'sources':[asdict(s) for s in corpus.sources],
        'metadata':dict(corpus.evidence_metadata)}))
    identity = next(iter(corpus.evidence_metadata))
    gold = {'query_id':'toy','status':'positive','required':[['missing']], 'acceptable':[identity],
        'forbidden':[], 'corroboration':[], 'supporting_spans':[]}
    with pytest.raises(IntegrityError, match='required'):
        validate_gold({'judgments':[gold]}, prepared)
    with pytest.raises(IntegrityError, match='required'):
        validate_gold({'judgments':[{**gold, 'required':[]}]}, prepared)

@pytest.mark.parametrize('content', ['', ' \n\t '])
def test_empty_drawers_prepare_without_invented_facts(tmp_path, content):
    data = snapshot()
    data['drawers'][0]['record']['content'] = content
    snapshot_path = tmp_path / 'snapshot.json'
    sample_path = tmp_path / 'sample.json'
    write_private(snapshot_path, data)
    write_private(sample_path, {'queries':[{'id':f'toy-{i}', 'palace':'toy', 'prompt':'Alpha uses Beta',
        'logged_at':'1970-01-01T00:00:01Z', 'log_file':'toy.jsonl', 'line':i+1} for i in range(32)],
        'log_hashes':{'toy.jsonl':'toyhash'}})
    prepare(snapshot_path, sample_path, file_digest(sample_path), tmp_path / 'prepared', 'toy', load_encoding(), 10)
    result = read_json(tmp_path / 'prepared' / 'prepared.json')
    assert result['counts']['empty_drawers'] == 1 and result['counts']['drawer_chunks'] == 0
    assert len(result['sources']) == 1
    data['drawers'][0]['record']['content'] = None
    with pytest.raises(IntegrityError, match='content'):
        adapt_snapshot(data, 'toy', load_encoding())
