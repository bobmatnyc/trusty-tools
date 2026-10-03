Fixed

- `disk_survey` no longer answers a fleet too large for its 55-second clamp
  with the same partial pass on every call. A budgeted pass that runs out of
  budget starts one unbudgeted background pass; later budgeted calls get the
  last complete pass at once, with per-worktree byte counts. A call that
  omits `budget_seconds` always runs a live, unbounded survey. Every response
  carries `freshness` (`live`, `cached` or `partial`), `age_seconds` and
  `background_pass`, and a budgeted pass waits for the shared size index no
  longer than the time it has left.
