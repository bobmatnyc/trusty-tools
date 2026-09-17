"""Offline frozen six-arm evaluation; heldout gold opens only after selector choice."""
from __future__ import annotations
import argparse
from dataclasses import asdict
import gzip
import hashlib
import json
from pathlib import Path
import platform
import resource
from statistics import median
from time import perf_counter_ns
from typing import cast
import tiktoken
from legacy import PREVIOUS, Source, Query, Gold, JSON, IntegrityError, RustHelper, digest, load_encoding, read_gold, pack, Retrieval
from contracts import Task, GRID, ARMS, SelectorPolicy
from maintenance import DerivedStore, maintain
from relevance_index import RelevanceIndex, build_index
from input_data import load_sources, load_queries
from relevance import retrieve, intervention, packet
from query_policy import parse_intents
from metrics import evaluate, aggregate, objective

ROOT = Path(__file__).resolve().parent
BUDGETS = (128, 256)


def save(path: Path, value: object) -> None:
    raw = json.dumps(value, sort_keys=True, ensure_ascii=False, allow_nan=False).encode()
    with path.open('xb') as file:
        file.write(gzip.compress(raw, mtime=0) if path.suffix == '.gz' else raw)


def verify_manifest(root: Path) -> dict[str, str]:
    hashes = {}
    for line in (root/'manifest.sha256').read_text().splitlines():
        expected, name = line.split(maxsplit=1)
        actual = hashlib.sha256((root.parent.parent/name.lstrip('*')).read_bytes()).hexdigest()
        if actual != expected:
            raise IntegrityError(f'manifest mismatch: {name}')
        hashes[name] = actual
    policy_path = 'experiments/trusty-memory-relevance/query_policy.py'
    if policy_path not in hashes:
        raise IntegrityError('query policy must be frozen in manifest before ranking')
    return hashes


def task_for(query: Query) -> Task:
    return Task(query.prompt, query.scope, query.as_of, query.knowledge_cutoff)


def index_key(query: Query) -> tuple[str, str, str, str]:
    return query.scenario, query.scope, query.as_of, query.knowledge_cutoff


def measured_case(query: Query, arm: str, policy: SelectorPolicy, index: RelevanceIndex,
                  sources: tuple[Source, ...], gold: Gold, budget: int,
                  helper: RustHelper, encoding: tiktoken.Encoding) -> dict[str, JSON]:
    task = task_for(query)
    elapsed: list[int] = []
    timings: dict[str, list[int]] = {}
    previous = None
    for repetition in range(4):
        started = perf_counter_ns()
        candidates = retrieve(task, arm, index, helper, policy)
        tick = perf_counter_ns()
        selected, provenance = intervention(task, arm, candidates, index, policy)
        selector_ns = perf_counter_ns()-tick
        result, rejections = packet(task, selected, index, budget, helper, encoding)
        total = perf_counter_ns()-started
        signature = digest((result.text, [e.id for e in result.evidence], rejections, provenance))
        if previous is not None and signature != previous:
            raise IntegrityError('repeated packet changed')
        previous = signature
        if repetition:
            elapsed.append(total)
            for key, value in {**candidates.timings, 'selector_ns': selector_ns,
                    **result.timings, 'end_to_end_ns': total}.items():
                timings.setdefault(key, []).append(value)
    metrics = evaluate(result, task, sources, gold, candidates.evidence, selected.evidence,
        selected.supports, budget, helper, encoding, provenance, parse_intents(task, index))
    metrics.update({'query_id': query.id, 'category': query.category, 'budget': budget, 'arm': arm})
    savings = 0
    if arm in {'cleanup', 'combined'}:
        comparison_arm = 'selector' if arm == 'combined' else 'baseline'
        before, _ = intervention(task, comparison_arm, candidates, index, policy)
        before_packet, _ = packet(task, before, index, budget, helper, encoding)
        savings = before_packet.tokens-result.tokens
    return {'query_id': query.id, 'category': query.category, 'arm': arm, 'budget': budget,
        'text': result.text, 'metrics': metrics, 'timings_ns': cast(dict[str, JSON], timings),
        'latency_samples_ns': cast(list[JSON], elapsed), 'duplicate_token_difference': savings,
        'candidate_ids': [e.id for e in candidates.evidence], 'selected_ids': [e.id for e in selected.evidence],
        'evidence': cast(list[JSON], [{'id': e.id, 'source_digest': e.source.fingerprint,
            'revision': e.source.revision, 'start_byte': e.fact.start_byte, 'end_byte': e.fact.end_byte,
            'claim': e.fact.claim} for e in result.evidence]),
        'provenance_members': cast(dict[str, JSON], provenance), 'supports': cast(list[JSON], [asdict(s) for s in selected.supports]),
        'demands': cast(list[JSON], [asdict(d) for d in parse_intents(task, index)]),
        'rejections': cast(list[JSON], rejections), 'counters': cast(dict[str, JSON], candidates.counters)}


def percentile(values: list[int], fraction: float) -> int:
    if not values:
        return 0
    values = sorted(values)
    return values[min(len(values)-1, max(0, int((len(values)-1)*fraction)))]


def summary(cases: list[dict[str, JSON]]) -> dict[str, JSON]:
    result = aggregate([cast(dict[str, JSON], case['metrics']) for case in cases])
    samples = [value for case in cases for value in cast(list[int], case['latency_samples_ns'])]
    result.update({'p50_ns': int(median(samples)) if samples else 0, 'p95_ns': percentile(samples, .95),
        'timing_samples': len(samples), 'duplicate_token_difference': sum(cast(int, case['duplicate_token_difference']) for case in cases)})
    return result


def run(helper_path: Path, output: Path) -> None:
    frozen = verify_manifest(ROOT)
    output.mkdir(parents=True, exist_ok=False)
    sources, events = load_sources(ROOT)
    queries = load_queries(ROOT)
    encoding = load_encoding()
    helper = RustHelper(helper_path)
    indexes: dict[tuple[str, str, str, str], RelevanceIndex] = {}
    try:
        initial = DerivedStore({s.key: s for s in sources})
        maintenance = []
        while len(initial.records) < sum(not s.deleted for s in initial.sources.values()):
            maintenance.append(maintain(initial, sources, (), 8))
        updated = DerivedStore.restore(initial.checkpoint())
        while updated.sequence < max((e.sequence for e in events), default=0):
            maintenance.append(maintain(updated, sources, events, 3))
        before = updated.checkpoint()
        noop = maintain(updated, sources, events, 3)
        if updated.checkpoint() != before or noop['changed'] or noop['removed']:
            raise IntegrityError('unchanged maintenance changed logical state')
        restored = DerivedStore.restore(updated.checkpoint())
        if restored.derived_hash() != updated.derived_hash():
            raise IntegrityError('checkpoint roundtrip changed derived records')
        rebuilt = DerivedStore(dict(updated.sources))
        while len(rebuilt.records) < sum(not s.deleted for s in rebuilt.sources.values()):
            maintain(rebuilt, tuple(rebuilt.sources.values()), (), 8)
        if rebuilt.derived_hash() != updated.derived_hash():
            raise IntegrityError('incremental versus clean derived index differs')
        states = {'initial': initial, 'updated': updated}
        builds = []
        # These are finite clock/scope projections, outside query timing.
        for query in queries:
            key = index_key(query)
            if key not in indexes:
                store = states[query.scenario]
                idx = build_index(tuple(store.sources.values()), task_for(query), store, helper)
                indexes[key] = idx
                builds.append({'key': key, 'generation': idx.generation, 'build_ns': idx.build_ns,
                    'native_build': idx.native_build, 'missing_sources': len(idx.missing_sources)})
        save(output/'provenance.json', {'manifest': frozen, 'python': platform.python_version(), 'platform': platform.platform(),
            'helper_sha256': hashlib.sha256(helper_path.read_bytes()).hexdigest(), 'helper_build_profile': 'debug',
            'helper_source_sha256': hashlib.sha256((ROOT.parent.parent/'crates/trusty-memory/examples/memory_prompt_probe.rs').read_bytes()).hexdigest(),
            'source_hashes': {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in ROOT.glob('*.py')},
            'reused_hashes': {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in PREVIOUS.glob('*.py') if p.name not in {'evaluate.py', 'test_experiment.py'}},
            'maintenance': maintenance, 'noop': noop, 'builds': builds, 'helper_startup_ns': helper.startup_ns,
            'embedding_models_constructed': 0, 'budgets': BUDGETS, 'warmups': 1, 'measured_repetitions': 3})
        tune_gold = read_gold(ROOT, 'tune', queries)
        choices = []
        for i, policy in enumerate(GRID):
            cases = [measured_case(q, 'selector', policy, indexes[index_key(q)], tuple(states[q.scenario].sources.values()),
                tune_gold[q.id], budget, helper, encoding) for budget in BUDGETS for q in queries if q.split == 'tune']
            value = summary(cases)
            save(output/f'tune-{i}.json.gz', {'policy': asdict(policy), 'summary': value, 'cases': cases})
            choices.append((objective(value), i, policy))
            print(f'tune {i}: {value}', flush=True)
        policy = min(choices, key=lambda item: (item[0], item[1]))[2]
        save(output/'selection.json', {'selector': asdict(policy), 'heldout_gold_parsed': False, 'grid': [asdict(p) for p in GRID]})
        gold = read_gold(ROOT, 'heldout', queries)
        results: dict[str, list[dict[str, JSON]]] = {arm: [] for arm in ARMS}
        for budget_index, budget in enumerate(BUDGETS):
            for arm in ARMS if budget_index % 2 == 0 else tuple(reversed(ARMS)):
                for q in queries:
                    if q.split == 'heldout':
                        results[arm].append(measured_case(q, arm, policy, indexes[index_key(q)],
                            tuple(states[q.scenario].sources.values()), gold[q.id], budget, helper, encoding))
                print(f'heldout budget={budget} arm={arm} complete', flush=True)
        summaries = {}
        for arm, cases in results.items():
            summaries[arm] = summary(cases)
            save(output/f'heldout-{arm}.json.gz', {'summary': summaries[arm], 'cases': cases,
                'by_budget': {str(b): summary([c for c in cases if c['budget'] == b]) for b in BUDGETS},
                'by_category': {str(c): summary([v for v in cases if v['category'] == c]) for c in {v['category'] for v in cases}}})
        save(output/'summary.json', {'arms': summaries, 'complete': True,
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
