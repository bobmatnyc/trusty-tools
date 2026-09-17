"""Invented fixtures only: source authority, coherent units, and native comparisons."""
from dataclasses import asdict, replace
import json
import math
import os
from pathlib import Path
import subprocess
import tempfile

import pytest
import passage_policy as policy
import passage_evaluate as evaluation
from passage_policy import (Span, build_lexicon, derive_passages, select_lexical,
                            pack_passages, validate_passages)
from passage_evaluate import score_spans, validate_gold, run_case
from real_adapter import adapt_snapshot, file_digest, write_private
from legacy import Evidence, IntegrityError, RustHelper, load_encoding, parse_source, eligible_facts
from contracts import Task, Selection
from maintenance import DerivedStore, derive_source
from plan_index import build_scoped_index

HELPER = Path('/Users/masa/trusty-search-experiment/target/debug/examples/memory_prompt_probe')
CACHE = Path(tempfile.gettempdir())/'data-gym-cache'


@pytest.fixture(scope='module')
def encoding():
    return load_encoding(CACHE)


@pytest.fixture(scope='module')
def helper():
    process = RustHelper(HELPER)
    try:
        yield process
    finally:
        process.close()


def corpus(encoding, body='Zircon pelican documents src/example.py and #123.\n\nA useful explanation.\n', count=20):
    drawers = [{'key_hex': f'{i+1:04x}', 'record': {'content': body if i == 0 else f'Ordinary generic filler {i}.',
        'expires_at_ms': None, 'created_at_ms': 1}} for i in range(count)]
    data = {'version': 'memory-real-snapshot-v1', 'captured_at_ms': 100000,
        'drawers': drawers, 'triples': [{'key_hex': 'eeee', 'row_kind': 'active', 'subject': 'Zircon',
            'predicate': 'uses', 'object': 'pelican', 'valid_from_ms': 1, 'valid_to_ms': None, 'provenance': None}],
        'counts': {'drawers': {'read': count, 'decoded': count, 'errors': 0},
                   'triples': {'read': 1, 'decoded': 1, 'errors': 0}}}
    adapted = adapt_snapshot(data, 'toy', encoding)
    return adapted, Task('Zircon pelican', 'toy', adapted.as_of, adapted.as_of)


def seeds(c):
    return tuple(Evidence(s, f) for s in c.sources for f in s.facts)


def test_selection_contract(encoding):
    c, task = corpus(encoding)
    lexicon = build_lexicon(c.sources, c.evidence_metadata, task)
    choices = (seeds(c)[-1], seeds(c)[0], seeds(c)[1])
    for prompt in ('Zircon pelican', 'src/example.py', '#123', '`Zircon`', 'src/EXAMPLE.py', 'please ' * 1000 + 'src/example.py'):
        result = select_lexical(replace(task, prompt=prompt), choices, lexicon)
        assert result.evidence == (choices[1],)
        assert (choices[0].id, 'kg_record') in result.rejections
    for prompt in ('please how should we do this', 'generic filler', 'missingentry', 'zircon'):
        assert not select_lexical(replace(task, prompt=prompt), choices, lexicon).evidence
    with pytest.raises(IntegrityError, match='context'):
        select_lexical(replace(task, scope='foreign'), choices, lexicon)
    with pytest.raises(IntegrityError, match='size'):
        select_lexical(replace(task, prompt='x'*65537), choices, lexicon)
    with pytest.raises(IntegrityError, match='duplicate'):
        select_lexical(task, (choices[1], choices[1]), lexicon)
    assert select_lexical(task, (), lexicon).rejections[-1] == ('query', 'no_candidates')


def test_rarity_mass_and_seed_cap(encoding):
    c, task = corpus(encoding, ('Zircon pelican '+ 'padding '*65)*12)
    lexicon = build_lexicon(c.sources, c.evidence_metadata, task)
    candidates = tuple(Evidence(c.sources[0], f) for f in c.sources[0].facts)[:20]
    selected = select_lexical(task, candidates, lexicon)
    assert len(selected.evidence) == 8
    assert [e.id for e in selected.evidence] == [e.id for e in candidates[:8]]
    assert any(reason == 'seed_cap' for _, reason in selected.rejections)
    assert select_lexical(task, candidates, replace(lexicon, source_count=10)).evidence
    assert not select_lexical(task, candidates, replace(lexicon, source_count=9)).evidence
    extra = dict(lexicon.document_frequency, **{f'unique{i}': 1 for i in range(20)})
    assert not select_lexical(replace(task, prompt=task.prompt+' '+' '.join(f'unique{i}' for i in range(20))),
                              candidates, replace(lexicon, document_frequency=extra)).evidence


def test_exact_quarter_support_across_hash_seeds(encoding):
    body = 'Zircon pelican ' + 'padding '*150 + 'nebula quartz saffron topaz walnut xenon'
    c, task = corpus(encoding, body)
    task = replace(task, prompt='Zircon pelican nebula quartz saffron topaz walnut xenon')
    lexicon = build_lexicon(c.sources, c.evidence_metadata, task)
    varied = dict(lexicon.document_frequency)
    varied.update({term: 2 for term in ('pelican', 'quartz', 'topaz', 'xenon')})
    lexicon = replace(lexicon, document_frequency=varied)
    first = Evidence(c.sources[0], c.sources[0].facts[0])
    assert select_lexical(task, (first,), lexicon).evidence == (first,)
    script = '''import sys, json
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from test_passage import corpus, build_lexicon, select_lexical, Evidence, replace, load_encoding
import passage_policy as policy
c, task = corpus(load_encoding(Path(sys.argv[2])), sys.argv[3])
task = replace(task, prompt=sys.argv[4])
lexicon = build_lexicon(c.sources, c.evidence_metadata, task)
varied = dict(lexicon.document_frequency)
varied.update({term: 2 for term in ('pelican', 'quartz', 'topaz', 'xenon')})
lexicon = replace(lexicon, document_frequency=varied)
first = Evidence(c.sources[0], c.sources[0].facts[0])
observed = []
original_fsum = policy.math.fsum
def canonical_sum(values):
    arguments = tuple(values)
    observed.append(arguments)
    return original_fsum(arguments)
policy.math.fsum = canonical_sum
print(json.dumps([[e.id for e in select_lexical(task, (first,), lexicon).evidence], observed]))
'''
    outputs = []
    for seed in ('1', '7', '99'):
        result = subprocess.run([sys_executable(), '-c', script, str(Path(policy.__file__).parent),
            str(CACHE), body, task.prompt], env={**os.environ, 'PYTHONHASHSEED': seed},
            capture_output=True, text=True, timeout=15)
        assert result.returncode == 0, result.stderr
        outputs.append(json.loads(result.stdout))
    weights = {term: math.log((lexicon.source_count+1)/(varied[term]+1))+1
               for term in task.prompt.casefold().split()}
    expected_arguments = [[weights[term] for term in ('pelican', 'zircon')],
                          [weights[term] for term in sorted(weights)]]
    assert outputs == [[[first.id], expected_arguments]]*3


def test_units_and_splitting(encoding):
    body = '# Heading\r\n\r\n日本語🙂 prose.\r\n\r\n- first item\r\n  continuation\r\n- next item\r\n\r\n```sh\r\nprintf hello\r\n# not a heading\r\n```\r\n\r\n# Next\r\nFinal.\r\n'
    c, task = corpus(encoding, body)
    view = derive_passages(c.sources, c.evidence_metadata, task, encoding)
    units = view.units[c.sources[0].source_id]
    assert len(units) == 7
    assert sum(u.atomic for u in units) == 1
    assert view.original_fingerprints[c.sources[0].source_id] != view.sources[0].fingerprint
    for e in (*view.base.values(), *view.preferred.values()):
        assert e.fact.claim.encode() == e.source.body.encode()[e.fact.start_byte:e.fact.end_byte]
        assert len(encoding.encode(e.fact.claim)) <= 160
        assert '# not a heading' not in e.fact.claim or '```sh' in e.fact.claim and '```\r\n' in e.fact.claim
    assert view.representation['representable_byte_fraction'] > 0
    assert all(e.fact.claim.count('# Heading') + e.fact.claim.count('# Next') < 2 for e in view.preferred.values())


def test_oversized_prose_clauses_and_atomic_fences(encoding):
    body = 'Zircon pelican prose. '*150 + '\n\n' + 'Clause without period; '*100 + '\n\n```sh\n' + 'echo command\n'*200 + '```\n'
    c, task = corpus(encoding, body)
    view = derive_passages(c.sources, c.evidence_metadata, task, encoding)
    units = view.units[c.sources[0].source_id]
    assert len(units) == 251
    assert units[-1].oversized and units[-1].atomic
    assert view.representation['oversized_fences'] == 1
    assert view.representation['notes_with_oversized_loss'] == 1
    assert all('echo command' not in e.fact.claim for e in view.base.values())
    unclosed, _ = corpus(encoding, '```sh\necho hi\n# protected\n')
    result = derive_passages(unclosed.sources, unclosed.evidence_metadata, task, encoding)
    assert result.units[unclosed.sources[0].source_id][0].atomic
    assert result.base[next(iter(result.base))].fact.claim == unclosed.sources[0].body


def test_split_protects_identifiers_and_inline_code():
    text = 'Dr. Person uses a/b.py. Version 1.2.3. Value 3.14. `inline. protected; code` End. Next! More? Last; Clause'
    parts = policy._fragments(text, '.!?')
    assert ''.join(parts) == text
    assert parts[0].endswith('End. ')
    assert parts[1:3] == ['Next! ', 'More? ']
    assert policy._fragments('``inline ` stays. inside`` End. Next', '.!?') == ['``inline ` stays. inside`` End. ', 'Next']
    assert policy._fragments('`unclosed. code; remains', '.!?;') == ['`unclosed. code; remains']


def test_eligibility_and_clock_contract(encoding):
    c, task = corpus(encoding, 'alpha beta '*150)
    first = c.sources[0]
    for altered in (replace(first, deleted=True), replace(first, expires_at=task.as_of),
                    replace(first, observed_at='2999-01-01T00:00:00Z')):
        sources = (altered, *c.sources[1:])
        assert first.source_id not in build_lexicon(sources, c.evidence_metadata, task).drawer_bodies
    invalid = replace(first, facts=(replace(first.facts[0], valid_to=task.as_of), *first.facts[1:]))
    with pytest.raises(IntegrityError, match='partially'):
        derive_passages((invalid, *c.sources[1:]), c.evidence_metadata, task, encoding)
    with pytest.raises(IntegrityError, match='duplicate'):
        derive_passages((first, first), c.evidence_metadata, task, encoding)


def test_native_packets(encoding, helper):
    c, task = corpus(encoding)
    lexicon = build_lexicon(c.sources, c.evidence_metadata, task)
    view = derive_passages(c.sources, c.evidence_metadata, task, encoding)
    store = DerivedStore({s.key: s for s in c.sources})
    store.records = {s.key: derive_source(s) for s in c.sources}
    index = build_scoped_index(c.sources, task, store, helper)
    try:
        cases = [run_case(task, arm, index, helper, budget, encoding, c.sources, lexicon, view)
                 for arm in evaluation.ARMS for budget in (128, 256)]
        assert len({tuple(c['candidate_ids']) for c in cases}) == 1
        assert len({tuple(c['selected_ids']) for c in cases if c['arm'] != 'raw_bm25'}) == 1
        assert all(c['tokens'] <= c['budget'] and len(c['timing_samples_ns']) == 3 for c in cases)
        assert all(c['expansion_bytes'] >= 0 for c in cases)
        assert all(c['emitted_kg_count'] == 0 for c in cases if c['arm'] != 'raw_bm25')
        assert any(c['emitted_ids'] for c in cases if c['arm'] == 'lexical_passages')
        with pytest.raises(IntegrityError, match='context'):
            run_case(replace(task, scope='wrong'), 'raw_bm25', index, helper, 128, encoding, c.sources, lexicon, view)
    finally:
        index.close()


def test_forged_views_and_overlap(encoding, helper):
    c, task = corpus(encoding, 'First paragraph.\n\nSecond paragraph.\n\nThird paragraph.\n')
    selection = Selection(tuple(Evidence(c.sources[0], f) for f in c.sources[0].facts))
    view = derive_passages(c.sources, c.evidence_metadata, task, encoding)
    result = pack_passages(task, selection, view, 256, helper, encoding)
    validate_passages(result, c.sources, view, selection, task, 256, helper, encoding)
    assert all(not policy._overlap(a, b) for i, a in enumerate(result.packet.evidence)
               for b in result.packet.evidence[i+1:])
    assert any(reason == 'overlap' for _, reason in result.rejections)
    for forged in (replace(result, packet=replace(result.packet, text='forged')),
                   replace(result, expanded_candidates=()), replace(result, seed_ids={})): 
        with pytest.raises(IntegrityError, match='differs'):
            validate_passages(forged, c.sources, view, selection, task, 256, helper, encoding)
    changed = (replace(c.sources[0], body='changed'), *c.sources[1:])
    with pytest.raises(IntegrityError):
        validate_passages(result, changed, view, selection, task, 256, helper, encoding)
    with pytest.raises(IntegrityError, match='missing'):
        validate_passages(result, c.sources[1:], view, selection, task, 256, helper, encoding)
    with pytest.raises(IntegrityError, match='budget'):
        pack_passages(task, selection, view, 127, helper, encoding)


def test_budget_whole_units_and_fallback(encoding, helper):
    c, task = corpus(encoding, 'Zircon pelican '+'word '*130+'.\n\n'+'small neighbor.\n')
    view = derive_passages(c.sources, c.evidence_metadata, task, encoding)
    selection = Selection((Evidence(c.sources[0], c.sources[0].facts[0]),))
    small = pack_passages(task, selection, view, 128, helper, encoding)
    big = pack_passages(task, selection, view, 256, helper, encoding)
    assert not small.packet.evidence
    assert any(reason == 'budget' for _, reason in small.rejections)
    assert big.packet.evidence and big.packet.tokens <= 256
    assert big.packet.evidence[0].fact.claim.endswith('small neighbor.\n')


def test_validator_rejects_globally_superseded_seed(encoding, helper):
    c, task = corpus(encoding)
    older = replace(c.sources[0], facts=tuple(replace(f, single_value_slot='exclusive') for f in c.sources[0].facts))
    newer = replace(c.sources[1], revision=2,
                    facts=tuple(replace(f, single_value_slot='exclusive') for f in c.sources[1].facts))
    originals = (older, newer, *c.sources[2:])
    selected = Selection((Evidence(older, older.facts[0]),))
    assert selected.evidence[0] not in eligible_facts(originals, task.legacy_query())
    forged_view = derive_passages((older,), c.evidence_metadata, task, encoding)
    forged = pack_passages(task, selected, forged_view, 256, helper, encoding)
    assert forged.packet.evidence
    with pytest.raises(IntegrityError, match='ineligible'):
        validate_passages(forged, originals, forged_view, selected, task, 256, helper, encoding)


def test_parsed_empty_tombstone_is_excluded(encoding):
    c, task = corpus(encoding)
    tombstone = parse_source(json.loads(json.dumps(asdict(replace(c.sources[0], deleted=True, body='', facts=())))))
    sources = (tombstone, *c.sources[1:])
    lexicon = build_lexicon(sources, c.evidence_metadata, task)
    view = derive_passages(sources, c.evidence_metadata, task, encoding)
    assert tombstone.source_id not in lexicon.drawer_bodies
    assert tombstone.source_id not in view.units
    assert view.representation['notes'] == 19


def test_validator_rejects_partial_source_eligibility(encoding, helper):
    c, task = corpus(encoding, 'Zircon pelican passage. '*100)
    view = derive_passages(c.sources, c.evidence_metadata, task, encoding)
    source = c.sources[0]
    selected = Selection((Evidence(source, source.facts[1]),))
    result = pack_passages(task, selected, view, 256, helper, encoding)
    partial = replace(source, facts=(replace(source.facts[0], valid_to=task.as_of), *source.facts[1:]))
    originals = (partial, *c.sources[1:])
    selected = Selection((Evidence(partial, partial.facts[1]),))
    assert selected.evidence[0] in eligible_facts(originals, task.legacy_query())
    with pytest.raises(IntegrityError, match='partially ineligible'):
        validate_passages(result, originals, view, selected, task, 256, helper, encoding)


def prepared_gold(c):
    prepared = {'sources': [asdict(s) for s in c.sources], 'metadata': dict(c.evidence_metadata),
        'scope': 'toy', 'as_of': c.as_of, 'snapshot_sha256': 'synthetic-snapshot', 'sample_sha256': 'synthetic-sample',
        'queries': [{'id': f'toy-{i}', 'prompt': 'Zircon pelican', 'palace': 'toy',
                     'logged_at': '1970-01-01T00:00:01Z'} for i in range(32)]}
    span = {'source_id': c.sources[0].source_id, 'start_byte': 0, 'end_byte': len('Zircon pelican')}
    gold = {'judgments': [{'query_id': f'toy-{i}', 'status': 'positive' if i == 0 else 'unavailable',
                          'supporting_spans': [span] if i == 0 else [], 'required': ['ignored-chunk-id'],
                          'optional_spans': [{'ignored': 'annotation'}]} for i in range(32)]}
    return json.loads(json.dumps(prepared)), gold


def test_gold_and_intervals(encoding):
    c, _ = corpus(encoding)
    prepared, gold = prepared_gold(c)
    judgments = validate_gold(gold, prepared)
    assert len(judgments) == 32
    assert judgments[0].supporting_spans[0].source_body_sha256 == policy._body_hash(c.sources[0].body)
    a = Span('note', 0, 10, 'sha')
    b = Span('note', 5, 15, 'sha')
    result = score_spans((Span('note', 0, 7, 'sha'), Span('note', 7, 15, 'sha')), (a, b))
    assert result == {'byte_coverage': 1, 'full_span_fraction': 1, 'complete': True, 'note_recall': 1}
    result = score_spans((Span('note', 0, 7, 'sha'), Span('note', 8, 15, 'sha')), (a, b))
    assert result['byte_coverage'] == 14/15 and result['complete'] is False
    assert score_spans((), (a,))['byte_coverage'] == 0
    assert score_spans((a,), ())['complete'] is None
    with pytest.raises(IntegrityError, match='digest'):
        score_spans((replace(a, source_body_sha256='bad'),), (a,))
    for mutation in ('digest', 'quote', 'duplicate', 'status', 'source', 'missing'):
        changed = json.loads(json.dumps(gold))
        item = changed['judgments'][0]
        if mutation in ('digest', 'quote'):
            item['supporting_spans'][0]['source_body_sha256' if mutation == 'digest' else 'quote'] = 'bad'
        elif mutation == 'duplicate':
            item['supporting_spans'] *= 2
        elif mutation == 'status':
            item['status'] = 'negative'
        elif mutation == 'source':
            item['supporting_spans'][0]['source_id'] = c.sources[-1].source_id
        else:
            changed['judgments'].pop()
        with pytest.raises(IntegrityError):
            validate_gold(changed, prepared)


def test_utf8_gold_boundary(encoding):
    c, _ = corpus(encoding, 'Zircon pelican🙂')
    prepared, gold = prepared_gold(c)
    gold['judgments'][0]['supporting_spans'][0].update(start_byte=14, end_byte=15)
    with pytest.raises(IntegrityError, match='UTF-8'):
        validate_gold(gold, prepared)


def test_cli_evaluate(encoding, tmp_path):
    c, _ = corpus(encoding)
    prepared, gold = prepared_gold(c)
    prepared_path, gold_path = tmp_path/'prepared.json', tmp_path/'gold.json'
    write_private(prepared_path, prepared)
    write_private(gold_path, gold)
    scratch = tmp_path/'scratch'
    scratch.mkdir(mode=0o700)
    output = tmp_path/'results'
    command = [sys_executable(), str(Path(evaluation.__file__)), '--prepared', str(prepared_path),
        '--gold', str(gold_path), '--output', str(output), '--helper', str(HELPER), '--cache', str(CACHE),
        '--prepared-sha256', file_digest(prepared_path), '--gold-sha256', file_digest(gold_path)]
    result = subprocess.run(command, env={**os.environ, 'TMPDIR': str(scratch)}, capture_output=True, text=True, timeout=90)
    assert result.returncode == 0, result.stderr
    summary = json.loads((output/'summary.json').read_text())
    assert all(summary[f'{arm}-128']['unavailable'] == 31 for arm in evaluation.ARMS)
    assert summary['lexical_passages-128']['negative_abstention'] is None
    cases = json.loads((output/'cases.json').read_text())
    assert len(cases) == 192
    assert (output/'cases.json').stat().st_mode & 0o777 == 0o600
    assert output.stat().st_mode & 0o777 == 0o700
    provenance = json.loads((output/'provenance.json').read_text())
    assert provenance['representation']['notes'] == 20 and provenance['code_sha256']
    result = subprocess.run(command, env={**os.environ, 'TMPDIR': str(scratch)}, capture_output=True, text=True, timeout=90)
    assert result.returncode != 0 and 'FileExistsError' in result.stderr
    command[-1] = 'bad'
    result = subprocess.run(command, env={**os.environ, 'TMPDIR': str(scratch)}, capture_output=True, text=True, timeout=90)
    assert result.returncode != 0 and 'IntegrityError' in result.stderr


def sys_executable():
    import sys
    return sys.executable
