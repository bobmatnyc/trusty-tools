"""Bounded directed predicate plans return complete provenance-bearing support groups."""
from __future__ import annotations
from collections import deque
from time import perf_counter_ns
from contracts import Task, Demand, Support, CandidateSet
from relevance_index import RelevanceIndex
from query_policy import PREDICATES
from legacy import Evidence


def relation_lookup(task: Task, demands: tuple[Demand, ...], index: RelevanceIndex,
                    allowed: set[str] | None = None) -> CandidateSet:
    """Optional allowed IDs constrain selector-only validation to existing candidates."""
    started = perf_counter_ns()
    scanned = probes = 0
    visited: set[str] = set()
    emitted: dict[str, Evidence] = {}
    supports: list[Support] = []
    rejections: list[tuple[str, str]] = []
    truncated: set[str] = set()

    def posting(entity: str, predicate: str, direction: str) -> tuple[Evidence, ...]:
        nonlocal probes, scanned
        probes += 1
        result = []
        for e in index.postings.get((entity, predicate, direction), ()):
            if allowed is not None and e.id not in allowed:
                continue
            if scanned >= 128:
                truncated.add('scan_limit')
                break
            scanned += 1
            result.append(e)
        return tuple(result)

    def publish(path: tuple[str, ...], endpoint: Evidence, demand_index: int, entity: str) -> None:
        members = tuple(dict.fromkeys((*path, endpoint.id)))
        if len(set(emitted) | set(members)) > 32:
            truncated.add('emitted_limit')
            return
        if any(s.members == members and s.demand_index == demand_index and s.entity == entity for s in supports):
            return
        supports.append(Support(endpoint.id, members, demand_index, entity))
        for identity in members:
            emitted[identity] = index.evidence[identity]

    seeds: set[str] = set()
    for demand_index, demand in enumerate(demands):
        if demand.status != 'ready':
            rejections.append((str(demand_index), demand.status))
            continue
        if demand.intent == 'alias':
            for identity in demand.aliases:
                if allowed is None or identity in allowed:
                    if scanned >= 128:
                        truncated.add('scan_limit')
                        break
                    scanned += 1
                    publish((), index.evidence[identity], demand_index,
                        (index.evidence[identity].fact.object_entity or '') if demand.entities else '')
            continue
        for seed in demand.entities:
            if seed not in seeds and len(seeds) >= 3:
                truncated.add('seed_limit')
                continue
            if seed not in visited and len(visited) >= 32:
                truncated.add('visited_limit')
                continue
            seeds.add(seed)
            visited.add(seed)
            alias_path = tuple(identity for identity in demand.aliases if
                index.evidence[identity].fact.object_entity == seed and
                len(index.aliases.get(index.evidence[identity].fact.subject, ())) == 1)
            if allowed is not None and not set(alias_path) <= allowed:
                rejections.append((seed, 'no_complete_path'))
                continue
            queue = deque([(seed, alias_path, 0, '')])
            seen_paths: set[tuple[str, tuple[str, ...]]] = set()
            while queue:
                entity, path, depth, first_relation = queue.popleft()
                if (entity, path) in seen_paths:
                    continue
                seen_paths.add((entity, path))
                direction = 'in' if demand.intent == 'reverse_dependency' else 'out'
                for predicate in PREDICATES.get(demand.intent, ()):
                    for endpoint in posting(entity, predicate, direction):
                        publish(path, endpoint, demand_index, seed)
                if depth >= 2:
                    continue
                transitions: tuple[str, ...] = ()
                if demand.intent in {'contact', 'channel'}:
                    transitions = ('maintained_by', 'owned_by', 'escalates_to') if depth == 0 else (
                        ('escalates_to',) if first_relation in {'maintained_by', 'owned_by'} else ())
                elif demand.intent in {'location', 'access'} and depth == 0:
                    transitions = ('uses',)
                for predicate in transitions:
                    for edge in posting(entity, predicate, 'out'):
                        target = edge.fact.object_entity
                        if target is None or edge.id in path:
                            continue
                        if target not in visited and len(visited) >= 32:
                            truncated.add('visited_limit')
                            continue
                        visited.add(target)
                        queue.append((target, (*path, edge.id), depth + 1, first_relation or predicate))
            if not any(s.demand_index == demand_index for s in supports):
                rejections.append((seed, 'no_complete_path'))
    supports.sort(key=lambda s: (s.demand_index, len(s.members), s.members))
    ordered = tuple(dict.fromkeys(identity for support in supports for identity in support.members))
    return CandidateSet(tuple(emitted[i] for i in ordered), tuple(supports),
        {'relation_graph_ns': perf_counter_ns() - started},
        {'index_probes': probes, 'examined_assertions': scanned, 'emitted_assertions': len(emitted),
         'visited_nodes': len(visited), 'seeds': len(seeds), **{reason: 1 for reason in truncated}}, tuple(rejections))
