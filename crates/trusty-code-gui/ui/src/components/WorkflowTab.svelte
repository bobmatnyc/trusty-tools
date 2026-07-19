<script lang="ts">
  // Why: DOC-39 §4.9 (issue #3153 shell rebuild) scopes Workflow as a
  // brand-new, deferred subsystem (§5.7 workflow/delivery API is MISSING
  // entirely) — mockup 10e's lanes are explicitly DEFERRED by the build
  // brief. `ServiceNav.svelte`'s `tabVisibility` (AC-9.4) already hides this
  // tab whenever the workstream is projectless or not a git repo, so this
  // component only ever renders for a git-bound project — still nothing to
  // show yet, since the underlying subsystem doesn't exist.
  // What: A Foundry-style empty state naming the gap.
  // Test: none — static content; `nav-tabs.test.ts` covers the visibility
  // gate that decides whether this component ever mounts.
</script>

<div class="flex flex-col items-center justify-center gap-2 py-16 text-center">
  <span class="font-display text-2xl text-trusty-text-muted">◇</span>
  <p class="font-mono text-xs font-semibold uppercase tracking-wide text-trusty-text-muted">
    workflow
  </p>
  <p class="max-w-sm text-xs text-trusty-text-muted">
    Workflow/delivery guarding (DOC-39 §4.9/§5.7) is a new, not-yet-built subsystem —
    mockup 10e's lanes are explicitly deferred.
  </p>
</div>
