"""Direct graph-name and fallback contracts without expected-answer labels."""
from typing import Any
from urllib.parse import parse_qs, urlparse

import pytest
import kg_name_adapter as kg
import kg_seek_adapter as seek


def test_direct_probe_no_search_deduplicates_orders_and_caps(monkeypatch: pytest.MonkeyPatch) -> None:
    ids = [f'b.rs::Function::b{n:02}::{n+1}::{n+1}' for n in range(12)]
    def request(base: str, path: str) -> dict[str, Any]:
        assert '/search' not in path
        query = parse_qs(urlparse(path).query)
        if '/neighbors?' in path:
            assert query['node'] == ['seed'] and query['direction'] == ['out']
            assert query['edge_kinds'] == ['CallsFunction'] and query['max_hops'] == ['1']
            return {'neighbors': [{'chunk_id': cid, 'edge': 'CallsFunction'} for cid in reversed(ids+ids)]}
        assert query['limit'] == ['1'] and 'path_prefix' not in query
        cid = next(cid for cid in ids if seek.predecessor(cid) == query['after'][0])
        return {'chunks': [{'id': cid, 'file': '/fixture/b.rs', 'content': '\n'.join(str(n) for n in range(9)),
                            'start_line': 1, 'end_line': 9, 'calls': ['another'], 'language': 'rust'}]}
    monkeypatch.setattr(kg, 'request', request)
    def no_fallback(*args: Any) -> Any:
        raise AssertionError('unexpected fallback')
    monkeypatch.setattr(seek, 'execute', no_fallback)
    result = kg.execute('http://127.0.0.1:18872', 'experiment', 'What does seed call?')
    assert result is not None
    assert [hit['id'] for hit in result['results']] == ids[:10]
    assert len(result['trace']) == 11 and result['name_probe'] == 'direct'
    assert result['results'][0]['calls'] == ['another']
    assert result['results'][0]['compact_snippet'] == '\n'.join(str(n) for n in range(7))


def test_empty_probe_fallback_preserves_trace_and_elapsed(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(kg, 'request', lambda *args: {'neighbors': []})
    fallback_trace = [{'method': 'POST', 'path': '/search', 'response': {'results': []}}]
    def fallback(base: str, index: str, text: str) -> dict[str, Any]:
        assert text == 'Who calls seed?'
        return {'results': [], 'trace': fallback_trace, 'seed_count': 0,
                'elapsed_ms': 0, 'latency_ms': 0}
    monkeypatch.setattr(seek, 'execute', fallback)
    result = kg.execute('http://127.0.0.1:18872', 'experiment', 'Who calls seed?')
    assert result is not None
    assert result['trace'][1:] == fallback_trace
    assert result['trace'][0]['response'] == {'neighbors': []}
    assert result['elapsed_ms'] > 0 and result['latency_ms'] == result['elapsed_ms']
    assert result['name_probe'] == 'fallback'


@pytest.mark.parametrize('chunks', [[], [{'id': 'wrong'}], [{'id': 'b.rs:1:2'}, {'id': 'b.rs:1:2'}]])
def test_nonexact_seek_fails_closed(monkeypatch: pytest.MonkeyPatch, chunks: list[dict[str, Any]]) -> None:
    responses = iter([{'neighbors': [{'chunk_id': 'b.rs:1:2', 'edge': 'CallsFunction'}]}, {'chunks': chunks}])
    monkeypatch.setattr(kg, 'request', lambda *args: next(responses))
    with pytest.raises(ValueError, match='exact ID'):
        kg.execute('http://127.0.0.1:18872', 'experiment', 'Who calls seed?')


def test_unknown_query_no_transport(monkeypatch: pytest.MonkeyPatch) -> None:
    def fail(*args: Any) -> Any:
        raise AssertionError('unexpected network')
    monkeypatch.setattr(kg, 'request', fail)
    assert kg.execute('http://127.0.0.1:18872', 'experiment', 'Find a declaration') is None
