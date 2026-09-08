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
