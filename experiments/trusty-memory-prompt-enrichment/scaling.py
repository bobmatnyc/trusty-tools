"""Resident latency scaling on explicit synthetic unrelated noise and a substantive hub."""
from __future__ import annotations
from statistics import median
from time import perf_counter_ns
from typing import cast
import numpy as np
from adapters import RustHelper
from records import Fact, Source, Evidence, Query, Policy, JSON, integer
from projection import Projection, graph_lookup


def scaling_probe(helper: RustHelper, scratch: str) -> list[dict[str, JSON]]:
    results: list[dict[str, JSON]] = []
    for count in (100, 1000, 10000):
        triples = [{'id': f'noise-{i}', 'subject': f'noise-subject-{i}', 'predicate': 'related_to',
            'object': f'noise-object-{i}', 'valid_from': '2026-01-01T00:00:00Z', 'valid_to': None} for i in range(count)]
        triples += [{'id': f'hub-{i}', 'subject': 'Hub', 'predicate': 'depends_on', 'object': f'target-{i}',
            'valid_from': '2026-01-01T00:00:00Z', 'valid_to': None} for i in range(512)]
        triples += [{'id': 'small', 'subject': 'Needle', 'predicate': 'depends_on', 'object': 'Endpoint',
            'valid_from': '2026-01-01T00:00:00Z', 'valid_to': None}]
        name = f'scale-{count}'
        built = helper.request({'op': 'load', 'projection': name,
            'documents': cast(list[JSON], [{'id': t['id'], 'text': f'{t["subject"]} {t["predicate"]} {t["object"]}'} for t in triples]),
            'triples': cast(list[JSON], triples), 'scratch_dir': scratch})
        evidence = {}
        adjacency: dict[str, list[str]] = {}
        for row in triples:
            claim = f'{row["subject"]} depends on {row["object"]}.'
            fact = Fact(str(row['id']), str(row['subject']), str(row['predicate']), str(row['object']), str(row['object']),
                claim, 0, len(claim.encode()), '2026-01-01T00:00:00Z', None, None, False)
            source = Source(str(row['id']), 'scale', 1, str(row['id']), claim, '2026-01-01T00:00:00Z', None, None, False, (fact,))
            e = Evidence(source, fact)
            evidence[e.id] = e
            for node in (fact.subject, fact.object):
                adjacency.setdefault(node, []).append(e.id)
        nodes = tuple(sorted(adjacency))
        projection = Projection(name, evidence, {}, {k: tuple(sorted(v)) for k, v in adjacency.items()}, {}, nodes,
            {n.casefold(): (n,) for n in nodes}, (), np.empty((0, 384), dtype=np.float32), (), set(), 0, 0, len(evidence), built)
        for entity in ('Needle', 'Hub'):
            query = Query('scale', 'heldout', 'initial', 'scale', entity, 'scale', '2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z', entity)
            for method in ('query_active', 'expand_neighbors', 'bounded_graph', 'bm25'):
                times = []
                counts = []
                scan = []
                for repetition in range(6):
                    started = perf_counter_ns()
                    if method == 'bounded_graph':
                        found, details = graph_lookup(query, projection, Policy())
                        edges = len(found)
                        examined = integer(details['scanned_edges'])
                        api_ns = perf_counter_ns() - started
                    elif method == 'bm25':
                        value = helper.request({'op': 'search', 'projection': name, 'text': entity, 'limit': 20})
                        edges = len(cast(list[JSON], value['hits']))
                        api_ns = integer(value['_helper_ns'])
                        examined = -1
                    else:
                        value = helper.request({'op': 'graph', 'projection': name, 'method': method, 'entity': entity, 'hops': 1})
                        edges = integer(value['edges'])
                        api_ns = integer(value['api_ns'])
                        examined = -1  # Existing native API does not expose or bound examined edges.
                    if edges == 0:
                        raise RuntimeError('known seeded scale graph returned no evidence')
                    if repetition:
                        times.append((perf_counter_ns() - started, api_ns))
                        counts.append(edges)
                        scan.append(examined)
                results.append({'noise_edges': count, 'total_edges': len(triples), 'entity': entity, 'method': method,
                    'p50_e2e_ns': int(median(t[0] for t in times)), 'p95_e2e_ns': int(np.percentile([t[0] for t in times], 95)),
                    'p50_api_ns': int(median(t[1] for t in times)), 'returned': counts[0], 'scanned': scan[0], 'build': built})
    return results
