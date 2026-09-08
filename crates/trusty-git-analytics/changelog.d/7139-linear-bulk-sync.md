Added

- `tga linear sync` and `tga linear freshness`: bulk-ingest a Linear team's
  full issue set (paginated, incremental by cursor) into `linear_issues` and
  the source-agnostic `work_items` corpus, and report the last successful
  sync per team. Previously Linear issues were only ever resolved one at a
  time, from a commit message reference — an engagement registered with only
  a Linear board had no path to ticket-linked metrics.
- `LinearIssue` (and the `linear_issues` table) now carry `created_at`,
  `updated_at`, `started_at`, `completed_at`, and `canceled_at`, making
  lead-time-from-ticket computable for a Linear-only engagement.
- `tga audit`'s one-shot sweep now runs `linear sync` alongside `jira sync`,
  so an engagement registered with only `[boards.linear]` produces
  ticket-linked metrics from the standard sweep, not only a separate manual
  `tga linear sync` invocation.
- The bulk sync's per-page reads now retry a 429/503 with backoff (reusing
  the same retry budget `tga jira sync` uses) instead of discarding an
  in-progress backfill, and the page walk is bounded so a server that never
  stops paginating cannot hang it. `store_linear_issues` now writes a page's
  rows in one transaction instead of one autocommit per row.
