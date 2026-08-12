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
  call. Nothing is dropped silently: whenever anything is withheld the response
  carries `truncated: true`, the `sessions_total` / `recent_commits_total` /
  `recent_memory_total` counts, and a `truncation_notice` naming the counts and
  the exact `sessions_offset` that retrieves the rest. A digest that already
  fits comes back byte-identical (#5557).
