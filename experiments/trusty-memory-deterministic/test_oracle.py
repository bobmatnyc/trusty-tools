from copy import deepcopy
import pytest
from oracle import SourceOracle


def seed():
    oracle = SourceOracle()
    mutations = [dict(op='upsert',revision=1,drawer=dict(id=i,scope='s',body=i)) for i in ('a','unqueried')]
    oracle.apply(mutations)
    state = dict(sources=list(deepcopy(oracle.sources).values()),tombstones=[])
    return oracle,state


def test_oracle_detects_changed_and_unqueried_lost_source():
    oracle,state = seed(); oracle.check(state)
    corrupted = deepcopy(state); corrupted['sources'][0]['drawer']['body'] = 'corrupt'
    with pytest.raises(ValueError,match='source store'):
        oracle.check(corrupted)
    state['sources'].pop()
    with pytest.raises(ValueError,match='source store'):
        oracle.check(state)


def test_oracle_preserves_tombstone_against_late_update():
    oracle,_ = seed()
    oracle.apply([dict(op='remove',scope='s',id='a',revision=2),
                  dict(op='upsert',revision=1,drawer=dict(id='a',scope='s',body='old'))])
    assert ('s','a') not in oracle.sources
    assert oracle.tombstones[('s','a')] == 2
