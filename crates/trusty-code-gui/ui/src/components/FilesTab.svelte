<script lang="ts">
  // Why: DOC-39 §1.3 (issue #3153 shell rebuild) lists a full IDE as an
  // explicit non-goal (deferred to Q4); the build brief's DEFER list keeps
  // only the ⌘K literal-find affordance salvageable here for later. Neither
  // exists yet, so Files is a stub tab — reserved nav slot, no content.
  // What: A Foundry-style empty state naming the gap.
  // Test: none — static content, nothing branchy to unit-test.
</script>

<div class="flex flex-col items-center justify-center gap-2 py-16 text-center">
  <span class="font-display text-2xl text-trusty-text-muted">◇</span>
  <p class="font-mono text-xs font-semibold uppercase tracking-wide text-trusty-text-muted">
    files
  </p>
  <p class="max-w-sm text-xs text-trusty-text-muted">
    A full project IDE is an explicit DOC-39 §1.3 non-goal (Q4); ⌘K literal-find may
    salvage into this tab later. Nothing is built here yet.
  </p>
</div>
