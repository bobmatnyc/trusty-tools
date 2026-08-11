# Project Instructions

## Code Search

- Start codebase discovery with `trusty-search` when the command is available.
  Use it first for conceptual questions, unfamiliar features, and relationship
  tracing (for example, `trusty-search search "<question>"`).
- If `trusty-search` is unavailable, unhealthy, or has no usable index for this
  checkout, fall back immediately to direct workspace search rather than
  blocking the task.
- Use `rg`, Git, and direct file inspection for exact identifiers, exhaustive
  matches, uncommitted changes, and final verification. Treat the current
  working tree—not the search index—as the source of truth before editing.
