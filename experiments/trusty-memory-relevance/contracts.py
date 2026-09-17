"""Typed relevance boundaries keep evaluator routing metadata outside retrieval."""
from __future__ import annotations
from dataclasses import dataclass, field
from legacy import Evidence, Query, IntegrityError

VERSION = 'memory-relevance-v1'
ARMS = ('baseline', 'selector', 'relation_graph', 'claim_index', 'cleanup', 'combined')

@dataclass(frozen=True)
class Task:
    prompt: str
    scope: str
    as_of: str
    knowledge_cutoff: str

    def legacy_query(self) -> Query:
        return Query('opaque', 'tune', 'initial', 'opaque', self.prompt, self.scope,
            self.as_of, self.knowledge_cutoff, None)

@dataclass(frozen=True)
class SelectorPolicy:
    unknown_overlap: int
    max_task_facts: int

    def __post_init__(self) -> None:
        if self.unknown_overlap not in (1, 2) or self.max_task_facts not in (4, 8):
            raise IntegrityError('unsupported selector policy')

GRID = tuple(SelectorPolicy(overlap, cap) for overlap, cap in ((1, 4), (1, 8), (2, 4), (2, 8)))

@dataclass(frozen=True)
class Demand:
    intent: str
    entities: tuple[str, ...]
    status: str
    aliases: tuple[str, ...] = ()
    clause: str = ''

@dataclass(frozen=True)
class Support:
    endpoint: str
    members: tuple[str, ...]
    demand_index: int
    entity: str = ''

@dataclass(frozen=True)
class Selection:
    evidence: tuple[Evidence, ...]
    supports: tuple[Support, ...] = ()
    rejections: tuple[tuple[str, str], ...] = ()

@dataclass
class CandidateSet:
    evidence: tuple[Evidence, ...]
    supports: tuple[Support, ...] = ()
    timings: dict[str, int] = field(default_factory=dict)
    counters: dict[str, int] = field(default_factory=dict)
    rejections: tuple[tuple[str, str], ...] = ()

@dataclass(frozen=True)
class Consolidated:
    representatives: tuple[Evidence, ...]
    members: dict[str, tuple[str, ...]]


def semantic_key(e: Evidence) -> tuple[str | bool | None, ...]:
    f = e.fact
    return (e.source.scope, f.subject, f.predicate, f.object, f.object_entity,
        f.valid_from, f.valid_to, e.source.expires_at, f.standing)


def consolidate(evidence: tuple[Evidence, ...]) -> Consolidated:
    groups: dict[tuple[str | bool | None, ...], list[Evidence]] = {}
    for e in evidence:
        groups.setdefault(semantic_key(e), []).append(e)
    representatives = tuple(min(group, key=lambda e: e.id) for group in groups.values())
    members = {min(group, key=lambda e: e.id).id: tuple(sorted(e.id for e in group)) for group in groups.values()}
    return Consolidated(representatives, members)
