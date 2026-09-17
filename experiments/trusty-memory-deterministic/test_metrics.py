from metrics import score, summarize


def query(**changes):
    return dict(id='q', split='heldout', scenario='initial', category='fact',
                mode='current', scope='one', relevant_ids=['a'], **changes)


def hit(ident='a', **changes):
    return dict(id=ident, scope='one', excerpt='exact current text', index_fresh=True, **changes)


def test_recall_and_rank_use_source_labels():
    row = score(query(), [hit('b'), hit()])
    assert row['recall_at_5'] == 1
    assert row['reciprocal_rank'] == .5
    assert summarize([row])['hits_at_5'] == 1


def test_unanswerable_is_not_a_retrieval_success():
    q = query(expected_empty=True)
    q['relevant_ids'] = []
    row = score(q, [])
    summary = summarize([row])
    assert summary['answerable'] == 0
    assert summary['mean_recall_at_5'] is None
    assert summary['mrr'] is None
    assert summary['correct_abstentions'] == 1


def test_independent_temporal_scope_and_evidence_errors():
    q = query(invalid_ids=['a'], forbidden_ids=['b'], required_evidence={'a':['new state']})
    bad = hit('b'); bad['scope'] = 'other'; bad['index_fresh'] = False
    row = score(q, [hit(), bad])
    assert row['invalid_hits'] == ['a']
    assert row['forbidden_hits'] == ['b']
    assert row['scope_errors'] == ['b']
    assert row['evidence_errors'] == ['a']
    assert row['index_stale_hits'] == 1


def test_no_hit_and_duplicate_are_not_hidden():
    assert score(query(), [])['reciprocal_rank'] == 0
    assert score(query(), [hit(), hit()])['duplicate_hits'] == 1


def test_scope_violation_union_not_double_counted():
    q = query(forbidden_ids=['b','c'])
    bad = hit('b'); bad['scope'] = 'other'
    summary = summarize([score(q,[bad,hit('c')])])
    assert summary['scope_errors'] == 2
    assert summary['scope_error_queries'] == 1
