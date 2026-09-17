"""Strict canonical timestamps and evaluator-only fixture routing; no policy synonyms."""
from __future__ import annotations
import json
from pathlib import Path
import re
from legacy import Source, Event, Query, JSON, FixtureError, parse_source, obj, array, string, integer, keys

TIME = re.compile(r'\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z')


def canonical(value: JSON) -> None:
    if value is not None and (not isinstance(value, str) or TIME.fullmatch(value) is None):
        raise FixtureError('timestamp must be canonical UTC second precision')


def source_record(value: JSON) -> Source:
    row = obj(value)
    for key in ('observed_at', 'verified_at', 'expires_at'):
        canonical(row[key])
    for value in array(row['facts']):
        fact = obj(value)
        canonical(fact['valid_from'])
        canonical(fact['valid_to'])
    return parse_source(row)


def load_sources(root: Path) -> tuple[tuple[Source, ...], tuple[Event, ...]]:
    row = obj(json.loads((root/'sources.json').read_text()))
    keys(row, {'version', 'sources', 'events'})
    if row['version'] != 'graph-prompt-v1':
        raise FixtureError('unknown source schema')
    sources = tuple(source_record(value) for value in array(row['sources']))
    if len({s.key for s in sources}) != len(sources):
        raise FixtureError('duplicate source')
    events = []
    for value in array(row['events']):
        event = obj(value)
        keys(event, {'sequence', 'source'})
        events.append(Event(integer(event['sequence']), source_record(event['source'])))
    return sources, tuple(events)


def load_queries(root: Path) -> tuple[Query, ...]:
    row = obj(json.loads((root/'queries.json').read_text()))
    keys(row, {'queries'})
    result = []
    for value in array(row['queries']):
        q = obj(value)
        keys(q, {'id', 'split', 'scenario', 'category', 'prompt', 'scope', 'as_of', 'knowledge_cutoff', 'entity_hint'})
        if q['entity_hint'] is not None:
            raise FixtureError('entity hints are forbidden')
        canonical(q['as_of'])
        canonical(q['knowledge_cutoff'])
        query = Query(string(q['id']), string(q['split']), string(q['scenario']), string(q['category']),
            string(q['prompt']), string(q['scope']), string(q['as_of']), string(q['knowledge_cutoff']), None)
        if query.split not in {'tune', 'heldout'} or query.scenario not in {'initial', 'updated'}:
            raise FixtureError('unsupported query routing')
        result.append(query)
    if len({q.id for q in result}) != len(result):
        raise FixtureError('duplicate query ID')
    return tuple(result)
