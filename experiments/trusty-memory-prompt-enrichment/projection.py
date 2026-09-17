"""Deterministic source revisions, cached vectors, and scoped bounded adjacency."""
from __future__ import annotations
from dataclasses import dataclass, asdict
from collections import deque
import re
from time import perf_counter_ns
from typing import cast
import numpy as np
from numpy.typing import NDArray
from adapters import LocalEncoder, RustHelper
from records import Evidence, Source, Query, Event, Policy, JSON, IntegrityError, digest, eligible_facts

STRUCTURAL = {'tags', 'contains', 'mentioned_in', 'mentioned-in'}

@dataclass
class SourceIndex:
    sources: dict[str, Source]
    vectors: dict[str, NDArray[np.float32]]
    sequence: int = 0

    def semantic_hash(self) -> str:
        return digest([(k, self.sources[k].fingerprint, self.vectors[k].tobytes().hex() if k in self.vectors else None) for k in sorted(self.sources)])

@dataclass
class Projection:
    key: str
    evidence: dict[str, Evidence]
    by_source: dict[str, tuple[Evidence, ...]]
    adjacency: dict[str, tuple[str, ...]]
    aliases: dict[str, tuple[str, ...]]
    entities: tuple[str, ...]
    seed_index: dict[str, tuple[str, ...]]
    document_ids: tuple[str, ...]
    vectors: NDArray[np.float32]
    standing: tuple[Evidence, ...]
    hot: set[str]
    build_ns: int
    structural_omitted: int
    graph_facts: int
    native: dict[str, JSON]


def build_source_index(sources: tuple[Source, ...], encoder: LocalEncoder) -> SourceIndex:
    ordered = tuple(sorted((s for s in sources if not s.deleted), key=lambda s: s.key))
    vectors = encoder.encode(tuple(s.title + '\n' + s.body for s in ordered), corpus=True)
    return SourceIndex({s.key: s for s in ordered}, {s.key: vectors[i] for i, s in enumerate(ordered)})


def update_projection(current: SourceIndex, events: tuple[Event, ...], limit: int,
                      encoder: LocalEncoder) -> dict[str, JSON]:
    """Apply a bounded event batch; unchanged cycles encode nothing or refresh no clocks."""
    if limit < 1:
        raise IntegrityError('maintenance limit must be positive')
    pending = [e for e in events if e.sequence > current.sequence]
    batch = pending[:limit]
    started = perf_counter_ns()
    changed = removed = 0
    staged_sources = dict(current.sources)
    staged_vectors = dict(current.vectors)
    sequence = current.sequence
    for event in batch:
        source = event.source
        old = staged_sources.get(source.key)
        if old and source.revision <= old.revision:
            raise IntegrityError('maintenance revision regressed')
        if source.deleted:
            removed += int(staged_sources.pop(source.key, None) is not None)
            staged_vectors.pop(source.key, None)
        else:
            staged_sources[source.key] = source
            staged_vectors[source.key] = encoder.encode((source.title + '\n' + source.body,), corpus=True)[0]
            changed += 1
        sequence = event.sequence
    current.sources, current.vectors, current.sequence = staged_sources, staged_vectors, sequence
    return {'changed': changed, 'removed': removed, 'pending_sequence': pending[len(batch)].sequence if len(pending) > len(batch) else None,
        'sequence': current.sequence, 'elapsed_ns': perf_counter_ns() - started,
        'source_hash': digest([(k, s.fingerprint) for k, s in sorted(current.sources.items())]),
        'derived_hash': current.semantic_hash()}


def backfill_vectors(current: SourceIndex, encoder: LocalEncoder, limit: int) -> dict[str, JSON]:
    """Publish a stable bounded batch of missing vectors without changing source records."""
    if limit < 1:
        raise IntegrityError('backfill limit must be positive')
    missing = sorted(set(current.sources) - set(current.vectors))
    keys = missing[:limit]
    values = encoder.encode(tuple(current.sources[k].title + '\n' + current.sources[k].body for k in keys), corpus=True)
    current.vectors.update({k: values[i] for i, k in enumerate(keys)})
    return {'changed': len(keys), 'pending': len(missing) - len(keys), 'derived_hash': current.semantic_hash()}


def build_projection(index: SourceIndex, query: Query, helper: RustHelper, scratch: str) -> Projection:
    started = perf_counter_ns()
    evidence = eligible_facts(tuple(index.sources.values()), query)
    mapping = {e.id: e for e in evidence}
    by_source: dict[str, tuple[Evidence, ...]] = {}
    for key in sorted({e.source.key for e in evidence}):
        by_source[key] = tuple(sorted((e for e in evidence if e.source.key == key), key=lambda e: (e.fact.start_byte, e.id)))
    adjacency: dict[str, list[str]] = {}
    aliases: dict[str, set[str]] = {}
    entities: set[str] = set()
    omitted = 0
    for e in evidence:
        f = e.fact
        if f.predicate in STRUCTURAL:
            omitted += 1
            continue
        entities.add(f.subject)
        adjacency.setdefault(f.subject, []).append(e.id)
        if f.object_entity:
            entities.add(f.object_entity)
            adjacency.setdefault(f.object_entity, []).append(e.id)
        if f.predicate == 'is_alias_for' and f.object_entity:
            aliases.setdefault(f.subject.casefold(), set()).add(f.object_entity)
    predicates = sorted({e.fact.predicate for e in evidence})
    classified = helper.request({'op': 'classify', 'predicates': cast(list[JSON], predicates)})
    hot = {p for p, is_hot in zip(predicates, cast(list[bool], classified['hot'])) if is_hot}
    documents = [{'id': key, 'text': index.sources[key].title + '\n' + index.sources[key].body} for key in by_source]
    triples = [{'id': e.id, 'subject': e.fact.subject, 'predicate': e.fact.predicate, 'object': e.fact.object,
        'valid_from': e.fact.valid_from, 'valid_to': None} for e in evidence]
    native = helper.request({'op': 'load', 'projection': query.projection, 'documents': cast(list[JSON], documents),
        'triples': cast(list[JSON], triples), 'scratch_dir': scratch})
    ids = tuple(key for key in by_source if key in index.vectors)
    vectors = np.stack([index.vectors[k] for k in ids]) if ids else np.empty((0, 384), dtype=np.float32)
    if not np.isfinite(vectors).all() or (len(ids) and (vectors.shape != (len(ids), 384) or
            not np.allclose(np.linalg.norm(vectors, axis=1), 1, atol=1e-5))):
        raise IntegrityError('invalid available dense records')
    return Projection(query.projection, mapping, by_source,
        {k: tuple(sorted(set(v))) for k, v in adjacency.items()}, {k: tuple(sorted(v)) for k, v in aliases.items()},
        tuple(sorted(entities)), {token: tuple(sorted(e for e in entities if re.findall(r'[\w-]+', e.casefold())[0] == token))
            for token in {re.findall(r'[\w-]+', e.casefold())[0] for e in entities}}, ids, vectors, tuple(e for e in evidence if e.fact.standing and e.fact.predicate in hot),
        hot, perf_counter_ns() - started, omitted, len(evidence) - omitted, native)


def graph_lookup(query: Query, projection: Projection, policy: Policy) -> tuple[list[Evidence], dict[str, JSON]]:
    """Exact unambiguous seeds; entity hops and every examined assertion are bounded."""
    started = perf_counter_ns()
    text = query.text.casefold()
    seeds = []
    ambiguous = 0
    candidates = {entity for word in re.findall(r'[\w-]+', text) for entity in projection.seed_index.get(word, ())}
    for entity in sorted(candidates):
        name = entity.casefold()
        if not re.search(r'(?<![\w-])' + re.escape(name) + r'(?![\w-])', text):
            continue
        alias = projection.aliases.get(name)
        if alias is not None:
            if len(alias) != 1:
                ambiguous += 1
                continue
            entity = alias[0]
        if entity not in seeds:
            seeds.append(entity)
    seeds.sort(key=lambda e: (0 if query.entity_hint and e.casefold() == query.entity_hint.casefold() else 1, e))
    seeds = seeds[:policy.max_seeds]
    emitted: dict[str, tuple[int, int]] = {}
    scanned = 0
    truncated = False
    for rank, seed in enumerate(seeds):
        queue = deque([(seed, 0)])
        visited = {seed}
        while queue:
            entity, depth = queue.popleft()
            for identity in projection.adjacency.get(entity, ()):
                if scanned >= policy.max_scanned_edges or len(emitted) >= policy.max_emitted_edges:
                    truncated = True
                    break
                scanned += 1
                e = projection.evidence[identity]
                f = e.fact
                if f.object_entity and depth >= policy.max_hops:
                    continue
                emitted.setdefault(identity, (rank, depth))
                if f.object_entity and depth < policy.max_hops:
                    neighbor = f.object_entity if f.subject == entity else f.subject
                    if neighbor not in visited:
                        visited.add(neighbor)
                        queue.append((neighbor, depth + 1))
            if truncated:
                break
        if truncated:
            break
    order = sorted(emitted, key=lambda identity: (*emitted[identity], identity))
    return [projection.evidence[i] for i in order], {'graph_ns': perf_counter_ns() - started,
        'seeds': cast(list[JSON], seeds), 'scanned_edges': scanned, 'emitted_edges': len(order),
        'truncated': truncated, 'ambiguous_aliases': ambiguous}
