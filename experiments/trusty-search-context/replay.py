"""Reopen a stopped fixture, evaluate frozen query adapters, and stop our daemon."""
from __future__ import annotations
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import time
from typing import Any
from urllib.error import URLError
import benchmark
from benchmark import request
from routing import adapted_queries
from run_treatment import ROOT, REVISION


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument('--run', required=True)
    parser.add_argument('--name', required=True)
    parser.add_argument('--split', choices=('tuning', 'held_out'), required=True)
    parser.add_argument('--port', type=int, default=18873)
    parser.add_argument('--variants', default='original,normalized,directed')
    parser.add_argument('--query-type', choices=('kg',))
    args = parser.parse_args()
    for value in (args.run, args.name):
        if '/' in value or value in ('.', '..'):
            raise ValueError('Unsafe fixture name')
    source = ROOT / 'runs' / args.run
    lease = (source / 'replay.lock').open('a')
    fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
    for pidfile in [source / 'pid']:
        try:
            os.kill(int(pidfile.read_text()), 0)
        except ProcessLookupError:
            pass
        else:
            raise ValueError('Source daemon PID is still live; refuse concurrent reuse')
    if not (source / 'results.json').is_file() or (source / 'failure.json').exists():
        raise ValueError('Source must be a completed accepted run')
    prior = json.loads((source / 'manifest.json').read_text())
    benchmark.validate_complete(prior['status'])
    dest = ROOT / 'replays' / args.name
    dest.mkdir(parents=True, exist_ok=False)
    data, corpus = dest / 'data', source / 'corpus'
    data.mkdir()
    (data / '.trusty-search-test-daemon').touch()
    base = f'http://127.0.0.1:{args.port}'
    binary = ROOT / 'target/debug/examples/context_experiment_daemon'
    env = dict(os.environ)
    env.update({'TRUSTY_DATA_DIR': str(data), 'TRUSTY_DATA_DIR_OVERRIDE': str(data / 'shared'),
        'TRUSTY_SEARCH_TEST_CORPUS_ROOT': str(corpus), 'TRUSTY_SEARCH_TEST_URL': base,
        'TRUSTY_SEARCH_EXPERIMENT_SOURCE_REVISION': REVISION,
        'TRUSTY_SEARCH_EXPERIMENT_CONTEXT_WORDS': str(prior['words']),
        'TRUSTY_SEARCH_EXPERIMENT_SUBCHUNK_WINDOW': str(prior['window']),
        'TRUSTY_MAX_KG_NODES': str(prior.get('kg_node_cap', 100000)),
        'TRUSTY_NO_AUTO_DISCOVER': '1', 'TRUSTY_MAX_RESIDENT_INDEXES': '1', 'RUST_LOG': 'warn'})
    if hashlib.sha256(binary.read_bytes()).hexdigest() != prior['binary_sha256']:
        raise ValueError('Binary changed since indexing')
    with (dest / 'daemon.log').open('w') as log:
        process = subprocess.Popen([str(binary)], env=env, cwd=corpus, stdout=log, stderr=subprocess.STDOUT)
        (dest / 'pid').write_text(str(process.pid))
        try:
            deadline = time.monotonic() + 90
            while True:
                try:
                    evidence = request(base, '/experiment/evidence')
                    if Path(evidence['data_dir']).resolve() != data.resolve():
                        raise ValueError('Wrong daemon')
                    break
                except (URLError, ConnectionError):
                    if process.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError('Startup failed')
                    time.sleep(.5)
            request(base, '/indexes', {'id': 'experiment', 'root_path': str(corpus), 'skip_vector': True, 'skip_kg': False})
            _, restored = benchmark.verify_fixture(base, 'experiment')
            if restored['chunk_count'] != prior['status']['chunk_count']:
                raise ValueError('Restored corpus count changed')
            graph = request(base, '/indexes/experiment/graph/stats')
            if graph != prior['graph_stats']:
                raise ValueError('Restored graph counts changed')
            queries = json.loads((ROOT / 'queries.json').read_text())
            if args.query_type:
                queries = [q for q in queries if q['type'] == args.query_type]
            originals = {q['id']: q['query'] for q in queries}
            for variant in args.variants.split(','):
                benchmark.request = request
                selected = queries if variant == 'original' else adapted_queries(queries)
                if variant in ('directed', 'directed_seek', 'directed_name'):
                    if variant == 'directed_name':
                        from kg_name_adapter import execute
                    elif variant == 'directed_seek':
                        from kg_seek_adapter import execute
                    else:
                        from kg_adapter import execute
                    def dispatch(base: str, path: str, body: dict[str, Any] | None = None) -> dict[str, Any]:
                        if path == '/indexes/experiment/search' and body is not None and body.get('stage') == 'graph':
                            answer = execute(base, 'experiment', body['text'])
                            if answer is not None:
                                return answer
                        return request(base, path, body)
                    benchmark.request = dispatch
                elif variant not in ('original', 'normalized'):
                    raise ValueError('Unknown variant')
                result = benchmark.evaluate(base, 'experiment', selected, args.split, 3)
                for row in result['rows']:
                    row['request_text'] = row['query']
                    row['query'] = originals[row['id']]
                result.update({'variant': variant, 'source_run': args.run, 'graph_stats': graph,
                    'index_manifest': prior, 'query_sha256': hashlib.sha256((ROOT / 'queries.json').read_bytes()).hexdigest()})
                (dest / f'{variant}.json').write_text(json.dumps(result, indent=2))
                print(args.name, variant, json.dumps(result['summary']['graph']['all']), flush=True)
        finally:
            benchmark.request = request
            if process.poll() is None:
                process.send_signal(signal.SIGTERM)
                try:
                    process.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=10)


if __name__ == '__main__':
    main()
