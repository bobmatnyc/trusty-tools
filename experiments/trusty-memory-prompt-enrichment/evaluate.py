"""Frozen offline experiment: select policy on tune, then compare heldout packets."""
from __future__ import annotations
import argparse
from dataclasses import asdict, replace
import gzip
import hashlib
import json
from pathlib import Path
import platform
import resource
import tempfile
from time import perf_counter_ns
from typing import cast
import numpy as np
import onnxruntime as ort  # type: ignore[import-untyped]
import tokenizers
import tiktoken
from adapters import LocalEncoder, RustHelper, MODEL_HASH, TOKENIZER_HASH
from offline_encoding import load_encoding, CONTENT_SHA256
from records import JSON, Source, Query, Policy, IntegrityError, digest, load_inputs, read_gold
from projection import SourceIndex, Projection, build_source_index, build_projection, update_projection
from retrieval import TREATMENTS, Retrieval, retrieve, pack
from scoring import evaluate, aggregate, objective

ROOT = Path(__file__).resolve().parent
BUDGETS = (128, 256, 512)


def save(path: Path, value: object) -> None:
    payload = json.dumps(value, sort_keys=True, indent=2, ensure_ascii=False, allow_nan=False).encode()
    if path.suffix == '.gz':
        with path.open('xb') as file:
            file.write(gzip.compress(payload, mtime=0))
    else:
        with path.open('xb') as file:
            file.write(payload)


def verify_manifest(root: Path) -> dict[str, str]:
    repo = root.parent.parent
    hashes = {}
    for line in (root / 'manifest.sha256').read_text().splitlines():
        expected, name = line.split(maxsplit=1)
        path = repo / name.lstrip('*')
        actual = hashlib.sha256(path.read_bytes()).hexdigest()
        if actual != expected:
            raise IntegrityError(f'frozen manifest mismatch: {name}')
        hashes[name] = actual
    return hashes


def percentile(values: list[int], q: float) -> int:
    return int(np.percentile(values, q)) if values else 0


def compare(queries: tuple[Query, ...], treatment: str, policy: Policy,
            projections: dict[str, Projection], states: dict[str, SourceIndex],
            model: LocalEncoder, helper: RustHelper, encoding: tiktoken.Encoding,
            gold: dict[str, object], repetitions: int) -> dict[str, object]:
    from records import Gold
    rows: list[dict[str, JSON]] = []
    packets: list[dict[str, object]] = []
    elapsed: list[int] = []
    stage_samples: dict[str, list[int]] = {}
    cache_started = perf_counter_ns()
    cache = {(p.key, budget): pack(queries[0], Retrieval(list(p.standing), {}, {}), p, budget, helper, encoding, treatment, prelude=False)
        for p in projections.values() for budget in BUDGETS} if treatment == 'standing_cache' else {}
    cache_build_ns = perf_counter_ns() - cache_started if cache else 0
    for query in queries:
        projection = projections[query.projection]
        for budget in BUDGETS:
            semantic = None
            packet = None
            details: dict[str, JSON] = {}
            # Warmup participates in determinism verification but not measured percentiles.
            for repetition in range(repetitions + 1):
                tick = perf_counter_ns()
                if treatment == 'standing_cache':
                    packet = cache[(projection.key, budget)]
                    retrieval = Retrieval(packet.evidence, {}, {})
                    packet.timings = {'standing_cache_lookup_ns': perf_counter_ns() - tick}
                else:
                    retrieval = retrieve(query, treatment, projection, model, helper, policy)
                    packet = pack(query, retrieval, projection, budget, helper, encoding, treatment)
                total = perf_counter_ns() - tick
                signature = digest((packet.text, [e.id for e in packet.evidence], packet.dropped_ids))
                if semantic is not None and signature != semantic:
                    raise IntegrityError('same-state repeated packet changed')
                semantic = signature
                details = retrieval.details
                if repetition:
                    elapsed.append(total)
                    for key, value in {**packet.timings, 'end_to_end_ns': total}.items():
                        stage_samples.setdefault(key, []).append(value)
            if packet is None:
                raise IntegrityError('packet was not evaluated')
            metrics = evaluate(packet, cast(Gold, gold[query.id]), tuple(states[query.scenario].sources.values()), query, treatment=treatment, budget=budget, helper=helper, encoding=encoding)
            metrics.update({'budget': budget, 'treatment': treatment})
            rows.append(metrics)
            packets.append({'query_id': query.id, 'budget': budget, 'text': packet.text,
                'evidence': [{'id': e.id, 'source_digest': e.source.fingerprint, 'revision': e.source.revision,
                    'start_byte': e.fact.start_byte, 'end_byte': e.fact.end_byte, 'triple': e.triple,
                    'rendered': packet.renderings[e.id]} for e in packet.evidence],
                'dropped_ids': packet.dropped_ids, 'details': details, 'metrics': metrics})
    categories = sorted({q.category for q in queries})
    return {'treatment': treatment, 'policy': asdict(policy), 'summary': aggregate(rows),
        'by_category': {c: aggregate([r for r in rows if r['category'] == c]) for c in categories},
        'by_budget': {str(b): aggregate([r for r in rows if r['budget'] == b]) for b in BUDGETS},
        'latency': {'p50_ns': percentile(elapsed, 50), 'p95_ns': percentile(elapsed, 95),
            'samples': len(elapsed), 'stages': {k: {'p50_ns': percentile(v, 50), 'p95_ns': percentile(v, 95)} for k, v in stage_samples.items()}},
        'standing_cache_build_ns': cache_build_ns, 'packets': packets}


def run(helper_path: Path, model_dir: Path, output: Path) -> None:
    hashes = verify_manifest(ROOT)
    output.mkdir(parents=True, exist_ok=False)
    sources, events, queries = load_inputs(ROOT)
    encoding = load_encoding()
    model = LocalEncoder(model_dir)
    helper = RustHelper(helper_path)
    try:
        with tempfile.TemporaryDirectory(prefix='memory-prompt-enrichment-') as scratch:
            tick = perf_counter_ns()
            initial = build_source_index(sources, model)
            corpus_ns = perf_counter_ns() - tick
            updated = SourceIndex(dict(initial.sources), dict(initial.vectors))
            maintenance = []
            while updated.sequence < max((e.sequence for e in events), default=0):
                maintenance.append(update_projection(updated, events, 3, model))
            unchanged = update_projection(updated, events, 3, model)
            if unchanged['changed'] or unchanged['removed']:
                raise IntegrityError('unchanged maintenance rewrote sources')
            rebuilt = build_source_index(tuple(updated.sources.values()), model)
            if set(rebuilt.vectors) != set(updated.vectors) or any(not np.allclose(rebuilt.vectors[k], updated.vectors[k], atol=1e-6, rtol=0) for k in rebuilt.vectors):
                raise IntegrityError('incremental vectors differ from clean rebuild')
            states = {'initial': initial, 'updated': updated}
            projections = {}
            for query in queries:
                if query.projection not in projections:
                    projections[query.projection] = build_projection(states[query.scenario], query, helper, scratch)
            build = {key: {'build_ns': p.build_ns, 'native': p.native, 'eligible_facts': len(p.evidence),
                'graph_facts': p.graph_facts, 'structural_omitted': p.structural_omitted,
                'vector_bytes': p.vectors.nbytes} for key, p in projections.items()}
            provenance = {'manifest': hashes, 'model_hash': MODEL_HASH, 'model_tokenizer_hash': TOKENIZER_HASH,
                'prompt_tokenizer_hash': CONTENT_SHA256, 'helper_hash': hashlib.sha256(helper_path.read_bytes()).hexdigest(),
                'python': platform.python_version(), 'platform': platform.platform(), 'numpy': np.__version__,
                'onnxruntime': ort.__version__, 'tokenizers': tokenizers.__version__, 'tiktoken': tiktoken.__version__,
                'source_hashes': {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(ROOT.glob('*.py'))},
                'helper_startup_ns': helper.startup_ns, 'model_load_ns': model.load_ns, 'corpus_encoding_ns': corpus_ns,
                'truncation_events': model.truncation_events, 'projections': build,
                'maintenance': maintenance, 'unchanged': unchanged, 'incremental_rebuild_equal_atol': 1e-6,
                'limitations': ['Finite clock projections rebuilt outside queries; not incremental native publication.',
                    'Scope/time filtering belongs to adapter; native active APIs have no as-of/TTL contract.',
                    'Native formatted controls verify exact bullets; matched treatments verify full claims.',
                    'Source body presentation removes ineligible fact spans.',
                    'Graph entity hops exclude literal-property edges, but all examined facts consume scan budget.']}
            save(output / 'provenance.json', provenance)
            tune = tuple(q for q in queries if q.split == 'tune')
            tune_gold = cast(dict[str, object], read_gold(ROOT, 'tune', queries))
            choices: dict[str, Policy] = {}
            for lane, policies in [('graph', [Policy(max_seeds=s, max_hops=h) for s, h in [(1, 1), (3, 1), (1, 2), (3, 2)]]),
                ('dense', [Policy(minimum_cosine=c) for c in [0.25, 0.40, 0.55]])]:
                scored = []
                for i, policy in enumerate(policies):
                    result = compare(tune, 'bm25_' + lane, policy, projections, states, model, helper, encoding, tune_gold, 1)
                    save(output / f'tune-{lane}-{i}.json.gz', result)
                    scored.append((objective(cast(dict[str, JSON], result['summary'])), i, policy))
                    print(f'tune {lane} {i}: {result["summary"]}', flush=True)
                choices[lane] = min(scored, key=lambda entry: (entry[0], entry[1]))[2]
            selected = replace(choices['graph'], minimum_cosine=choices['dense'].minimum_cosine)
            save(output / 'selection.json', {'policy': asdict(selected), 'selected_before_heldout_gold': True})
            heldout_gold = cast(dict[str, object], read_gold(ROOT, 'heldout', queries))
            heldout = tuple(q for q in queries if q.split == 'heldout')
            summaries = {}
            for i, treatment in enumerate(TREATMENTS):
                rotated = heldout[i:] + heldout[:i]
                result = compare(rotated, treatment, selected, projections, states, model, helper, encoding, heldout_gold, 5)
                save(output / f'heldout-{treatment}.json.gz', result)
                summaries[treatment] = {k: v for k, v in result.items() if k != 'packets'}
                print(f'heldout {treatment}: {result["summary"]}', flush=True)
            from scaling import scaling_probe
            save(output / 'scaling.json', scaling_probe(helper, scratch))
            save(output / 'summary.json', {'treatments': summaries,
                'max_rss_native_units': resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
                'rss_units': 'bytes on macOS; KiB on Linux', 'complete': True})
    finally:
        helper.close()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--helper', type=Path, required=True)
    parser.add_argument('--model-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    run(args.helper, args.model_dir, args.output)

if __name__ == '__main__':
    main()
