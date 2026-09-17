"""Frozen evaluation using the real Rust BM25 engine and explicit portable state."""
from __future__ import annotations
import argparse
import gzip
import hashlib
import json
import math
from pathlib import Path
import resource
import statistics
import subprocess
import time
import platform
import importlib.metadata
from typing import Any
from offline_encoding import load_encoding, CONTENT_SHA256
from metrics import score, summarize
from oracle import SourceOracle

Json = dict[str, Any]
ROOT = Path(__file__).resolve().parent
QUERY_KEYS = ('id', 'text', 'scope', 'mode', 'as_of', 'knowledge_cutoff', 'top_k')
DEFAULT = dict(context_tokens=64, chunk_tokens=256, freshness_weight=.05, kg_weight=.15)
BUDGET = dict(max_documents=16, max_bytes=1048576, max_edges=4096)
ZERO = dict(max_documents=0, max_bytes=0, max_edges=0)
ENCODING = load_encoding()

class EvaluationError(RuntimeError):
    """Engine output violated the frozen contract."""

def canonical(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(',', ':'), allow_nan=False).encode()

def digest(value: Any) -> str:
    return hashlib.sha256(canonical(value)).hexdigest()

def invoke(binary: Path, req: Json) -> tuple[Json, float]:
    start = time.perf_counter_ns()
    proc = subprocess.run([str(binary), '--format', 'json'], input=canonical(req), capture_output=True, timeout=60)
    elapsed = (time.perf_counter_ns() - start) / 1e6
    if proc.returncode:
        raise EvaluationError(f'Engine exit {proc.returncode}: {proc.stdout[:1000]!r} {proc.stderr[:1000]!r}')
    reply: Json = json.loads(proc.stdout)
    if not reply.get('ok') or reply.get('request_id') != req['request_id']:
        raise EvaluationError('Failed response or wrong request identity')
    return reply, elapsed

def request(treatment: str, policy: Json, state: Json | None, *, mutations: list[Json] | None = None,
            queries: list[Json] | None = None, maintenance: bool = False) -> Json:
    return dict(schema_version=1, request_id='frozen-evaluation', treatment=treatment,
                as_of='2026-09-17T00:00:00Z', state=state, policy=policy,
                mutations=mutations or [], maintenance=BUDGET if maintenance else ZERO,
                queries=[{key:q[key] for key in QUERY_KEYS} for q in (queries or [])])

def drain(binary: Path, treatment: str, policy: Json, state: Json | None,
          mutations: list[Json], oracle: SourceOracle) -> tuple[Json, list[Json]]:
    oracle.apply(mutations)
    notes = []
    size = len(mutations) + (len(state['sources']) if state else 0)
    for step in range(4 * (math.ceil(size / BUDGET['max_documents']) + 2)):
        result, elapsed = invoke(binary, request(treatment, policy, state,
            mutations=mutations if step == 0 else [], maintenance=True))
        state = result['state']; stats = result['maintenance']
        oracle.check(state)
        notes.append(dict(stats=stats, elapsed_ms=elapsed))
        if stats['pending'] == 0:
            break
        if treatment == 'raw' and step >= math.ceil(size / BUDGET['max_documents']):
            break
    else:
        raise EvaluationError(f'Maintenance did not converge: {treatment}: {notes[-1]}')
    assert state is not None
    return state, notes

def validate_hits(query: Json, hits: list[Json], sources: dict[tuple[str,str],Json]) -> None:
    if len(hits) > query['top_k']:
        raise EvaluationError('top_k exceeded')
    if len({(h['scope'],h['id']) for h in hits}) != len(hits):
        raise EvaluationError('Duplicate source identity')
    for rank, hit in enumerate(hits, 1):
        if rank != hit['rank'] or not math.isfinite(hit['score']):
            raise EvaluationError('Invalid rank or score')
        source = sources.get((hit['scope'],hit['id']))
        if source is None:
            raise EvaluationError('Deleted or missing source hydrated')
        if hit['revision'] != source['_revision']:
            raise EvaluationError('Wrong source revision')
        body = source['body'].encode()
        if hashlib.sha256(body).hexdigest() != hit['body_digest']:
            raise EvaluationError('Locator does not identify current revision')
        start, end = hit['byte_start'],hit['byte_end']
        if not 0 <= start <= end <= len(body) or hit['excerpt'] != body[start:end].decode():
            raise EvaluationError('Excerpt does not match source byte range')
        line_start = body[:start].count(b'\n') + 1
        line_end = body[:max(start,end-1)].count(b'\n') + 1
        if (hit['line_start'],hit['line_end']) != (line_start,line_end):
            raise EvaluationError('Invalid line locator')

def run_variant(binary: Path, fixture: Json, labels: list[Json], treatment: str, policy: Json) -> Json:
    cpu_start = resource.getrusage(resource.RUSAGE_CHILDREN)
    oracle = SourceOracle()
    state, maintenance = drain(binary,treatment,policy,None,
        [dict(op='upsert',revision=1,drawer=d) for d in fixture['memories']],oracle)
    rows: list[Json] = []; responses: list[Json] = []; latencies: list[float] = []
    card_tokens: list[int] = []; full_tokens: list[int] = []; hit_tokens: list[int] = []; hashes: list[str] = []; sizes: list[int] = []
    for scenario in ('initial','postevents'):
        if scenario == 'postevents':
            events = sorted(fixture['events'],key=lambda e:(e['as_of'],e['id']))
            state, notes = drain(binary,treatment,policy,state,[m for e in events for m in e['mutations']],oracle)
            maintenance.extend(notes)
        sources = oracle.drawers()
        sizes.append(len(canonical(state['snapshot'])))
        queries = [q for q in labels if q['scenario'] == scenario]
        req = request(treatment,policy,state,queries=queries)
        first,_ = invoke(binary,req); second,_ = invoke(binary,req)
        oracle.check(first['state']); oracle.check(second['state'])
        if canonical(first['state']) != canonical(state) or canonical(second['state']) != canonical(state):
            raise EvaluationError('Read-only query mutated portable state')
        if canonical(first) != canonical(second):
            raise EvaluationError('Identical fresh processes differ')
        hashes.append(digest(first))
        by_id = {r['id']:r['hits'] for r in first['results']}
        if set(by_id) != {q['id'] for q in queries}:
            raise EvaluationError('Missing or extra query result')
        for query in queries:
            hits = by_id[query['id']]; validate_hits(query,hits,sources)
            rows.append(score(query,hits))
            cards = [dict(id=h['id'],scope=h['scope'],body_digest=h['body_digest'],
                          lines=[h['line_start'],h['line_end']],created_at=h['created_at'],
                          verified_at=h['verified_at'],status=h['status'],
                          excerpt=h['excerpt']) for h in hits]
            responses.append(dict(query_id=query['id'],hits=hits,cards=cards))
            full = [dict(id=h['id'],scope=h['scope'],body=sources[(h['scope'],h['id'])]['body']) for h in hits]
            card_tokens.append(len(ENCODING.encode(canonical(cards).decode())))
            full_tokens.append(len(ENCODING.encode(canonical(full).decode())))
            hit_tokens.append(len(ENCODING.encode(canonical(hits).decode())))
            for _ in range(3):
                timed,duration = invoke(binary,request(treatment,policy,state,queries=[query]))
                if canonical(timed['results'][0]['hits']) != canonical(hits):
                    raise EvaluationError('Timed query changed payload')
                latencies.append(duration)
        unchanged,_ = invoke(binary,request(treatment,policy,state,maintenance=True))
        oracle.check(unchanged['state'])
        if unchanged['maintenance']['rewritten'] or unchanged['maintenance']['removed']:
            raise EvaluationError('Unchanged maintenance rewrote index')
    cpu_end = resource.getrusage(resource.RUSAGE_CHILDREN)
    summary = summarize(rows)
    summary.update(dict(cli_request_p50_ms=statistics.median(latencies) if latencies else None,
        cli_request_p95_ms=sorted(latencies)[max(0,math.ceil(.95*len(latencies))-1)] if latencies else None,
        card_tokens=sum(card_tokens),full_body_tokens=sum(full_tokens),returned_hit_tokens=sum(hit_tokens),snapshot_bytes=max(sizes),
        child_cpu_seconds=cpu_end.ru_utime+cpu_end.ru_stime-cpu_start.ru_utime-cpu_start.ru_stime))
    return dict(treatment=treatment,policy=policy,summary=summary,rows=rows,responses=responses,
                maintenance=maintenance,semantic_hashes=hashes,final_state=state,
                by_category={c:summarize([r for r in rows if r['category']==c]) for c in sorted({r['category'] for r in rows})},
                by_scenario={c:summarize([r for r in rows if r['scenario']==c]) for c in sorted({r['scenario'] for r in rows})})

def objective(result: Json) -> tuple[float,...]:
    s = result['summary']
    return (-s['scope_errors'],-s['invalid_queries'],-s['evidence_errors'],s['hits_at_5'],
            s['mean_recall_at_5'] or 0.,s['mrr'] or 0.,-s['card_tokens'],-s['snapshot_bytes'])

def write_artifact(path: Path, value: Json) -> None:
    path.write_bytes(gzip.compress(canonical(value),mtime=0))

def verify_freeze() -> dict[str,str]:
    """Reject changed source labels or protocol before running any treatment."""
    repo = ROOT.parent.parent
    hashes = {}
    for line in (ROOT/'manifest.sha256').read_text().splitlines():
        expected, relative = line.split(maxsplit=1)
        actual = hashlib.sha256((repo/relative).read_bytes()).hexdigest()
        if actual != expected:
            raise EvaluationError(f'Frozen input changed: {relative}')
        hashes[relative] = actual
    return hashes

def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    args = parser.parse_args(); binary = args.binary.resolve(strict=True)
    frozen = verify_freeze()
    output = args.output.resolve(); output.mkdir(parents=True,exist_ok=False)
    fixture = json.loads((ROOT/'fixture.json').read_text())
    queries = json.loads((ROOT/'queries.json').read_text())['queries']
    provenance: Json = dict(fixture_sha256=hashlib.sha256((ROOT/'fixture.json').read_bytes()).hexdigest(),
        queries_sha256=hashlib.sha256((ROOT/'queries.json').read_bytes()).hexdigest(),
        binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
        source_revision=subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip())
    repo = ROOT.parent.parent
    paths = sorted((repo/'crates/trusty-memory/examples/support/memory_deterministic').glob('*.rs'))
    paths += [repo/'crates/trusty-memory/examples/memory_deterministic_eval.rs',
              ROOT/'evaluate.py', ROOT/'metrics.py', ROOT/'oracle.py', ROOT/'offline_encoding.py',
              repo/'docs/research/trusty-memory-deterministic-2026-09-17/interface.md']
    provenance['source_hashes'] = {str(p.relative_to(repo)):hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}
    provenance['frozen_hashes'] = frozen
    provenance['runtime'] = dict(python=platform.python_version(),platform=platform.platform(),
                                tiktoken=importlib.metadata.version('tiktoken'),tokenizer='cl100k_base',encoding_sha256=CONTENT_SHA256)
    policies = [dict(DEFAULT),dict(DEFAULT,context_tokens=32),dict(DEFAULT,chunk_tokens=128),
        dict(DEFAULT,context_tokens=32,chunk_tokens=128),dict(DEFAULT,freshness_weight=0.),
        dict(DEFAULT,freshness_weight=.15),dict(DEFAULT,kg_weight=.30)]
    (output/'protocol.json').write_bytes(canonical(dict(provenance=provenance,policies=policies,
        objective='scope safety, temporal validity, excerpt validity, hit@5, recall@5, MRR, card tokens, index bytes')))
    candidates = []
    for i,policy in enumerate(policies):
        result = run_variant(binary,fixture,[q for q in queries if q['split']=='tuning'],'kg',policy)
        candidates.append(result); write_artifact(output/f'tuning-{i}.json.gz',result)
        print('tuning',i,json.dumps(result['summary']),flush=True)
    winner = max(range(len(candidates)),key=lambda i:objective(candidates[i]))
    selection = dict(index=winner,policy=policies[winner],summary=candidates[winner]['summary'],
                     provenance=provenance,grid_sha256=digest(policies))
    (output/'selection.json').write_bytes(canonical(selection))
    summaries = {}
    for treatment in ('raw','repaired_raw','context','chunks','temporal','kg'):
        result = run_variant(binary,fixture,[q for q in queries if q['split']=='heldout'],treatment,selection['policy'])
        summaries[treatment] = result['summary']; write_artifact(output/f'heldout-{treatment}.json.gz',result)
        print('heldout',treatment,json.dumps(result['summary']),flush=True)
    (output/'summary.json').write_bytes(canonical(dict(provenance=provenance,selection=selection,
        treatments=summaries,max_child_rss_native_units=resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)))

if __name__ == '__main__':
    main()
