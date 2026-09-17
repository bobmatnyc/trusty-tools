"""Matched candidate fusion and complete-claim packing under exact token budgets."""
from __future__ import annotations
from dataclasses import dataclass
import re
from time import perf_counter_ns
from typing import cast
import numpy as np
import tiktoken
from adapters import LocalEncoder, RustHelper
from projection import Projection, graph_lookup
from records import Evidence, Query, Policy, JSON, IntegrityError, array, obj, string, integer

TREATMENTS = ('bm25', 'bm25_graph', 'bm25_dense', 'bm25_graph_dense',
    'bm25_source_packet', 'standing_cache', 'current_lexical_graph', 'graph_only')

@dataclass
class Retrieval:
    evidence: list[Evidence]
    timings: dict[str, int]
    details: dict[str, JSON]

@dataclass
class Packet:
    text: str
    tokens: int
    evidence: list[Evidence]
    standing_ids: set[str]
    prelude_tokens: int
    dropped_ids: list[str]
    renderings: dict[str, str]
    timings: dict[str, int]


def retrieve(query: Query, treatment: str, index: Projection, model: LocalEncoder,
             helper: RustHelper, policy: Policy) -> Retrieval:
    started = perf_counter_ns()
    timings: dict[str, int] = {}
    details: dict[str, JSON] = {}
    lanes: list[list[Evidence]] = []
    if treatment.startswith('bm25'):
        tick = perf_counter_ns()
        result = helper.request({'op': 'search', 'projection': index.key, 'text': query.text, 'limit': 20})
        timings['bm25_ns'] = perf_counter_ns() - tick
        details['bm25_api_ns'] = result['_helper_ns']
        lanes.append([e for value in array(result['hits']) for e in index.by_source[string(obj(value)['id'])]])
    if 'dense' in treatment:
        tick = perf_counter_ns()
        vector = model.encode((query.text,))[0]
        timings['query_embedding_ns'] = perf_counter_ns() - tick
        tick = perf_counter_ns()
        scores = index.vectors @ vector
        order = sorted((i for i, score in enumerate(scores) if score >= policy.minimum_cosine),
            key=lambda i: (-float(scores[i]), index.document_ids[i]))[:20]
        lanes.append([e for i in order for e in index.by_source[index.document_ids[i]]])
        timings['dense_similarity_ns'] = perf_counter_ns() - tick
    if treatment in {'bm25_graph', 'bm25_graph_dense', 'graph_only'}:
        evidence, details = graph_lookup(query, index, policy)
        timings['graph_ns'] = integer(details['graph_ns'])
        lanes.append(evidence)
    if treatment == 'standing_cache':
        lanes.append(list(index.standing))
    if treatment == 'current_lexical_graph':
        tick = perf_counter_ns()
        result = helper.request({'op': 'current_page', 'projection': index.key, 'limit': 200})
        words = {w for w in re.split(r'[^\w-]', query.text.casefold()) if len(w) >= 3}
        triples = []
        mapping = {e.triple: e for e in index.evidence.values()}
        for value in array(result['triples']):
            row = obj(value)
            triple = string(row['subject']), string(row['predicate']), string(row['object'])
            if triple[1] not in index.hot:
                continue
            overlap_words = {word for field in (triple[0], triple[2]) for word in re.split(r'[:\s_/-]', field.casefold()) if len(word) >= 3}
            if words & overlap_words and triple in mapping:
                triples.append(mapping[triple])
            if len(triples) >= 8:
                break
        timings['current_policy_ns'] = perf_counter_ns() - tick
        details['page_api_ns'] = result['api_ns']
        lanes.append(triples)
    tick = perf_counter_ns()
    scores_by_id: dict[str, float] = {}
    candidates: dict[str, Evidence] = {}
    for lane in lanes:
        for rank, e in enumerate(lane, 1):
            scores_by_id[e.id] = scores_by_id.get(e.id, 0) + 1 / (60 + rank)
            candidates[e.id] = e
    fused_order = sorted(candidates, key=lambda i: (-scores_by_id[i], i))
    timings['fusion_ns'] = perf_counter_ns() - tick
    timings['retrieval_ns'] = perf_counter_ns() - started
    return Retrieval([candidates[i] for i in fused_order], timings, details)


def format_claims(evidence: list[Evidence], helper: RustHelper, native: bool = False) -> str:
    triples = [list(e.triple) if native else [e.id, 'is_fact', e.fact.claim] for e in evidence]
    return string(helper.request({'op': 'format', 'triples': cast(list[JSON], triples)})['text']) if triples else ''


def native_bullet(e: Evidence) -> str:
    if e.fact.predicate in {'is_alias_for', 'is_shorthand_for'}:
        return f'- {e.fact.subject} → {e.fact.object}'
    return f'- {e.fact.object}'


def pack(query: Query, retrieval: Retrieval, index: Projection, budget: int,
         helper: RustHelper, encoding: tiktoken.Encoding, treatment: str,
         prelude: bool = True) -> Packet:
    """Never truncate a fact; include only complete, verifiable evidence within budget."""
    started = perf_counter_ns()
    native = treatment in {'standing_cache', 'current_lexical_graph'}
    selected: list[Evidence] = []
    dropped: list[str] = []
    standing_ids: set[str] = set()
    renderings: dict[str, str] = {}
    text = ''
    prelude_tokens = 0
    # The common standing prelude always uses the matched extractive representation.
    for e in index.standing if prelude else ():
        candidate = format_claims(selected + [e], helper)
        if len(encoding.encode(candidate, disallowed_special=())) <= budget:
            selected.append(e)
            standing_ids.add(e.id)
            renderings[e.id] = e.fact.claim
            text = candidate
        else:
            dropped.append(e.id)
    prelude_text = text
    prelude_tokens = len(encoding.encode(text, disallowed_special=()))
    task: list[Evidence] = []
    source_ids: set[str] = set()
    for e in retrieval.evidence:
        if e.id in {v.id for v in selected}:
            continue
        if treatment == 'bm25_source_packet':
            if e.source.key in source_ids:
                continue
            source_ids.add(e.source.key)
            group = [v for v in index.by_source[e.source.key] if v.id not in {v.id for v in selected}]
            body = e.source.body.encode()
            eligible_ids = {v.fact.fact_id for v in index.by_source[e.source.key]}
            for fact in sorted(e.source.facts, key=lambda f: f.start_byte, reverse=True):
                if fact.fact_id not in eligible_ids:
                    body = body[:fact.start_byte] + body[fact.end_byte:]
            block = f'### {e.source.title}\n{body.decode()}\n'
            candidate = text + block
        else:
            group = [e]
            candidate = prelude_text + format_claims(task + group, helper, native=native)
        if len(encoding.encode(candidate, disallowed_special=())) > budget:
            dropped.extend(v.id for v in group)
            continue
        selected.extend(group)
        task.extend(group)
        text = candidate
        for value in group:
            renderings[value.id] = native_bullet(value) if native else value.fact.claim
    tokens = len(encoding.encode(text, disallowed_special=()))
    if tokens > budget or any(renderings[e.id] not in text for e in selected):
        raise IntegrityError('packet budget or complete-evidence contract failed')
    if treatment == 'standing_cache':
        standing_ids = {e.id for e in selected if e.fact.standing}
        prelude_tokens = tokens
    return Packet(text, tokens, selected, standing_ids, prelude_tokens, dropped, renderings,
        {**retrieval.timings, 'packing_ns': perf_counter_ns() - started})
