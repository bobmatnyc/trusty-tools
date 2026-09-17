"""Atomic per-source materialization with explicit missing-derived fallback and checkpoints."""
from __future__ import annotations
from dataclasses import dataclass, field, asdict
from time import perf_counter_ns
import json
from typing import Callable, cast
from contracts import VERSION
from legacy import Source, Event, JSON, IntegrityError, digest, parse_source, obj, array, integer, string

@dataclass(frozen=True)
class DerivedRecord:
    revision: int
    source_digest: str
    policy: str
    claims: tuple[tuple[str, str], ...]
    postings: tuple[tuple[str, str, str, str], ...]
    aliases: tuple[tuple[str, str, str], ...]

@dataclass
class DerivedStore:
    sources: dict[str, Source]
    records: dict[str, DerivedRecord] = field(default_factory=dict)
    sequence: int = 0
    replays: dict[int, str] = field(default_factory=dict)
    tombstones: dict[str, int] = field(default_factory=dict)

    def source_hash(self) -> str:
        return digest([(k, s.fingerprint) for k, s in sorted(self.sources.items())])

    def derived_hash(self) -> str:
        return digest([(k, asdict(r)) for k, r in sorted(self.records.items())])

    def valid(self, key: str) -> bool:
        s, r = self.sources[key], self.records.get(key)
        return r is not None and r.policy == VERSION and r.source_digest == s.fingerprint and r.revision == s.revision and r == derive_source(s)

    def checkpoint(self) -> str:
        return json.dumps({'version': VERSION, 'sources': [asdict(s) for _, s in sorted(self.sources.items())],
            'records': {k: asdict(r) for k, r in sorted(self.records.items())}, 'sequence': self.sequence,
            'replays': {str(k): v for k, v in sorted(self.replays.items())}, 'tombstones': self.tombstones}, sort_keys=True)

    @classmethod
    def restore(cls, text: str) -> DerivedStore:
        data = obj(json.loads(text))
        if data.get('version') != VERSION:
            raise IntegrityError('unsupported checkpoint version')
        sources = [parse_source(row) for row in array(data['sources'])]
        store = cls({s.key: s for s in sources}, sequence=integer(data['sequence']))
        for key, value in obj(data['records']).items():
            row = obj(value)
            store.records[key] = DerivedRecord(integer(row['revision']), string(row['source_digest']), string(row['policy']),
                tuple(cast(tuple[str, str], tuple(string(v) for v in array(x))) for x in array(row['claims'])),
                tuple(cast(tuple[str, str, str, str], tuple(string(v) for v in array(x))) for x in array(row['postings'])),
                tuple(cast(tuple[str, str, str], tuple(string(v) for v in array(x))) for x in array(row['aliases'])))
        store.replays = {int(k): string(v) for k, v in obj(data['replays']).items()}
        store.tombstones = {k: integer(v) for k, v in obj(data['tombstones']).items()}
        for key in store.records:
            if key not in store.sources:
                raise IntegrityError('orphan derived record')
            if store.valid(key) and store.records[key] != derive_source(store.sources[key]):
                raise IntegrityError('checkpoint derived content differs from source')
        return store


def derive_source(source: Source) -> DerivedRecord:
    """Materialize exact claims/directed postings without altering any source clocks."""
    claims, postings, aliases = [], [], []
    for fact in source.facts:
        identity = f'{source.key}|{source.revision}|{fact.fact_id}'
        claims.append((identity, source.title + '\n' + fact.claim))
        postings.append((fact.subject, fact.predicate, 'out', identity))
        if fact.object_entity:
            postings.append((fact.object_entity, fact.predicate, 'in', identity))
        if fact.predicate == 'is_alias_for' and fact.object_entity:
            aliases.append((fact.subject, fact.object_entity, identity))
    return DerivedRecord(source.revision, source.fingerprint, VERSION,
        tuple(sorted(claims)), tuple(sorted(postings)), tuple(sorted(aliases)))


def maintain(store: DerivedStore, sources: tuple[Source, ...], events: tuple[Event, ...], batch_limit: int,
             derive: Callable[[Source], DerivedRecord] = derive_source) -> dict[str, JSON]:
    """Publish at most batch_limit source records atomically; failed staging leaves no changes."""
    if batch_limit < 1:
        raise IntegrityError('batch limit must be positive')
    start = perf_counter_ns()
    staged = DerivedStore(dict(store.sources), dict(store.records), store.sequence, dict(store.replays), dict(store.tombstones))
    examined = len(sources) + len(events) + len(store.sources)
    for source in sources:
        if source.key not in staged.sources:
            staged.sources[source.key] = source
    previous = 0
    for event in events:
        if event.sequence <= previous:
            raise IntegrityError('event sequence is not increasing')
        previous = event.sequence
        if event.sequence <= staged.sequence and staged.replays.get(event.sequence) != event.source.fingerprint:
            raise IntegrityError('conflicting replay')
    changed = removed = records_examined = 0
    touched: set[str] = set()
    validation_records = 0
    def complete(key: str) -> bool:
        nonlocal validation_records
        record = staged.records.get(key)
        if record:
            validation_records += len(record.claims) + len(record.postings) + len(record.aliases) + len(staged.sources[key].facts)
        return staged.valid(key)
    for event in events:
        if event.sequence <= staged.sequence:
            continue
        if len(touched) >= batch_limit and event.source.key not in touched:
            break
        source = event.source
        old = staged.sources.get(source.key)
        if source.revision <= max(old.revision if old else 0, staged.tombstones.get(source.key, 0)):
            raise IntegrityError('source revision regressed')
        records_examined += len(staged.records[source.key].claims) if source.key in staged.records else 0
        staged.sources[source.key] = source
        staged.records.pop(source.key, None)
        if source.deleted:
            staged.tombstones[source.key] = source.revision
            removed += 1
        else:
            staged.records[source.key] = derive(source)
            records_examined += len(source.facts)
            changed += 1
        touched.add(source.key)
        staged.sequence = event.sequence
        staged.replays[event.sequence] = source.fingerprint
    for key in sorted(staged.sources):
        source = staged.sources[key]
        if source.deleted or complete(key) or key in touched:
            continue
        if len(touched) >= batch_limit:
            break
        staged.records[key] = derive(source)
        records_examined += len(source.facts)
        changed += 1
        touched.add(key)
    pending = sum(not s.deleted and not complete(k) for k, s in staged.sources.items())
    pending_events = sum(e.sequence > staged.sequence for e in events)
    store.sources, store.records, store.sequence = staged.sources, staged.records, staged.sequence
    store.replays, store.tombstones = staged.replays, staged.tombstones
    return {'changed': changed, 'removed': removed, 'pending_sources': pending, 'pending_events': pending_events,
        'examined_sources': examined, 'examined_derived_records': records_examined, 'validation_derived_items': validation_records,
        'copied_source_records': len(store.sources), 'published_source_ids': len(touched), 'cursor': store.sequence,
        'source_hash': store.source_hash(), 'derived_hash': store.derived_hash(), 'elapsed_ns': perf_counter_ns() - start}
