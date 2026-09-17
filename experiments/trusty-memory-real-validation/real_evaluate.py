"""Prepare private evidence, then evaluate frozen policies against independent judgments.

Why: #8246 needs applicability and relevance measured on retained real prompts.
What: Three unchanged treatments; all content-bearing output stays outside Git.
Test: test_prepare_and_helper, test_gold_statuses_and_span_coverage.
"""
from __future__ import annotations
import argparse
import json
from dataclasses import asdict
from pathlib import Path
import statistics
from time import perf_counter_ns
from typing import Literal, cast
import tiktoken
from real_adapter import (prepare, read_json, file_digest, private_dir, write_private)
from legacy import (JSON, Source, RustHelper, IntegrityError, obj, array, string, integer,
    parse_source, digest, load_encoding, validate_packet)
from contracts import Task, CandidateSet, Selection
from maintenance import DerivedStore, derive_source
from plan_index import ScopedIndex, build_scoped_index, validate_context
from plan_experiment import run_arm
from relevance import packet

Arm = Literal['raw_bm25', 'old_combined', 'structured_plan']
ARMS: tuple[Arm, ...] = ('raw_bm25', 'old_combined', 'structured_plan')

def run_case(task: Task, arm: Arm, index: ScopedIndex, helper: RustHelper,
             budget: int, encoding: tiktoken.Encoding, sources: tuple[Source, ...]) -> dict[str, JSON]:
    validate_context(task, index)
    if arm not in ARMS or budget not in (128, 256):
        raise IntegrityError('unknown arm or unsupported budget')
    timings: list[int] = []
    previous: str | None = None
    result: dict[str, JSON] = {}
    for repetition in range(4):
        started = perf_counter_ns()
        plan: JSON = {}
        execution: JSON = {}
        if arm == 'raw_bm25':
            response = helper.request({'op':'search', 'projection':index.claim_id, 'text':task.prompt, 'limit':20})
            candidates = CandidateSet(tuple(index.evidence[string(obj(hit)['id'])] for hit in array(response['hits'])))
            selection = Selection(candidates.evidence)
        else:
            run = run_arm(task, arm, index, helper)
            candidates, selection = run.candidates, run.selection
            plan = obj(json.loads(json.dumps(asdict(run.plan))))
            execution = obj(json.loads(json.dumps(asdict(run.execution))))
        packed, rejections = packet(task, selection, index, budget, helper, encoding)
        elapsed = perf_counter_ns() - started
        validate_packet(packed, sources, task.legacy_query(), treatment='bm25', budget=budget,
            helper=helper, encoding=encoding)
        result = {'arm':arm, 'budget':budget, 'text':packed.text,
            'candidate_ids':[item.id for item in candidates.evidence],
            'selected_ids':[item.id for item in selection.evidence],
            'evidence_ids':[item.id for item in packed.evidence],
            'tokens':len(encoding.encode(packed.text, disallowed_special=())),
            'plan':plan, 'execution':execution, 'counters':cast(JSON, candidates.counters),
            'rejections':cast(JSON, rejections)}
        signature = digest(result)
        if previous is not None and previous != signature:
            raise IntegrityError('repeated result differs')
        previous = signature
        if repetition:
            timings.append(elapsed)
    result['latency_samples_ns'] = cast(JSON, timings)
    return result

def validate_gold(data: dict[str, JSON], prepared: dict[str, JSON]) -> dict[str, dict[str, JSON]]:
    queries = {string(obj(row)['id']) for row in array(prepared['queries'])}
    sources = {source.source_id:source for source in (parse_source(row) for row in array(prepared['sources']))}
    metadata = obj(prepared['metadata'])
    judgments: dict[str, dict[str, JSON]] = {}
    for value in array(data['judgments']):
        row = obj(value)
        query_id = string(row['query_id'])
        status = row['status']
        if query_id in judgments or status not in ('positive', 'negative', 'unavailable', 'ambiguous'):
            raise IntegrityError('duplicate judgment or invalid status')
        spans = [obj(span) for span in array(row['supporting_spans'])]
        for span in spans:
            sid, start, end = string(span['source_id']), integer(span['start_byte']), integer(span['end_byte'])
            if sid not in sources or not 0 <= start < end <= len(sources[sid].body.encode()):
                raise IntegrityError('invalid supporting span')
            sources[sid].body.encode()[start:end].decode('utf-8')
            if not any(obj(m)['source_id'] == sid and obj(m)['kind'] == 'drawer_text' for m in metadata.values()):
                raise IntegrityError('supporting span is not drawer evidence')
        accepted = [string(identity) for identity in array(row['acceptable'])]
        forbidden = [string(identity) for identity in array(row['forbidden'])]
        groups = [tuple(string(identity) for identity in array(group)) for group in array(row.get('required', []))]
        if any(not group or any(identity not in metadata for identity in group) for group in groups):
            raise IntegrityError('invalid required evidence group')
        required = {identity for group in groups for identity in group}
        if status == 'positive' and not groups:
            raise IntegrityError('positive judgment needs required evidence groups')
        if status != 'positive' and groups:
            raise IntegrityError('nonpositive judgment cannot contain required evidence groups')
        if any(identity not in metadata for identity in accepted + forbidden):
            raise IntegrityError('gold names unknown evidence')
        if required & set(forbidden) or set(accepted) & set(forbidden):
            raise IntegrityError('gold relevance and forbidden evidence overlap')
        for identity in required | set(accepted):
            record = obj(metadata[identity])
            if record['kind'] == 'drawer_text' and not any(span['source_id'] == record['source_id']
                and integer(span['start_byte']) < integer(record['end_byte'])
                and integer(span['end_byte']) > integer(record['start_byte']) for span in spans):
                raise IntegrityError('relevant drawer evidence requires a supporting span')
        corroboration = [array(pair) for pair in array(row['corroboration'])]
        for pair in corroboration:
            if len(pair) != 2 or any(string(identity) not in metadata for identity in pair):
                raise IntegrityError('invalid corroboration IDs')
            if obj(metadata[string(pair[0])])['kind'] != 'kg_record' or obj(metadata[string(pair[1])])['kind'] != 'drawer_text':
                raise IntegrityError('corroboration must connect KG to drawer')
        corroborated = {string(pair[0]) for pair in corroboration}
        if any(obj(metadata[identity])['kind'] == 'kg_record' and identity not in corroborated for identity in required | set(accepted)):
            raise IntegrityError('accepted graph claim lacks corroboration')
        if status == 'negative' and (spans or accepted):
            raise IntegrityError('gold status contradicts evidence')
        row['required'] = [list(group) for group in groups]
        judgments[query_id] = row
    if set(judgments) != queries:
        raise IntegrityError('gold/sample IDs differ')
    return judgments

def merge_intervals(intervals: list[tuple[int, int]]) -> list[tuple[int, int]]:
    """Union byte ranges without double-counting overlapping supporting quotes."""
    merged: list[tuple[int, int]] = []
    for start, end in sorted(intervals):
        if end <= start:
            continue
        if merged and start <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(end, merged[-1][1]))
        else:
            merged.append((start, end))
    return merged

def score_case(case: dict[str, JSON], gold: dict[str, JSON], metadata: dict[str, JSON]) -> dict[str, JSON]:
    spans = [obj(span) for span in array(gold['supporting_spans'])]
    groups = [{string(identity) for identity in array(group)} for group in array(gold['required'])]
    accepted = {string(identity) for identity in array(gold['acceptable'])} | set().union(*groups)
    corroborated = {string(array(pair)[0]) for pair in array(gold['corroboration'])}
    support_by_source: dict[str, list[tuple[int, int]]] = {}
    for span in spans:
        support_by_source.setdefault(string(span['source_id']), []).append((integer(span['start_byte']), integer(span['end_byte'])))
    support_by_source = {sid:merge_intervals(intervals) for sid, intervals in support_by_source.items()}
    supporting_bytes = sum(end - start for intervals in support_by_source.values() for start, end in intervals)
    emitted = [string(identity) for identity in array(case['evidence_ids'])]
    result: dict[str, JSON] = {'status':gold['status'], 'empty':not emitted,
        'emitted':len(emitted), 'forbidden':len(set(emitted) & {string(i) for i in array(gold['forbidden'])})}
    for lane in ('candidate', 'selected', 'evidence'):
        ids = [string(identity) for identity in array(case[lane + '_ids'])]
        records = [obj(metadata[identity]) for identity in ids]
        for kind in ('drawer_text', 'kg_record'):
            result[lane + '_' + kind] = sum(record['kind'] == kind for record in records)
        covered = 0
        relevant = set(accepted)
        intersections: dict[str, list[tuple[int, int]]] = {}
        for span in spans:
            sid, start, end = string(span['source_id']), integer(span['start_byte']), integer(span['end_byte'])
            intervals = sorted((max(start, integer(m['start_byte'])), min(end, integer(m['end_byte'])))
                for m in records if m['kind'] == 'drawer_text' and m['source_id'] == sid
                and integer(m['start_byte']) < end and integer(m['end_byte']) > start)
            intersections.setdefault(sid, []).extend(intervals)
            cursor = start
            for left, right in intervals:
                if left > cursor:
                    break
                cursor = max(cursor, right)
            covered += cursor >= end
            relevant.update(identity for identity in ids if obj(metadata[identity])['kind'] == 'drawer_text'
                and obj(metadata[identity])['source_id'] == sid
                and integer(obj(metadata[identity])['start_byte']) < end
                and integer(obj(metadata[identity])['end_byte']) > start)
        notes = {string(span['source_id']) for span in spans}
        returned_notes = {string(m['source_id']) for m in records if m['kind'] == 'drawer_text'}
        result[lane + '_span_coverage'] = covered / len(spans) if spans else None
        covered_bytes = sum(end - start for intervals in intersections.values() for start, end in merge_intervals(intervals))
        result[lane + '_byte_coverage'] = covered_bytes / supporting_bytes if supporting_bytes else None
        result[lane + '_note_recall'] = len(notes & returned_notes) / len(notes) if notes else None
        matched = sum(bool(group & set(ids)) for group in groups)
        result[lane + '_group_coverage'] = matched / len(groups) if groups else None
        result[lane + '_complete'] = matched == len(groups) if groups else None
        if lane == 'evidence':
            drawer_ids = {identity for identity in ids if obj(metadata[identity])['kind'] == 'drawer_text'}
            graph_ids = set(ids) - drawer_ids
            judged_graph = graph_ids & corroborated
            result['drawer_relevant_emitted'] = len(drawer_ids & relevant)
            result['drawer_relevant_chunk_fraction'] = len(drawer_ids & relevant) / len(drawer_ids) if drawer_ids else None
            result['kg_judged_emitted'] = len(judged_graph)
            result['kg_relevant_emitted'] = len(judged_graph & relevant)
            result['kg_unresolved_emitted'] = len(graph_ids - corroborated)
            result['kg_judged_precision'] = len(judged_graph & relevant) / len(judged_graph) if judged_graph else None
    return result

def summary(cases: list[dict[str, JSON]]) -> dict[str, JSON]:
    """Aggregate only applicable judgments; excluded labels never count as abstention."""
    result: dict[str, JSON] = {}
    for arm in ARMS:
        for budget in (128, 256):
            selected = [case for case in cases if case['arm'] == arm and case['budget'] == budget]
            if not selected:
                continue
            metrics = [obj(case['metrics']) for case in selected]
            positives = [row for row in metrics if row['status'] == 'positive']
            negatives = [row for row in metrics if row['status'] == 'negative']
            output: dict[str, JSON] = {'cases':len(selected), 'positive':len(positives), 'negative':len(negatives),
                'unavailable':sum(row['status'] == 'unavailable' for row in metrics),
                'ambiguous':sum(row['status'] == 'ambiguous' for row in metrics),
                'negative_abstention':sum(row['empty'] is True for row in negatives) / len(negatives) if negatives else None,
                'positive_raw_emitted':sum(integer(row['emitted']) for row in positives),
                'mean_tokens':statistics.mean(integer(case['tokens']) for case in selected),
                'forbidden_emitted':sum(integer(row['forbidden']) for row in metrics)}
            for key in ('candidate_span_coverage', 'selected_span_coverage', 'evidence_span_coverage',
                        'candidate_byte_coverage', 'selected_byte_coverage', 'evidence_byte_coverage',
                        'candidate_group_coverage', 'selected_group_coverage', 'evidence_group_coverage',
                        'candidate_complete', 'selected_complete', 'evidence_complete',
                        'candidate_note_recall', 'selected_note_recall', 'evidence_note_recall',
                        'drawer_relevant_chunk_fraction', 'kg_judged_precision'):
                values = [float(cast(float, row[key])) for row in positives if row[key] is not None]
                output[key] = statistics.mean(values) if values else None
                output[key + '_n'] = len(values)
            for key in ('drawer_relevant_emitted', 'kg_judged_emitted', 'kg_relevant_emitted', 'kg_unresolved_emitted'):
                output['positive_' + key] = sum(integer(row[key]) for row in positives)
            statuses: dict[str, int] = {}
            entity_statuses: dict[str, int] = {}
            ready_queries = bounded_queries = 0
            for case in selected:
                requests = [obj(value) for value in array(obj(case['plan']).get('requests', []))]
                ready_queries += any(request['status'] == 'ready' for request in requests)
                bounded_queries += 'bounded' in array(obj(case['execution']).get('per_request', []))
                for request in requests:
                    status = string(request['status'])
                    statuses[status] = statuses.get(status, 0) + 1
                    for value in array(request['roots']):
                        entity_status = string(obj(value)['status'])
                        entity_statuses[entity_status] = entity_statuses.get(entity_status, 0) + 1
            output['parse_status_counts'] = cast(JSON, statuses)
            output['entity_status_counts'] = cast(JSON, entity_statuses)
            output['ready_queries'] = ready_queries if arm == 'structured_plan' else None
            output['bounded_queries'] = bounded_queries if arm == 'structured_plan' else None
            for lane in ('candidate', 'selected', 'evidence'):
                for kind in ('drawer_text', 'kg_record'):
                    key = lane + '_' + kind
                    output[key] = sum(integer(row[key]) for row in metrics)
            latency = sorted(integer(value) for case in selected for value in array(case['latency_samples_ns']))
            output['latency_n'] = len(latency)
            output['p50_ns'] = statistics.median(latency)
            output['p95_ns'] = latency[(len(latency) * 95 + 99) // 100 - 1]
            result[f'{arm}-{budget}'] = output
    return result

def evaluate(prepared_path: Path, gold_path: Path, prepared_digest: str, gold_digest: str,
             helper_path: Path, output: Path, encoding: tiktoken.Encoding) -> None:
    if file_digest(prepared_path) != prepared_digest or file_digest(gold_path) != gold_digest:
        raise IntegrityError('prepared corpus or gold digest mismatch')
    prepared = read_json(prepared_path)
    judgments = validate_gold(read_json(gold_path), prepared)
    sources = tuple(parse_source(row) for row in array(prepared['sources']))
    task = Task('', string(prepared['scope']), string(prepared['as_of']), string(prepared['as_of']))
    store = DerivedStore({source.key:source for source in sources})
    store.records = {source.key:derive_source(source) for source in sources}
    destination = private_dir(output)
    helper = RustHelper(helper_path)
    cases: list[dict[str, JSON]] = []
    try:
        index = build_scoped_index(sources, task, store, helper)
        try:
            for value in array(prepared['queries']):
                query = obj(value)
                query_task = Task(string(query['prompt']), task.scope, task.as_of, task.knowledge_cutoff)
                for arm in ARMS:
                    for budget in (128, 256):
                        case = run_case(query_task, arm, index, helper, budget, encoding, sources)
                        case['query_id'] = query['id']
                        case['metrics'] = score_case(case, judgments[string(query['id'])], obj(prepared['metadata']))
                        cases.append(case)
            write_private(destination / 'cases.json', cases)
            write_private(destination / 'summary.json', summary(cases))
            write_private(destination / 'provenance.json', {'prepared_sha256':prepared_digest, 'gold_sha256':gold_digest,
                'helper_sha256':file_digest(helper_path), 'counts':prepared['counts'],
                'drawer_decode_versions':prepared.get('drawer_decode_versions', {}),
                'index_build_ns':index.build_ns, 'embeddings':False})
        finally:
            index.close()
    finally:
        helper.close()

def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('prepare', 'evaluate'))
    for name in ('snapshot', 'prompts', 'prepared', 'gold', 'helper'):
        parser.add_argument('--' + name, type=Path)
    for name in ('sample-sha256', 'prepared-sha256', 'gold-sha256', 'scope'):
        parser.add_argument('--' + name)
    parser.add_argument('--private-output', type=Path, required=True)
    parser.add_argument('--max-rows', type=int, default=100000)
    args = parser.parse_args()
    encoding = load_encoding()
    if args.mode == 'prepare':
        if not all((args.snapshot, args.prompts, args.sample_sha256, args.scope)):
            parser.error('prepare requires snapshot, prompts, sample-sha256 and scope')
        prepare(args.snapshot, args.prompts, args.sample_sha256, args.private_output, args.scope, encoding, args.max_rows)
    else:
        if not all((args.prepared, args.gold, args.prepared_sha256, args.gold_sha256, args.helper)):
            parser.error('evaluate requires prepared, gold, both hashes and helper')
        evaluate(args.prepared, args.gold, args.prepared_sha256, args.gold_sha256, args.helper, args.private_output, encoding)
    print('Private artifacts written successfully.')

if __name__ == '__main__':
    main()
