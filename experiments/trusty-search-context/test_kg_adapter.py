"""Adapter contracts; mocked transport isolates query and pagination logic."""
from urllib.parse import parse_qs, urlparse

import pytest
import kg_adapter as kg


@pytest.mark.parametrize('text,expected', [
    ('What does load_config call?', ('load_config', 'out')),
    ('Who calls load_config?', ('load_config', 'in')),
    ('What does Foo::load call?', ('Foo::load', 'out')),
    ('Where is load_config defined?', None),
])
def test_query_only_direction(text, expected):
    assert kg.query_intent(text) == expected


@pytest.mark.parametrize('cid,path', [
    ('src/a.rs::Function::Foo::run::1::8::dup::1::sub::0', 'src/a.rs'),
    ('src/a.rs:1:8', 'src/a.rs'),
    ('src/a.rs::Function::run::1::8', 'src/a.rs'),
])
def test_chunk_id_shapes(cid, path):
    assert kg.chunk_path(cid) == path


def test_unknown_query_makes_no_requests(monkeypatch):
    monkeypatch.setattr(kg, 'request', lambda *args: pytest.fail('unexpected request'))
    assert kg.execute('http://127.0.0.1:18872', 'experiment', 'Find a definition') is None


def test_exact_seeds_dedup_direction_and_pagination(monkeypatch):
    ids = ['src/b.rs::Function::b::10::20', 'src/b.rs::Function::c::30::40']
    def transport(base, path, body=None):
        if path.endswith('/search'):
            assert body['text'] == 'a' and body['top_k'] == 3
            return {'results': [{'id': 'src/a.rs::Function::a::1::9', 'path': 'src/a.rs', 'function_name': 'a'},
                                {'id': 'wrong', 'function_name': 'near_a'}]}
        query = parse_qs(urlparse(path).query, keep_blank_values=True)
        if '/graph/neighbors?' in path:
            assert query['node'] == ['src/a.rs::a']
            assert query['direction'] == ['in']
            assert query['edge_kinds'] == ['CallsFunction']
            assert query['max_hops'] == ['1']
            return {'neighbors': [{'symbol': 'c', 'chunk_id': ids[1], 'edge': 'CallsFunction'},
                                  {'symbol': 'b', 'chunk_id': ids[0], 'edge': 'CallsFunction'},
                                  {'symbol': 'b', 'chunk_id': ids[0], 'edge': 'CallsFunction'}]}
        assert query['path_prefix'] == ['src/b.rs']
        n = 0 if query['after'] == [''] else 1
        return {'chunks': [{'id': ids[n], 'start_line': 10+20*n, 'end_line': 20+20*n,
                            'file': '/fixture/src/b.rs', 'language': 'rust', 'calls': ['helper'], 'chunk_type': 'Function', 'content': 'actual source', 'function_name': ['b', 'c'][n]}],
                'next_cursor': ids[0] if n == 0 else None}
    monkeypatch.setattr(kg, 'request', transport)
    result = kg.execute('http://127.0.0.1:18872', 'experiment', 'Who calls a?')
    assert [h['id'] for h in result['results']] == ids
    assert result['results'][1]['start_line'] == 30
    assert result['results'][1]['file'] == '/fixture/src/b.rs'
    assert result['results'][1]['compact_snippet'] == 'actual source'
    assert result['results'][1]['calls'] == ['helper']
    assert result['results'][1]['chunk_type'] == 'Function'
    assert len(result['trace']) == 4
    assert result['elapsed_ms'] >= 0
    assert all(h['match_reason'] == 'explicit_kg' for h in result['results'])


def test_missing_graph_chunk_is_error(monkeypatch):
    replies = iter([{'results': [{'id': 'a.rs:1:2', 'path': 'a.rs', 'function_name': 'a'}]},
                    {'neighbors': [{'chunk_id': 'b.rs:1:2', 'edge': 'CallsFunction'}]},
                    {'chunks': [], 'next_cursor': None}])
    monkeypatch.setattr(kg, 'request', lambda *args: next(replies))
    with pytest.raises(ValueError, match='absent corpus'):
        kg.execute('http://127.0.0.1:18872', 'experiment', 'What does a call?')


def test_neighbor_limit_is_ten(monkeypatch):
    ids = [f'b.rs::Function::b{n:02}::{n+1}::{n+1}' for n in range(15)]
    replies = iter([{'results': [{'id': 'a.rs:1:2', 'path': 'a.rs', 'function_name': 'a'}]},
                    {'neighbors': [{'chunk_id': cid, 'edge': 'CallsFunction'} for cid in reversed(ids)]},
                    {'chunks': [{'id': cid, 'start_line': n+1, 'end_line': n+1, 'file': '/fixture/b.rs', 'content': 'b'}
                                for n, cid in enumerate(ids)], 'next_cursor': None}])
    monkeypatch.setattr(kg, 'request', lambda *args: next(replies))
    result = kg.execute('http://127.0.0.1:18872', 'experiment', 'What does a call?')
    assert [h['id'] for h in result['results']] == ids[:10]


def test_stalled_cursor_is_error(monkeypatch):
    replies = iter([{'results': [{'id': 'a.rs:1:2', 'path': 'a.rs', 'function_name': 'a'}]},
                    {'neighbors': [{'chunk_id': 'b.rs:1:2', 'edge': 'CallsFunction'}]},
                    {'chunks': [], 'next_cursor': 'same'}, {'chunks': [], 'next_cursor': 'same'}])
    monkeypatch.setattr(kg, 'request', lambda *args: next(replies))
    with pytest.raises(ValueError, match='did not advance'):
        kg.execute('http://127.0.0.1:18872', 'experiment', 'What does a call?')


@pytest.mark.parametrize('flag', ['bm25_lane_degraded', 'stale_index_root'])
def test_degraded_seed_refused_before_traversal(monkeypatch, flag):
    replies = iter([{'meta': {flag: True}, 'results': []}])
    monkeypatch.setattr(kg, 'request', lambda *args: next(replies))
    with pytest.raises(ValueError, match=flag):
        kg.execute('http://127.0.0.1:18872', 'experiment', 'Who calls a?')


@pytest.mark.parametrize('source,expected', [('\n'.join(str(n) for n in range(9)), '\n'.join(str(n) for n in range(7))), ('short\n', 'short\n')])
def test_snippet_matches_existing_seven_line_contract(monkeypatch, source, expected):
    replies = iter([{'results': [{'id': 'a.rs:1:2', 'path': 'a.rs', 'function_name': 'a'}]},
                    {'neighbors': [{'chunk_id': 'b.rs:1:9', 'edge': 'CallsFunction'}]},
                    {'chunks': [{'id': 'b.rs:1:9', 'file': '/fixture/b.rs', 'start_line': 1,
                                 'end_line': 9, 'content': source}], 'next_cursor': None}])
    monkeypatch.setattr(kg, 'request', lambda *args: next(replies))
    result = kg.execute('http://127.0.0.1:18872', 'experiment', 'Who calls a?')
    assert result['results'][0]['compact_snippet'] == expected
