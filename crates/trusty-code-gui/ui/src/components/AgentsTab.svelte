<script lang="ts">
  // Why: DOC-39 §4.5 (issue #3153 shell rebuild) describes per-agent todo
  // checklists read from the agent roster (`session.get_agents`, §5.4 —
  // shipped, eviction-safe) and 10b's model badge (deferred, #3255). The
  // roster endpoint exists but requires an existing `session_id`, and
  // rendering it meaningfully (per-agent todos, live/settled monitor
  // lifecycle, §4.6) is a materially separate slice from this structural
  // shell rebuild — recorded here as a stub rather than a half-wired
  // roster fetch with no monitor lifecycle behind it.
  // What: A Foundry-style empty state naming the gap and its tracking issue.
  // Test: none — static content, nothing branchy to unit-test.
</script>

<div class="flex flex-col items-center justify-center gap-2 py-16 text-center">
  <span class="font-display text-2xl text-trusty-text-muted">◇</span>
  <p class="font-mono text-xs font-semibold uppercase tracking-wide text-trusty-text-muted">
    agents
  </p>
  <p class="max-w-sm text-xs text-trusty-text-muted">
    Per-agent roster + todo checklists (DOC-39 §4.5/§5.4) and the model badge (#3255)
    land in a follow-up slice — not part of this structural shell rebuild.
  </p>
</div>
