<script lang="ts">
  // Why: DOC-39 §4.5 (issue #3153 shell rebuild) leaves goal-slot placement
  // to "engineer's judgment... appears in no mockup" — Memory or Workstream
  // pane. This structural rebuild pass does not wire the 5 goal slots
  // (`session.get_goals`/`set_goal`/`clear_goal`) on EITHER tab: even
  // though the brief calls that "pure UI wiring" against already-shipped
  // APIs, it is a materially separate, independently-testable feature
  // (5-slot cardinality, Model-vs-Operator source styling, in-place edit)
  // that deserves its own slice rather than a rushed addition to a shell
  // restructure. Recorded here as a stub, not silently dropped.
  // What: A Foundry-style empty state naming the deferred goal-slot surface
  // and the memory-palace stats gap (no daemon aggregation route yet,
  // §7B/#3181).
  // Test: none — static content, nothing branchy to unit-test.
</script>

<div class="flex flex-col items-center justify-center gap-2 py-16 text-center">
  <span class="font-display text-2xl text-trusty-text-muted">◇</span>
  <p class="font-mono text-xs font-semibold uppercase tracking-wide text-trusty-text-muted">
    memory
  </p>
  <p class="max-w-sm text-xs text-trusty-text-muted">
    Goal slots (DOC-39 §4.5, pure UI wiring over shipped APIs) and memory-palace
    stats (needs #3181's service-status aggregation) are a follow-up slice.
  </p>
</div>
