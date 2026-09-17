"""Indexed cursor bounds and strict transport response verification."""
from typing import Any
from urllib.parse import parse_qs, urlparse

import pytest
import kg_seek_adapter as kg


@pytest.mark.parametrize('target', [
    'a.rs:1:10', 'a.rs:1:100', 'a.rs:1:19',
    'a.rs::Function::run::1::10',
    'a.rs::Function::run::1::10::sub::0',
    'a.rs::Function::Foo::run::1::10::dup::11::sub::90',
])
def test_seek_bound_excludes_numeric_prefix_siblings(target: str) -> None:
    bound = kg.predecessor(target)
    assert bound.encode() < target.encode()
    prefix = target[:-1]
    digit = int(target[-1])
    siblings = [prefix + str(n) + tail for n in range(digit)
                for tail in ('', '0', '9', '99999', '::dup::9', '::sub::0')]
    assert all(sibling.encode() < bound.encode() for sibling in siblings)
    assert (target + '0').encode() > target.encode()


@pytest.mark.parametrize('target', ['x', '', 'a.rs:1:١', 'a.rs:1:9x'])
def test_seek_rejects_invalid_ids(target: str) -> None:
    with pytest.raises(ValueError):
        kg.predecessor(target)


@pytest.mark.parametrize('returned', ['match', 'other', 'empty', 'two'])
def test_transport_uses_indexed_seek_and_exact_id(monkeypatch: pytest.MonkeyPatch, returned: str) -> None:
    cid = 'b.rs::Function::target::10::20::sub::0'
    source = '\n'.join(str(n) for n in range(9))
    chunk = {'id': cid, 'file': '/fixture/b.rs', 'start_line': 10, 'end_line': 20,
             'content': source, 'language': 'rust', 'chunk_type': 'Function', 'calls': ['other']}
    def transport(base: str, path: str, body: dict[str, Any] | None = None) -> dict[str, Any]:
        if path.endswith('/search'):
            assert body is not None and body['text'] == 'seed'
            return {'results': [{'id': 'a.rs:1:2', 'path': 'a.rs', 'function_name': 'seed'}]}
        query = parse_qs(urlparse(path).query)
        if '/neighbors?' in path:
            assert query['node'] == ['a.rs::seed']
            return {'neighbors': [{'chunk_id': cid, 'edge': 'CallsFunction'}]}
        assert 'path_prefix' not in query
        assert query['limit'] == ['1']
        assert query['after'] == [kg.predecessor(cid)]
        rows = [chunk] if returned == 'match' else [] if returned == 'empty' else [chunk, chunk] if returned == 'two' else [{**chunk, 'id': cid+'1'}]
        return {'chunks': rows}
    monkeypatch.setattr(kg, 'request', transport)
    if returned != 'match':
        with pytest.raises(ValueError, match='exact ID'):
            kg.execute('http://127.0.0.1:18872', 'experiment', 'What does seed call?')
    else:
        result = kg.execute('http://127.0.0.1:18872', 'experiment', 'What does seed call?')
        assert result is not None
        hit = result['results'][0]
        assert hit['id'] == cid and hit['file'] == '/fixture/b.rs'
        assert hit['compact_snippet'] == '\n'.join(str(n) for n in range(7))
        assert hit['calls'] == ['other'] and hit['language'] == 'rust'
        assert len(result['trace']) == 3


def test_unrecognized_question_does_not_call_transport(monkeypatch: pytest.MonkeyPatch) -> None:
    def fail(*args: Any, **kwargs: Any) -> Any:
        raise AssertionError('unexpected transport')
    monkeypatch.setattr(kg, 'request', fail)
    assert kg.execute('http://127.0.0.1:18872', 'experiment', 'Find a definition') is None


@pytest.mark.parametrize('flag', ['bm25_lane_degraded', 'stale_index_root'])
def test_degraded_seed_is_rejected(monkeypatch: pytest.MonkeyPatch, flag: str) -> None:
    monkeypatch.setattr(kg, 'request', lambda *args: {'meta': {flag: True}})
    with pytest.raises(ValueError, match=flag):
        kg.execute('http://127.0.0.1:18872', 'experiment', 'Who calls seed?')
