"""Immutable request bindings for #8246; interface.md defines the frozen contract."""
from __future__ import annotations
from dataclasses import dataclass
from typing import Literal, Mapping
import plan_bridge
from contracts import CandidateSet, Selection
from legacy import IntegrityError

Arm = Literal['old_combined', 'defect_fixes', 'structured_plan']
ARMS: tuple[Arm, ...] = ('old_combined', 'defect_fixes', 'structured_plan')
Direction = Literal['out', 'in']
ParseStatus = Literal['ready', 'unsupported', 'unresolved_entity', 'ambiguous_entity']
ExecStatus = Literal['not_run', 'complete', 'no_evidence', 'partial', 'bounded']
Predicate = Literal['owned_by', 'maintained_by', 'located_at', 'access_code', 'rationale',
    'release_rule', 'release_prohibition', 'uses_channel', 'depends_on', 'uses',
    'escalates_to', 'contact_window', 'is_alias_for']

@dataclass(frozen=True)
class Span:
    start: int
    end: int
    text: str

    def __post_init__(self) -> None:
        if self.start < 0 or self.end <= self.start or self.end-self.start != len(self.text):
            raise IntegrityError('invalid prompt span')

@dataclass(frozen=True)
class EntityRef:
    span: Span
    targets: tuple[str, ...]
    alias_ids: tuple[str, ...]
    status: ParseStatus

@dataclass(frozen=True)
class Step:
    predicate: Predicate
    direction: Direction
    qualifier: str | None = None

@dataclass(frozen=True)
class Request:
    span: Span
    roots: tuple[EntityRef, ...]
    steps: tuple[Step, ...]
    excluded_targets: tuple[EntityRef, ...] = ()
    excluded_outputs: tuple[Predicate, ...] = ()
    enumerate_aliases: bool = False
    status: ParseStatus = 'ready'

    def __post_init__(self) -> None:
        if len(self.steps) > 2:
            raise IntegrityError('relation depth exceeds two')

@dataclass(frozen=True)
class Plan:
    requests: tuple[Request, ...]
    diagnostics: tuple[tuple[Span, str], ...]

@dataclass(frozen=True)
class Path:
    request_index: int
    root: str
    bindings: tuple[str, ...]
    endpoint: str
    members: tuple[str, ...]

@dataclass(frozen=True)
class Execution:
    paths: tuple[Path, ...]
    per_request: tuple[ExecStatus, ...]
    counters: Mapping[str, int]
    reasons: tuple[tuple[str, str], ...]

@dataclass(frozen=True)
class RunResult:
    plan: Plan
    execution: Execution
    candidates: CandidateSet
    selection: Selection
    provenance: Mapping[str, tuple[str, ...]]
