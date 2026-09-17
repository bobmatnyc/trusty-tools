"""Eligible lexical and directed relation indexes; missing records remain source-readable."""
from __future__ import annotations
from dataclasses import dataclass
from time import perf_counter_ns
from typing import cast
import re
import tempfile
import numpy as np
from legacy import Source, Evidence, Projection, RustHelper, JSON, eligible_facts, digest
from contracts import Task, VERSION
from maintenance import DerivedStore

STRUCTURAL = {'tags', 'contains', 'mentioned_in', 'mentioned-in'}

@dataclass
class RelevanceIndex:
    evidence: dict[str, Evidence]
    by_source: dict[str, tuple[Evidence, ...]]
    old: Projection
    names: tuple[str, ...]
    aliases: dict[str, tuple[str, ...]]
    alias_evidence: dict[str, tuple[Evidence, ...]]
    postings: dict[tuple[str, str, str], tuple[Evidence, ...]]
    source_id: str
    claim_id: str
    fallback_id: str
    missing_sources: tuple[str, ...]
    generation: str
    build_ns: int
    scratch: tempfile.TemporaryDirectory[str]
    native_build: dict[str, JSON]

    def close(self) -> None:
        self.scratch.cleanup()


def build_index(sources: tuple[Source, ...], task: Task, store: DerivedStore, helper: RustHelper) -> RelevanceIndex:
    started = perf_counter_ns()
    eligible = eligible_facts(sources, task.legacy_query())
    evidence = {e.id: e for e in eligible}
    by_source: dict[str, list[Evidence]] = {}
    adjacency: dict[str, list[str]] = {}
    aliases: dict[str, set[str]] = {}
    alias_evidence: dict[str, list[Evidence]] = {}
    postings: dict[tuple[str, str, str], list[Evidence]] = {}
    entities: set[str] = set()
    for e in eligible:
        f = e.fact
        by_source.setdefault(e.source.key, []).append(e)
        postings.setdefault((f.subject, f.predicate, 'out'), []).append(e)
        if f.object_entity:
            postings.setdefault((f.object_entity, f.predicate, 'in'), []).append(e)
        if f.predicate in STRUCTURAL:
            continue
        entities.add(f.subject)
        adjacency.setdefault(f.subject, []).append(e.id)
        if f.object_entity:
            entities.add(f.object_entity)
            adjacency.setdefault(f.object_entity, []).append(e.id)
        if f.predicate == 'is_alias_for' and f.object_entity:
            aliases.setdefault(f.subject, set()).add(f.object_entity)
            alias_evidence.setdefault(f.subject, []).append(e)
    generation = digest((VERSION, store.source_hash(), store.derived_hash(), task.scope, task.as_of, task.knowledge_cutoff))
    source_id, claim_id, fallback_id = (generation + suffix for suffix in ('-source', '-claims', '-fallback'))
    scratch = tempfile.TemporaryDirectory(prefix='memory-relevance-')
    missing = tuple(sorted(key for key in by_source if not store.valid(key)))
    documents = [{'id': key, 'text': es[0].source.title + '\n' + es[0].source.body} for key, es in sorted(by_source.items())]
    materialized_claims = {identity: text for key, record in store.records.items() if key in by_source and key not in missing
        for identity, text in record.claims}
    claim_documents = [{'id': e.id, 'text': materialized_claims[e.id]} for e in eligible if e.source.key not in missing]
    builds = {}
    for identity, docs in ((source_id, documents), (claim_id, claim_documents),
                           (fallback_id, [d for d in documents if d['id'] in missing])):
        builds[identity] = helper.request({'op': 'load', 'projection': identity, 'documents': cast(list[JSON], docs),
            'triples': [], 'scratch_dir': scratch.name})
    predicates = sorted({e.fact.predicate for e in eligible})
    hot_values = helper.request({'op': 'classify', 'predicates': cast(list[JSON], predicates)})
    hot = {p for p, value in zip(predicates, cast(list[bool], hot_values['hot'])) if value}
    stable_sources = {key: tuple(sorted(es, key=lambda e: (e.fact.start_byte, e.id))) for key, es in sorted(by_source.items())}
    seed_index: dict[str, list[str]] = {}
    for entity in sorted(entities):
        tokens = re.findall(r'[\w-]+', entity.casefold())
        if tokens:
            seed_index.setdefault(tokens[0], []).append(entity)
    old = Projection(source_id, evidence, stable_sources, {k: tuple(sorted(set(v))) for k, v in adjacency.items()},
        {k.casefold(): tuple(sorted(v)) for k, v in aliases.items()}, tuple(sorted(entities)),
        {k: tuple(v) for k, v in seed_index.items()}, (), np.empty((0, 384), dtype=np.float32),
        tuple(e for e in eligible if e.fact.standing and e.fact.predicate in hot), hot,
        perf_counter_ns()-started, sum(e.fact.predicate in STRUCTURAL for e in eligible),
        sum(e.fact.predicate not in STRUCTURAL for e in eligible), {})
    return RelevanceIndex(evidence, stable_sources, old, tuple(sorted(entities)),
        {k: tuple(sorted(v)) for k, v in aliases.items()},
        {k: tuple(sorted(v, key=lambda e: e.id)) for k, v in alias_evidence.items()},
        {k: tuple(sorted(v, key=lambda e: e.id)) for k, v in postings.items()}, source_id, claim_id,
        fallback_id, missing, generation, perf_counter_ns()-started, scratch, cast(dict[str, JSON], builds))
