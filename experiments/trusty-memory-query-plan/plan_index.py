"""Attach task context to the unchanged eligible index without a global registry.

Why: #8246 requires scope/time mismatches to fail before retrieval.
What: Wrap existing index construction with a read-only context property.
Test: test_public_task_boundaries_reject_foreign_context.
"""
from dataclasses import dataclass
import plan_bridge
from contracts import Task
from legacy import Source, RustHelper, IntegrityError
from maintenance import DerivedStore
from relevance_index import RelevanceIndex, build_index

@dataclass
class ScopedIndex(RelevanceIndex):
    _task_context: tuple[str, str, str]

    @property
    def task_context(self) -> tuple[str, str, str]:
        return self._task_context

def build_scoped_index(sources: tuple[Source, ...], task: Task, store: DerivedStore,
                       helper: RustHelper) -> ScopedIndex:
    """Reuse index construction; the returned subtype alone owns the scratch cleanup."""
    base = build_index(sources, task, store, helper)
    return ScopedIndex(base.evidence, base.by_source, base.old, base.names, base.aliases,
        base.alias_evidence, base.postings, base.source_id, base.claim_id, base.fallback_id,
        base.missing_sources, base.generation, base.build_ns, base.scratch, base.native_build,
        (task.scope, task.as_of, task.knowledge_cutoff))

def validate_context(task: Task, index: RelevanceIndex) -> None:
    """Reject unknown or mismatched construction context before a public task call."""
    if not isinstance(index, ScopedIndex) or index.task_context != (task.scope, task.as_of, task.knowledge_cutoff):
        raise IntegrityError('task/index context mismatch or unknown construction context')
