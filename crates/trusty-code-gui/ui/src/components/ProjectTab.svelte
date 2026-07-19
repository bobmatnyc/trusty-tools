<script lang="ts">
  // Why: DOC-39 §7C (issue #3153 shell rebuild) describes a read-only
  // project-info panel — inferred stack tags, CLAUDE.md/AGENTS.md
  // instruction summaries, service status — but every one of its data
  // sources is itself a MISSING API (stack detection #3182, daemon-side
  // summarization #3184, `project.status` aggregation #3181). Per DOC-39
  // §2.1 C-2 ("a UI need with no API is an unbuilt feature, never a UI
  // workaround"), this tab renders an honest not-yet-built notice rather
  // than a client-side stack-detection/markdown-parsing substitute.
  // What: A Foundry-style empty state (muted mark + mono label + one line
  // of sans guidance) naming the exact gap and the tracking issues.
  // Test: none — static content, nothing branchy to unit-test.
</script>

<div class="flex flex-col items-center justify-center gap-2 py-16 text-center">
  <span class="font-display text-2xl text-trusty-text-muted">◇</span>
  <p class="font-mono text-xs font-semibold uppercase tracking-wide text-trusty-text-muted">
    project
  </p>
  <p class="max-w-sm text-xs text-trusty-text-muted">
    The project context panel (stack tags, instruction summaries, service status —
    DOC-39 §7C) needs daemon APIs that don't exist yet (#3181/#3182/#3184).
  </p>
</div>
