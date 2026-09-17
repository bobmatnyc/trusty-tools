"""Validated source assertions and clock-specific eligibility; gold is separate."""
from __future__ import annotations
from dataclasses import dataclass, asdict, fields
from datetime import datetime
import hashlib
import json
from pathlib import Path
from typing import TypeAlias, cast

JSON: TypeAlias = None | bool | int | float | str | list['JSON'] | dict[str, 'JSON']

class ExperimentError(Exception):
    """Base for explicit, non-fallback experiment failures."""
class FixtureError(ExperimentError):
    pass
class ProtocolError(ExperimentError):
    pass
class ModelArtifactError(ExperimentError):
    pass
class IntegrityError(ExperimentError):
    pass

def obj(value: JSON) -> dict[str, JSON]:
    if not isinstance(value, dict):
        raise FixtureError('expected object')
    return value

def array(value: JSON) -> list[JSON]:
    if not isinstance(value, list):
        raise FixtureError('expected array')
    return value

def string(value: JSON) -> str:
    if not isinstance(value, str) or not value:
        raise FixtureError('expected nonempty string')
    return value

def optional_string(value: JSON) -> str | None:
    return None if value is None else string(value)

def integer(value: JSON) -> int:
    if type(value) is not int:
        raise FixtureError('expected integer')
    return value

def boolean(value: JSON) -> bool:
    if type(value) is not bool:
        raise FixtureError('expected boolean')
    return value

def keys(value: dict[str, JSON], expected: set[str]) -> None:
    if set(value) != expected:
        raise FixtureError(f'keys differ: {set(value) ^ expected}')

def stamp(value: str) -> str:
    if not value.endswith('Z'):
        raise FixtureError('timestamp must end in Z')
    try:
        datetime.fromisoformat(value.replace('Z', '+00:00'))
    except ValueError as error:
        raise FixtureError('invalid timestamp') from error
    return value

def digest(value: object) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()).hexdigest()

@dataclass(frozen=True)
class Fact:
    fact_id: str
    subject: str
    predicate: str
    object: str
    object_entity: str | None
    claim: str
    start_byte: int
    end_byte: int
    valid_from: str
    valid_to: str | None
    single_value_slot: str | None
    standing: bool

@dataclass(frozen=True)
class Source:
    source_id: str
    scope: str
    revision: int
    title: str
    body: str
    observed_at: str
    verified_at: str | None
    expires_at: str | None
    deleted: bool
    facts: tuple[Fact, ...]
    @property
    def key(self) -> str:
        return f'{self.scope}|{self.source_id}'
    @property
    def fingerprint(self) -> str:
        return digest(asdict(self))

@dataclass(frozen=True)
class Query:
    id: str
    split: str
    scenario: str
    category: str
    prompt: str
    scope: str
    as_of: str
    knowledge_cutoff: str
    entity_hint: str | None
    @property
    def text(self) -> str:
        return self.prompt + (' ' + self.entity_hint if self.entity_hint else '')
    @property
    def projection(self) -> str:
        return digest((self.scenario, self.scope, self.as_of, self.knowledge_cutoff))[:20]

@dataclass(frozen=True)
class Evidence:
    source: Source
    fact: Fact
    @property
    def id(self) -> str:
        return f'{self.source.key}|{self.source.revision}|{self.fact.fact_id}'
    @property
    def triple(self) -> tuple[str, str, str]:
        return self.fact.subject, self.fact.predicate, self.fact.object

@dataclass(frozen=True)
class Event:
    sequence: int
    source: Source

@dataclass(frozen=True)
class Policy:
    version: str = 'graph-prompt-v1'
    max_seeds: int = 1
    max_hops: int = 1
    max_scanned_edges: int = 128
    max_emitted_edges: int = 32
    minimum_cosine: float = 0.25

@dataclass(frozen=True)
class Gold:
    query_id: str
    required: tuple[tuple[str, ...], ...]
    acceptable: tuple[str, ...]
    forbidden: tuple[str, ...]
    expected_empty: bool

def parse_source(value: JSON) -> Source:
    row = obj(value)
    keys(row, {f.name for f in fields(Source)})
    facts = []
    for value in array(row['facts']):
        f = obj(value)
        keys(f, {v.name for v in fields(Fact)})
        facts.append(Fact(string(f['fact_id']), string(f['subject']), string(f['predicate']),
            string(f['object']), optional_string(f['object_entity']), string(f['claim']),
            integer(f['start_byte']), integer(f['end_byte']), stamp(string(f['valid_from'])),
            stamp(string(f['valid_to'])) if f['valid_to'] else None,
            optional_string(f['single_value_slot']), boolean(f['standing'])))
    if not isinstance(row['body'], str):
        raise FixtureError('body must be string')
    s = Source(string(row['source_id']), string(row['scope']), integer(row['revision']),
        string(row['title']), row['body'], stamp(string(row['observed_at'])),
        stamp(string(row['verified_at'])) if row['verified_at'] else None,
        stamp(string(row['expires_at'])) if row['expires_at'] else None,
        boolean(row['deleted']), tuple(facts))
    if s.revision < 1 or '|' in s.scope or '|' in s.source_id:
        raise FixtureError('invalid source identity')
    if s.deleted and (s.facts or s.body):
        raise FixtureError('tombstone contains source material')
    if len({f.fact_id for f in facts}) != len(facts):
        raise FixtureError('duplicate fact identity')
    for fact in facts:
        if '|' in fact.fact_id or fact.start_byte < 0 or fact.end_byte <= fact.start_byte:
            raise FixtureError('invalid fact identity or span')
        if s.body.encode()[fact.start_byte:fact.end_byte] != fact.claim.encode():
            raise FixtureError(f'exact claim span differs: {s.key}/{fact.fact_id}')
        if fact.valid_to and fact.valid_to <= fact.valid_from:
            raise FixtureError('invalid validity interval')
    return s

def load_inputs(root: Path) -> tuple[tuple[Source, ...], tuple[Event, ...], tuple[Query, ...]]:
    data = obj(json.loads((root / 'sources.json').read_text()))
    keys(data, {'version', 'sources', 'events'})
    if data['version'] != 'graph-prompt-v1':
        raise FixtureError('unknown fixture version')
    sources = tuple(parse_source(s) for s in array(data['sources']))
    events: list[Event] = []
    current = {s.key: s for s in sources}
    if len(current) != len(sources):
        raise FixtureError('duplicate initial source')
    for value in array(data['events']):
        row = obj(value)
        keys(row, {'sequence', 'source'})
        e = Event(integer(row['sequence']), parse_source(row['source']))
        if (events and e.sequence <= events[-1].sequence) or e.sequence < 1:
            raise FixtureError('non-increasing event sequence')
        if e.source.key in current and e.source.revision <= current[e.source.key].revision:
            raise FixtureError('non-increasing revision')
        current[e.source.key] = e.source
        events.append(e)
    qdata = obj(json.loads((root / 'queries.json').read_text()))
    keys(qdata, {'queries'})
    queries = []
    for value in array(qdata['queries']):
        row = obj(value)
        keys(row, {f.name for f in fields(Query)})
        q = Query(string(row['id']), string(row['split']), string(row['scenario']), string(row['category']),
            string(row['prompt']), string(row['scope']), string(row['as_of']), string(row['knowledge_cutoff']),
            optional_string(row['entity_hint']))
        stamp(q.as_of)
        stamp(q.knowledge_cutoff)
        if q.split not in {'tune', 'heldout'} or q.scenario not in {'initial', 'updated'}:
            raise FixtureError('unknown split/scenario')
        queries.append(q)
    if len({q.id for q in queries}) != len(queries):
        raise FixtureError('duplicate query id')
    validate_fixture(sources, tuple(queries))
    return sources, tuple(events), tuple(queries)

def validate_fixture(sources: tuple[Source, ...], queries: tuple[Query, ...]) -> None:
    split_scopes = {split: {q.scope for q in queries if q.split == split} for split in ('tune', 'heldout')}
    vocab = {split: {entity.casefold() for s in sources if s.scope in scopes for f in s.facts
        for entity in (f.subject, f.object_entity) if entity} for split, scopes in split_scopes.items()}
    if vocab['tune'] & vocab['heldout']:
        raise FixtureError(f'entity/alias overlap across splits: {sorted(vocab["tune"] & vocab["heldout"])[:4]}')

def eligible_facts(sources: tuple[Source, ...], query: Query) -> tuple[Evidence, ...]:
    candidates = [Evidence(s, f) for s in sources if s.scope == query.scope and not s.deleted
        and s.observed_at <= query.knowledge_cutoff and (s.expires_at is None or query.as_of < s.expires_at)
        for f in s.facts if f.valid_from <= query.as_of and (f.valid_to is None or query.as_of < f.valid_to)]
    newest: dict[str, tuple[str, str, int]] = {}
    for e in candidates:
        if e.fact.single_value_slot:
            key = e.fact.single_value_slot
            newest[key] = max(newest.get(key, ('', '', 0)), (e.fact.valid_from, e.source.observed_at, e.source.revision))
    # Tied assertions remain visible ambiguity; IDs order them but do not erase conflict.
    return tuple(sorted((e for e in candidates if not e.fact.single_value_slot or
        (e.fact.valid_from, e.source.observed_at, e.source.revision) == newest[e.fact.single_value_slot]), key=lambda e: e.id))

def read_gold(root: Path, split: str, queries: tuple[Query, ...]) -> dict[str, Gold]:
    data = obj(json.loads((root / f'gold-{split}.json').read_text()))
    keys(data, {'gold'})
    wanted = {q.id for q in queries if q.split == split}
    result = {}
    for value in array(data['gold']):
        row = obj(value)
        if row.get('query_id') not in wanted:
            continue
        keys(row, {f.name for f in fields(Gold)})
        g = Gold(string(row['query_id']), tuple(tuple(string(x) for x in array(group)) for group in array(row['required'])),
            tuple(string(x) for x in array(row['acceptable'])), tuple(string(x) for x in array(row['forbidden'])), boolean(row['expected_empty']))
        result[g.query_id] = g
    if set(result) != wanted:
        raise FixtureError('gold/query mismatch')
    return result
