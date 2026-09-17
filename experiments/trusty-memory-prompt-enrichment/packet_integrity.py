"""Independently reconstruct emitted text from authoritative records and public formatting."""
from __future__ import annotations
from typing import cast
import tiktoken
from adapters import RustHelper
from records import Evidence, Source, Query, JSON, IntegrityError, eligible_facts, string
from retrieval import Packet


def validate_packet(packet: Packet, sources: tuple[Source, ...], query: Query, *,
                    treatment: str, budget: int, helper: RustHelper,
                    encoding: tiktoken.Encoding) -> None:
    """Reject extra/missing assertion text, forged sidecars, and false token accounting."""
    source_map = {s.key: s for s in sources}
    authoritative: list[Evidence] = []
    ids: set[str] = set()
    for evidence in packet.evidence:
        source = source_map.get(evidence.source.key)
        if source is None or source.fingerprint != evidence.source.fingerprint:
            raise IntegrityError('source digest/revision mismatch')
        facts = {f.fact_id: f for f in source.facts}
        fact = facts.get(evidence.fact.fact_id)
        if fact is None or fact != evidence.fact:
            raise IntegrityError('evidence fact differs from authoritative source')
        if source.body.encode()[fact.start_byte:fact.end_byte] != fact.claim.encode():
            raise IntegrityError('invalid authoritative source span')
        if evidence.id in ids:
            raise IntegrityError('duplicate packet evidence')
        ids.add(evidence.id)
        authoritative.append(Evidence(source, fact))

    def formatted(evidence: list[Evidence], native: bool = False) -> str:
        triples = [[e.fact.subject, e.fact.predicate, e.fact.object] if native
            else [e.id, 'is_fact', e.fact.claim] for e in evidence]
        if not triples:
            return ''
        result = helper.request({'op': 'format', 'triples': cast(list[JSON], triples)})
        return string(result['text'])

    eligible = eligible_facts(sources, query)
    if treatment == 'standing_cache':
        if any(not e.fact.standing for e in authoritative):
            raise IntegrityError('standing cache contains task evidence')
        expected_text = formatted(authoritative, native=True)
        prelude_text = expected_text
        prelude_ids = ids
    else:
        predicates = sorted({e.fact.predicate for e in eligible if e.fact.standing})
        result = helper.request({'op': 'classify', 'predicates': cast(list[JSON], predicates)})
        hot = {p for p, value in zip(predicates, cast(list[bool], result['hot'])) if value}
        prelude: list[Evidence] = []
        for e in eligible:
            if not e.fact.standing or e.fact.predicate not in hot:
                continue
            candidate = formatted(prelude + [e])
            if len(encoding.encode(candidate, disallowed_special=())) <= budget:
                prelude.append(e)
        prelude_ids = {e.id for e in prelude}
        if [e.id for e in authoritative[:len(prelude)]] != [e.id for e in prelude]:
            raise IntegrityError('common standing prelude differs from source projection')
        prelude_text = formatted(prelude)
        task = authoritative[len(prelude):]
        if treatment == 'bm25_source_packet':
            expected_text = prelude_text
            grouped: dict[str, list[Evidence]] = {}
            for e in task:
                grouped.setdefault(e.source.key, []).append(e)
            for key, selected in grouped.items():
                source = source_map[key]
                complete = {e.id for e in eligible if e.source.key == key} - prelude_ids
                if {e.id for e in selected} != complete:
                    raise IntegrityError('source block has unregistered assertions')
                allowed = {e.fact.fact_id for e in eligible if e.source.key == key}
                body = source.body.encode()
                for f in sorted(source.facts, key=lambda f: f.start_byte, reverse=True):
                    if f.fact_id not in allowed:
                        body = body[:f.start_byte] + body[f.end_byte:]
                expected_text += f'### {source.title}\n{body.decode()}\n'
        elif treatment in {'bm25', 'bm25_graph', 'bm25_dense', 'bm25_graph_dense', 'graph_only', 'current_lexical_graph'}:
            expected_text = prelude_text + formatted(task, native=treatment == 'current_lexical_graph')
        else:
            raise IntegrityError('unsupported packet representation')
    if packet.text != expected_text:
        raise IntegrityError('packet contains missing, changed, or unregistered assertion text')
    tokens = len(encoding.encode(expected_text, disallowed_special=()))
    prelude_tokens = len(encoding.encode(prelude_text, disallowed_special=()))
    if tokens != packet.tokens or tokens > budget or packet.prelude_tokens != prelude_tokens:
        raise IntegrityError('packet token count or budget differs')
    if packet.standing_ids != prelude_ids:
        raise IntegrityError('packet standing identity accounting differs')
