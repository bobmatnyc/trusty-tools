"""Frozen three-arm measurement against mandatory original-source spans for #8246.

Why: Chunk IDs cannot measure coherent-passage support fairly.
What: Validate private inputs, execute unchanged candidates and score byte unions.
Test: test_gold_and_intervals, test_cli_evaluate.
"""
from __future__ import annotations

import argparse
from dataclasses import asdict
from datetime import datetime
import json
import os
from pathlib import Path
import statistics
import sys
import tempfile
from time import perf_counter_ns
from typing import Literal, Mapping, Sequence, cast

import tiktoken
import passage_policy as policy
from passage_policy import (Span, PassageView, Lexicon, build_lexicon, derive_passages,
    select_lexical, pack_passages, validate_passages, _body_hash, _drawers, _context)
from legacy import (Source, Evidence, RustHelper, JSON, IntegrityError, obj, array, string,
    integer, parse_source, digest, load_encoding, validate_packet)
from contracts import Task, Selection
from plan_index import ScopedIndex, build_scoped_index, validate_context
from maintenance import DerivedStore, derive_source
from relevance import packet
from real_adapter import file_digest, read_json, private_dir, write_private
from real_evaluate import merge_intervals
from offline_encoding import CONTENT_SHA256

Arm = Literal['raw_bm25', 'lexical_fixed', 'lexical_passages']
Status = Literal['positive', 'negative', 'unavailable', 'ambiguous']
ARMS: tuple[Arm, ...] = ('raw_bm25', 'lexical_fixed', 'lexical_passages')


from dataclasses import dataclass


@dataclass(frozen=True)
class Judgment:
    query_id: str
    status: Status
    supporting_spans: tuple[Span, ...]


def _metadata(prepared: Mapping[str, JSON]) -> dict[str, Mapping[str, JSON]]:
    return {k: obj(v) for k, v in obj(prepared['metadata']).items()}


def validate_gold(data: Mapping[str, JSON], prepared: Mapping[str, JSON]) -> tuple[Judgment, ...]:
    """Convert minimal mandatory spans using original bytes; test_gold_and_intervals."""
    queries = [string(obj(q)['id']) for q in array(prepared['queries'])]
    if len(queries) != 32 or len(set(queries)) != 32:
        raise IntegrityError('expected exactly 32 unique sample IDs')
    sources = tuple(parse_source(s) for s in array(prepared['sources']))
    task = Task('', string(prepared['scope']), string(prepared['as_of']), string(prepared['as_of']))
    drawers = {s.source_id: s for s in _drawers(sources, _metadata(prepared), task)}
    judgments: dict[str, Judgment] = {}
    for value in array(data['judgments']):
        row = obj(value)
        identity, status = string(row['query_id']), string(row['status'])
        if identity in judgments or status not in ('positive', 'negative', 'unavailable', 'ambiguous'):
            raise IntegrityError('duplicate judgment or invalid status')
        spans = []
        for value in array(row['supporting_spans']):
            item = obj(value)
            sid, start, end = string(item['source_id']), integer(item['start_byte']), integer(item['end_byte'])
            if sid not in drawers or not 0 <= start < end <= len(drawers[sid].body.encode()):
                raise IntegrityError('invalid original drawer span')
            body = drawers[sid].body
            try:
                quote = body.encode()[start:end].decode()
            except UnicodeDecodeError as error:
                raise IntegrityError('invalid UTF-8 gold boundary') from error
            sha = _body_hash(body)
            if item.get('source_body_sha256', sha) != sha or item.get('quote', quote) != quote:
                raise IntegrityError('gold body digest or quote differs')
            spans.append(Span(sid, start, end, sha))
        if len(set(spans)) != len(spans) or bool(spans) != (status == 'positive'):
            raise IntegrityError('duplicate spans or status contradicts required support')
        judgments[identity] = Judgment(identity, cast(Status, status), tuple(spans))
    if set(judgments) != set(queries):
        raise IntegrityError('gold/sample IDs differ')
    return tuple(judgments[q] for q in queries)


def score_spans(returned: Sequence[Span], required: Sequence[Span]) -> dict[str, JSON]:
    """Union overlapping evidence without hiding gaps; test_gold_and_intervals."""
    if not required:
        return dict.fromkeys(('byte_coverage', 'full_span_fraction', 'complete', 'note_recall'))
    sources = {s.source_id for s in required}
    covered = full = total = notes = 0
    for sid in sources:
        wanted = [s for s in required if s.source_id == sid]
        expected = {s.source_body_sha256 for s in wanted}
        got = [s for s in returned if s.source_id == sid]
        if len(expected) != 1 or any(s.source_body_sha256 not in expected for s in got):
            raise IntegrityError('coverage body digest mismatch')
        if any(not 0 <= s.start_byte < s.end_byte for s in wanted + got):
            raise IntegrityError('invalid coverage interval')
        gold_union = merge_intervals([(s.start_byte, s.end_byte) for s in wanted])
        have = merge_intervals([(s.start_byte, s.end_byte) for s in got])
        intersections = merge_intervals([(max(a, x), min(b, y)) for a, b in gold_union for x, y in have
                                        if a < y and x < b])
        supported = sum(b-a for a, b in intersections)
        covered += supported
        total += sum(b-a for a, b in gold_union)
        notes += supported > 0
        full += sum(any(a <= s.start_byte and b >= s.end_byte for a, b in have) for s in wanted)
    return {'byte_coverage': covered/total, 'full_span_fraction': full/len(required),
            'complete': full == len(required), 'note_recall': notes/len(sources)}


def _spans(evidence: Sequence[Evidence], lexicon: Lexicon) -> tuple[Span, ...]:
    return tuple(Span(e.source.source_id, e.fact.start_byte, e.fact.end_byte,
        _body_hash(e.source.body)) for e in evidence if e.source.source_id in lexicon.drawer_bodies)


def run_case(task: Task, arm: Arm, index: ScopedIndex, helper: RustHelper,
              budget: int, encoding: tiktoken.Encoding, originals: tuple[Source, ...],
              lexicon: Lexicon, view: PassageView) -> dict[str, JSON]:
    """Measure repeated public-input-only cases; test_native_packets."""
    validate_context(task, index)
    if arm not in ARMS or budget not in (128, 256) or len(task.prompt.encode()) > 65536:
        raise IntegrityError('unknown arm, budget or excessive prompt')
    if _context(task) != lexicon.task_context or _context(task) != view.task_context:
        raise IntegrityError('selection or passage context differs')
    if any(f.standing for s in originals for f in s.facts):
        raise IntegrityError('unexpected standing prelude')
    previous: str | None = None
    samples: list[dict[str, int]] = []
    result: dict[str, JSON] = {}
    for repetition in range(4):
        start = perf_counter_ns()
        response = helper.request({'op': 'search', 'projection': index.claim_id, 'text': task.prompt, 'limit': 20})
        ids = [string(obj(hit)['id']) for hit in array(response['hits'])]
        if len(ids) > 20 or len(set(ids)) != len(ids) or any(i not in index.evidence for i in ids):
            raise IntegrityError('invalid native candidate IDs')
        candidates = tuple(index.evidence[i] for i in ids)
        retrieved = perf_counter_ns()
        selection = Selection(candidates) if arm == 'raw_bm25' else select_lexical(task, candidates, lexicon)
        selected = perf_counter_ns()
        passage_result = None
        if arm == 'lexical_passages':
            passage_result = pack_passages(task, selection, view, budget, helper, encoding)
            packed, reasons = passage_result.packet, passage_result.rejections
            expanded = passage_result.expanded_candidates
        else:
            packed, reasons = packet(task, selection, index, budget, helper, encoding)
            expanded = selection.evidence
        finished = perf_counter_ns()
        if passage_result is not None:
            validate_passages(passage_result, originals, view, selection, task, budget, helper, encoding)
        else:
            validate_packet(packed, originals, task.legacy_query(), treatment='bm25',
                            budget=budget, helper=helper, encoding=encoding)
        stages = {'candidate': candidates, 'selected': selection.evidence,
                  'expanded': expanded, 'emitted': tuple(packed.evidence)}
        result = {'arm': arm, 'budget': budget, 'text': packed.text, 'tokens': packed.tokens,
            'empty': not packed.evidence, 'selector_abstained': not selection.evidence,
            'budget_only_empty': bool(selection.evidence) and not packed.evidence and
                bool(reasons[len(selection.rejections):]) and
                all(reason == 'budget' for _, reason in reasons[len(selection.rejections):]),
            'rejections': cast(JSON, [[i, why] for i, why in reasons]),
            'seed_ids': cast(JSON, dict(passage_result.seed_ids)) if passage_result else {},
            'packet_sources': cast(JSON, [asdict(e.source) for e in packed.evidence])}
        for stage, evidence in stages.items():
            result[stage + '_ids'] = [e.id for e in evidence]
            result[stage + '_spans'] = cast(JSON, [asdict(s) for s in _spans(evidence, lexicon)])
            result[stage + '_drawer_count'] = sum(e.source.source_id in lexicon.drawer_bodies for e in evidence)
            result[stage + '_kg_count'] = len(evidence) - integer(result[stage + '_drawer_count'])
        result['expansion_bytes'] = sum(s.end_byte-s.start_byte for s in _spans(packed.evidence, lexicon)) - sum(
            min(e.fact.end_byte, b)-max(e.fact.start_byte, a)
            for e in packed.evidence if e.source.source_id in lexicon.drawer_bodies
            for a, b in merge_intervals([(s.fact.start_byte, s.fact.end_byte)
                for s in selection.evidence if s.source.source_id == e.source.source_id])
            if e.fact.start_byte < b and a < e.fact.end_byte)
        signature = digest(result)
        if previous is not None and signature != previous:
            raise IntegrityError('repeated result differs')
        previous = signature
        if repetition:
            samples.append({'retrieval_ns': retrieved-start, 'selection_ns': selected-retrieved,
                            'packing_ns': finished-selected, 'total_ns': finished-start})
    result['timing_samples_ns'] = cast(JSON, samples)
    return result


def _span_rows(case: Mapping[str, JSON], stage: str) -> tuple[Span, ...]:
    return tuple(Span(string(r['source_id']), integer(r['start_byte']), integer(r['end_byte']),
        string(r['source_body_sha256'])) for r in (obj(v) for v in array(case[stage+'_spans'])))


def _summary(cases: list[dict[str, JSON]]) -> dict[str, JSON]:
    output: dict[str, JSON] = {}
    for arm in ARMS:
        for budget in (128, 256):
            rows = [c for c in cases if c['arm'] == arm and c['budget'] == budget]
            positive = [c for c in rows if c['status'] == 'positive']
            negative = [c for c in rows if c['status'] == 'negative']
            summary: dict[str, JSON] = {status: sum(c['status'] == status for c in rows)
                for status in ('positive', 'negative', 'unavailable', 'ambiguous')}
            summary.update({'negative_abstention': sum(bool(c['empty']) for c in negative)/len(negative) if negative else None,
                'negative_abstention_n': len(negative), 'positive_empty': sum(bool(c['empty']) for c in positive),
                'selector_abstained': sum(bool(c['selector_abstained']) for c in rows),
                'budget_only_empty': sum(bool(c['budget_only_empty']) for c in rows)})
            summary['mean_packet_tokens'] = statistics.mean(integer(c['tokens']) for c in rows)
            summary['support_source_creation'] = {stratum: sum(integer(obj(c['support_source_creation'])[stratum]) for c in positive)
                for stratum in ('before_or_equal', 'after', 'unknown')}
            for stage in ('candidate', 'selected', 'expanded', 'emitted'):
                for metric in ('byte_coverage', 'full_span_fraction', 'complete', 'note_recall'):
                    key = stage+'_'+metric
                    values = [float(cast(float, obj(c['metrics'])[key])) for c in positive]
                    summary[key] = statistics.mean(values) if values else None
                    summary[key+'_n'] = len(values)
            for metric in ('retrieval_ns', 'selection_ns', 'packing_ns', 'total_ns'):
                values_ns = sorted(integer(obj(t)[metric]) for c in rows for t in array(c['timing_samples_ns']))
                summary[metric+'_p50'] = statistics.median(values_ns)
                summary[metric+'_p95'] = values_ns[(len(values_ns)*95+99)//100-1]
                summary[metric+'_n'] = len(values_ns)
            output[f'{arm}-{budget}'] = summary
    for budget in (128, 256):
        fixed, passages = obj(output[f'lexical_fixed-{budget}']), obj(output[f'lexical_passages-{budget}'])
        output[f'paired-{budget}'] = {key: float(cast(float, passages[key]))-float(cast(float, fixed[key]))
            if passages[key] is not None else None for key in ('emitted_byte_coverage', 'emitted_complete', 'negative_abstention')}
    return output


def evaluate(prepared: Path, gold: Path, output: Path, helper_path: Path, cache: Path,
              expected_prepared_sha256: str, expected_gold_sha256: str) -> None:
    """Write exclusively private deterministic results; test_cli_evaluate."""
    if file_digest(prepared) != expected_prepared_sha256 or file_digest(gold) != expected_gold_sha256:
        raise IntegrityError('prepared or gold digest mismatch')
    scratch = Path(os.environ.get('TMPDIR', '')).resolve()
    if not os.environ.get('TMPDIR') or not scratch.is_dir() or scratch.stat().st_mode & 0o077 or any(
            (p / '.git').exists() for p in (scratch, *scratch.parents)):
        raise IntegrityError('explicit mode-0700 private TMPDIR outside Git required')
    tempfile.tempdir = str(scratch)
    encoding = load_encoding(cache)
    data = read_json(prepared)
    judgments = {j.query_id: j for j in validate_gold(read_json(gold), data)}
    sources = tuple(parse_source(s) for s in array(data['sources']))
    task = Task('', string(data['scope']), string(data['as_of']), string(data['as_of']))
    metadata = _metadata(data)
    started = perf_counter_ns()
    lexicon = build_lexicon(sources, metadata, task)
    lexical_done = perf_counter_ns()
    view = derive_passages(sources, metadata, task, encoding)
    passage_done = perf_counter_ns()
    store = DerivedStore({s.key: s for s in sources})
    store.records = {s.key: derive_source(s) for s in sources}
    destination = private_dir(output)
    helper = RustHelper(helper_path)
    cases: list[dict[str, JSON]] = []
    try:
        index = build_scoped_index(sources, task, store, helper)
        try:
            for value in array(data['queries']):
                query = obj(value)
                identity = string(query['id'])
                query_task = Task(string(query['prompt']), task.scope, task.as_of, task.knowledge_cutoff)
                if query.get('palace', task.scope) != task.scope:
                    raise IntegrityError('query palace differs')
                for arm in ARMS:
                    for budget in (128, 256):
                        case = run_case(query_task, arm, index, helper, budget, encoding, sources, lexicon, view)
                        judgment = judgments[identity]
                        metrics: dict[str, JSON] = {}
                        for stage in ('candidate', 'selected', 'expanded', 'emitted'):
                            metrics.update({stage+'_'+k: v for k, v in score_spans(_span_rows(case, stage), judgment.supporting_spans).items()})
                        case.update({'query_id': identity, 'status': judgment.status, 'metrics': metrics})
                        prompt_ms = datetime.fromisoformat(string(query['logged_at'])).timestamp()*1000
                        creation = {stratum: 0 for stratum in ('before_or_equal', 'after', 'unknown')}
                        for sid in {s.source_id for s in judgment.supporting_spans}:
                            record = next(obj(m['record']) for m in metadata.values() if m['source_id'] == sid)
                            stamp = record.get('created_at_ms')
                            creation['unknown' if type(stamp) is not int else 'before_or_equal' if stamp <= prompt_ms else 'after'] += 1
                        case['support_source_creation'] = cast(JSON, creation)
                        cases.append(case)
                recent = cases[-6:]
                if len({digest(c['candidate_ids']) for c in recent}) != 1 or len({digest(c['selected_ids']) for c in recent if c['arm'] != 'raw_bm25'}) != 1:
                    raise IntegrityError('cross-arm candidates or lexical seeds differ')
            write_private(destination/'cases.json', cases)
            write_private(destination/'summary.json', _summary(cases))
            code = {str(Path(filename).resolve()): file_digest(Path(filename)) for m in tuple(sys.modules.values())
                    if isinstance(filename := getattr(m, '__file__', None), str)
                    and '/experiments/trusty-memory-' in filename and filename.endswith('.py')}
            write_private(destination/'provenance.json', {'policy': policy.VERSION, 'code_sha256': code,
                'prepared_sha256': expected_prepared_sha256, 'gold_sha256': expected_gold_sha256,
                'snapshot_sha256': data['snapshot_sha256'], 'sample_sha256': data['sample_sha256'],
                'helper_sha256': file_digest(helper_path), 'encoding_sha256': CONTENT_SHA256,
                'context': list(_context(task)), 'representation': dict(view.representation),
                'index_build_ns': index.build_ns, 'lexicon_build_ns': lexical_done-started,
                'passage_build_ns': passage_done-lexical_done, 'embeddings': False})
        finally:
            index.close()
    finally:
        helper.close()


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('prepared', 'gold', 'output', 'helper', 'cache'):
        parser.add_argument('--'+name, type=Path, required=True)
    for name in ('prepared-sha256', 'gold-sha256'):
        parser.add_argument('--'+name, required=True)
    args = parser.parse_args(argv)
    try:
        evaluate(args.prepared, args.gold, args.output, args.helper, args.cache, args.prepared_sha256, args.gold_sha256)
    except Exception as error:
        print('Experiment failed: ' + type(error).__name__, file=sys.stderr)
        return 1
    print('Private artifacts written: ' + str(args.output))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
