Fixed

- `session_context_catchup` no longer returns an unbounded response. On this
  repo `full: true` returned 112,096 characters — past what the harness could
  hand back to the calling model, so it spilled the body to a file and the
  session resuming from it had to read that instead. Measurement put `sessions`
  at 94.7% of the payload with a 5,961-byte median record, so the count is what
  grows, not any one field: the digest is now paged by whole records to a
  48,000-byte budget. `sessions_offset` (new, optional, defaults to 0) selects
  the page and the response's `sessions_next_offset` names the next one, so
  `full: true` still delivers every snapshot in history — one readable page per
  call. Pages are ordered with the caller's own sessions first, then newest
  first, so page 0 always carries the entry a resume reads. Nothing is dropped
  silently: whenever anything is withheld the response carries
  `truncated: true`, the `sessions_total` / `recent_commits_total` /
  `recent_memory_total` counts, and a `truncation_notice` naming the counts and
  the exact `sessions_offset` that retrieves the rest. The one page that can
  exceed the budget — a single record larger than a whole page, which ships
  intact rather than being cut mid-field — reports `over_budget: true` and
  `page_bytes`, since nothing was withheld there and `truncated` would
  otherwise read as healthy. The offset is positional into a list rebuilt from
  disk on each call, so a snapshot paused mid-walk can make a later page repeat
  a record; that is stated in the tool schema and in the notice rather than
  left to be discovered. A digest that already fits comes back byte-identical
  (#5557).
