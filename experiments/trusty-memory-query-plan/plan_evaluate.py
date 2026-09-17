"""Offline fixed-arm runner; gold is evaluator-only and every executable input is frozen.

Why: #8246 needs independent required-group recall and repeatable stage evidence.
What: Verify the manifest, reuse eligibility/maintenance, measure one warmup plus three repeats.
Test: test_measured_case_and_manifest_fail_closed.
"""
from __future__ import annotations
import argparse
from dataclasses import asdict
import hashlib
from pathlib import Path
import platform
import resource
from statistics import median
from time import perf_counter_ns
from typing import cast
import tiktoken
import plan_bridge
from plan_bridge import RELEVANCE, GRAPH
from legacy import Source, Query, Gold, JSON, IntegrityError, RustHelper, digest, load_encoding, read_gold
from contracts import Task
from maintenance import DerivedStore, maintain
from relevance_index import RelevanceIndex
from input_data import load_sources, load_queries
from relevance import packet
from metrics import evaluate, aggregate
from evaluate import save, percentile, task_for, index_key
from plan_contracts import Arm, ARMS
from plan_experiment import run_arm
from plan_index import build_scoped_index, validate_context

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent.parent
DOCS = REPO/'docs/research/trusty-memory-query-plan-2026-09-17'
BUDGETS = (128, 256)

def frozen_paths(helper: Path) -> set[Path]:
    return {helper.resolve(), REPO/'crates/trusty-memory/examples/memory_prompt_probe.rs',
        *(p for directory in (ROOT, RELEVANCE, GRAPH) for p in directory.glob('*.py') if not p.name.startswith('test_')),
        *(ROOT/name for name in ('sources.json', 'queries.json', 'gold.json', 'gold-tune.json', 'gold-heldout.json')),
        *(DOCS/name for name in ('interface.md', 'protocol.md', 'research.md'))}

def verify_manifest(helper: Path) -> dict[str, str]:
    """Pre: complete manifest. Post: every code/spec/fixture/helper byte is hash-bound."""
    required = frozen_paths(helper)
    seen: dict[Path, str] = {}
    hashes: dict[str, str] = {}
    for line in (ROOT/'manifest.sha256').read_text().splitlines():
        expected, name = line.split(maxsplit=1)
        name = name.lstrip('*')
        path = (REPO/name).resolve()
        if path in seen or (not path.is_relative_to(REPO) and path != helper.resolve()):
            raise IntegrityError('duplicate or external manifest path')
        actual = hashlib.sha256(path.read_bytes()).hexdigest()
        if actual != expected:
            raise IntegrityError(f'manifest mismatch: {name}')
        seen[path], hashes[name] = actual, actual
    if not required <= seen.keys():
        raise IntegrityError('manifest missing frozen inputs: '+', '.join(str(p) for p in sorted(required-seen.keys())))
    return hashes

def measured_case(query: Query, arm: Arm, index: RelevanceIndex, sources: tuple[Source, ...],
                  gold: Gold, budget: int, helper: RustHelper, encoding: tiktoken.Encoding) -> dict[str, JSON]:
    task = task_for(query)
    validate_context(task, index)
    elapsed: list[int] = []
    timings: dict[str, list[int]] = {}
    previous: str | None = None
    for repetition in range(4):
        started = perf_counter_ns()
        run = run_arm(task, arm, index, helper)
        result, rejections = packet(task, run.selection, index, budget, helper, encoding)
        total = perf_counter_ns()-started
        signature = digest((result.text, [e.id for e in result.evidence], rejections, dict(run.provenance),
            [e.id for e in run.candidates.evidence], asdict(run.plan), asdict(run.execution)))
        if previous is not None and signature != previous:
            raise IntegrityError('repeated retrieval, plan, or packet changed')
        previous = signature
        if repetition:
            elapsed.append(total)
            for key, value in {**run.candidates.timings, **result.timings, 'end_to_end_ns': total}.items():
                timings.setdefault(key, []).append(value)
    metrics = evaluate(result, task, sources, gold, run.candidates.evidence, run.selection.evidence,
        run.selection.supports, budget, helper, encoding, dict(run.provenance), ())
    for name in tuple(metrics):
        if name.startswith(('requested_', 'supported_', 'completed_')):
            del metrics[name]
    metrics.update(query_id=query.id, category=query.category, budget=budget, arm=arm)
    ids = {e.id for e in result.evidence}
    return {'query_id': query.id, 'category': query.category, 'arm': arm, 'budget': budget,
        'text': result.text, 'metrics': metrics, 'timings_ns': cast(dict[str, JSON], timings),
        'latency_samples_ns': cast(list[JSON], elapsed),
        'candidate_ids': [e.id for e in run.candidates.evidence],
        'selected_ids': [e.id for e in run.selection.evidence],
        'evidence': cast(list[JSON], [{'id': e.id, 'source_id': e.source.source_id,
            'source_digest': e.source.fingerprint, 'revision': e.source.revision,
            'start_byte': e.fact.start_byte, 'end_byte': e.fact.end_byte, 'claim': e.fact.claim} for e in result.evidence]),
        'provenance_members': cast(dict[str, JSON], dict(run.provenance)),
        'supports': cast(list[JSON], [asdict(s) for s in run.selection.supports]),
        'plan': cast(dict[str, JSON], asdict(run.plan)), 'execution': cast(dict[str, JSON], asdict(run.execution)),
        'packet_support_losses': sum(not set(s.members) <= ids for s in run.selection.supports),
        'rejections': cast(list[JSON], rejections), 'counters': cast(dict[str, JSON], run.candidates.counters)}

def summary(cases: list[dict[str, JSON]]) -> dict[str, JSON]:
    rows = [cast(dict[str, JSON], c['metrics']) for c in cases]
    result = aggregate(rows)
    samples = [sample for c in cases for sample in cast(list[int], c['latency_samples_ns'])]
    result.update(p50_ns=int(median(samples)) if samples else 0, p95_ns=percentile(samples, .95), timing_samples=len(samples))
    result.update({key: sum(cast(int, row[key]) for row in rows) for key in ('stale', 'scope_errors', 'forbidden')})
    result['packet_support_losses'] = sum(cast(int, case['packet_support_losses']) for case in cases)
    result['adoption_target_met'] = bool(result['adoption_target_met']) and result['stale'] == 0 and result['scope_errors'] == 0
    return result

def run(helper_path: Path, output: Path) -> None:
    frozen = verify_manifest(helper_path)
    output.mkdir(parents=True, exist_ok=False)
    sources, events = load_sources(ROOT)
    queries = load_queries(ROOT)
    encoding, helper = load_encoding(), RustHelper(helper_path)
    indexes: dict[tuple[str, str, str, str], RelevanceIndex] = {}
    try:
        initial = DerivedStore({s.key: s for s in sources})
        maintenance: list[dict[str, JSON]] = []
        while len(initial.records) < sum(not s.deleted for s in initial.sources.values()):
            maintenance.append(maintain(initial, sources, (), 8))
        updated = DerivedStore.restore(initial.checkpoint())
        while updated.sequence < max((e.sequence for e in events), default=0):
            maintenance.append(maintain(updated, sources, events, 3))
        before = updated.checkpoint()
        noop = maintain(updated, sources, events, 3)
        if updated.checkpoint() != before or noop['changed'] or noop['removed']:
            raise IntegrityError('unchanged maintenance altered state')
        if DerivedStore.restore(updated.checkpoint()).derived_hash() != updated.derived_hash():
            raise IntegrityError('checkpoint roundtrip changed derived records')
        rebuilt = DerivedStore(dict(updated.sources))
        while len(rebuilt.records) < sum(not s.deleted for s in rebuilt.sources.values()):
            maintain(rebuilt, tuple(rebuilt.sources.values()), (), 8)
        if rebuilt.derived_hash() != updated.derived_hash():
            raise IntegrityError('incremental and clean derived index differ')
        states = {'initial': initial, 'updated': updated}
        builds = []
        for query in queries:
            key = index_key(query)
            if key not in indexes:
                store = states[query.scenario]
                idx = build_scoped_index(tuple(store.sources.values()), task_for(query), store, helper)
                indexes[key] = idx
                builds.append({'key': key, 'generation': idx.generation, 'build_ns': idx.build_ns,
                    'native_build': idx.native_build, 'missing_sources': len(idx.missing_sources)})
        save(output/'provenance.json', {'manifest': frozen, 'python': platform.python_version(),
            'platform': platform.platform(), 'helper_build_profile': 'debug',
            'helper_sha256': hashlib.sha256(helper_path.read_bytes()).hexdigest(),
            'maintenance': maintenance, 'noop': noop, 'builds': builds, 'helper_startup_ns': helper.startup_ns,
            'embedding_models_constructed': 0, 'budgets': BUDGETS, 'warmups': 1, 'measured_repetitions': 3,
            'policy': {'fact_cap': 4, 'tuning_grid': None, 'arms': ARMS}})
        summaries: dict[str, JSON] = {}
        for split in ('tune', 'heldout'):
            gold = read_gold(ROOT, split, queries)
            results: dict[Arm, list[dict[str, JSON]]] = {arm: [] for arm in ARMS}
            for budget_index, budget in enumerate(BUDGETS):
                for arm in ARMS if budget_index % 2 == 0 else tuple(reversed(ARMS)):
                    for query in queries:
                        if query.split == split:
                            results[arm].append(measured_case(query, arm, indexes[index_key(query)],
                                tuple(states[query.scenario].sources.values()), gold[query.id], budget, helper, encoding))
            repaired = {(c['query_id'], c['budget']): c['candidate_ids'] for c in results['defect_fixes']}
            if any(repaired[c['query_id'], c['budget']] != c['candidate_ids'] for c in results['structured_plan']):
                raise IntegrityError('shared candidates differ between repaired arms')
            split_summary: dict[str, JSON] = {}
            for arm, cases in results.items():
                value = summary(cases)
                budgets = {str(b): summary([c for c in cases if c['budget'] == b]) for b in BUDGETS}
                split_summary[arm] = {'overall': value, 'by_budget': cast(dict[str, JSON], budgets)}
                save(output/f'{split}-{arm}.json.gz', {'summary': value, 'by_budget': budgets, 'cases': cases})
            summaries[split] = split_summary
            print(f'{split}: three arms complete', flush=True)
        save(output/'summary.json', {'splits': summaries, 'complete': True,
            'rss_python_peak_native_units': resource.getrusage(resource.RUSAGE_SELF).ru_maxrss})
    finally:
        helper.close()
        for index in indexes.values():
            index.close()

def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--helper', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    run(args.helper, args.output)

if __name__ == '__main__':
    main()
