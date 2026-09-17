import hashlib
import pytest
from evaluate import DEFAULT, EvaluationError, canonical, objective, request, validate_hits, verify_freeze


def test_engine_never_receives_query_labels():
    query = dict(id='q',text='what',scope='s',mode='current',as_of='2026-01-01T00:00:00Z',
                 knowledge_cutoff=None,top_k=5,relevant_ids=['secret-label'],invalid_ids=['x'])
    req = request('raw',DEFAULT,None,queries=[query])
    assert 'relevant_ids' not in req['queries'][0]
    assert b'secret-label' not in canonical(req)
    assert req['maintenance']['max_documents'] == 0


def test_selection_prioritizes_temporal_safety_over_hits():
    safe = dict(scope_errors=0,invalid_queries=0,evidence_errors=0,hits_at_5=1,
                mean_recall_at_5=.1,mrr=.1,card_tokens=20,snapshot_bytes=200)
    unsafe = dict(safe,invalid_queries=1,hits_at_5=10)
    assert objective({'summary':safe}) > objective({'summary':unsafe})


def test_locator_validation_rejects_fabricated_excerpt():
    body = 'A café record'
    source = {('s','a'):{'body':body,'_revision':1}}
    hit = dict(id='a',scope='s',rank=1,score=1,body_digest=hashlib.sha256(body.encode()).hexdigest(),
               byte_start=0,byte_end=len(body.encode()),excerpt=body,revision=1,line_start=1,line_end=1)
    validate_hits({'top_k':5},[hit],source)
    with pytest.raises(EvaluationError,match='Excerpt'):
        validate_hits({'top_k':5},[dict(hit,excerpt='fabricated')],source)


def test_frozen_inputs_still_match_manifest():
    assert len(verify_freeze()) == 3


def test_locator_rejects_revision_lines_and_duplicates():
    source = {('s','a'):{'body':'abc','_revision':2}}
    hit = dict(id='a',scope='s',rank=1,score=1,body_digest=hashlib.sha256(b'abc').hexdigest(),
               byte_start=0,byte_end=3,excerpt='abc',revision=2,line_start=1,line_end=1)
    for change in ({'revision':999},{'line_start':99},{'line_end':99}):
        with pytest.raises(EvaluationError):
            validate_hits({'top_k':5},[dict(hit,**change)],source)
    with pytest.raises(EvaluationError,match='Duplicate'):
        validate_hits({'top_k':5},[hit,dict(hit,rank=2)],source)
