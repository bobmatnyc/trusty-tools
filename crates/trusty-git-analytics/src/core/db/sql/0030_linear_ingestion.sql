-- Migration v30: Linear bulk sync ingestion (issue #7139).
--
-- `linear_issues` (migration v2) previously carried no lifecycle timestamps
-- and was populated only by the per-commit-reference lookup
-- (`linear.fetch_on_reference`), so a board with no JIRA integration had no
-- path to a full team issue set and no way to compute lead-time-from-ticket.
--
-- This migration is additive only:
--   * Five nullable TEXT (RFC3339) columns land on `linear_issues`, mirroring
--     the fields Linear's GraphQL API exposes on an issue: `created_at`,
--     `updated_at`, `started_at`, `completed_at`, `canceled_at`. `created_at`
--     paired with `completed_at`/`canceled_at` is what makes
--     lead-time-from-ticket computable for a Linear-only engagement, the way
--     `fact_ticket_transitions` timestamps make it computable for JIRA.
--   * `linear_sync_cursor` records incremental-sync bookkeeping per team key,
--     mirroring `jira_sync_cursor` (migration v23): the `updatedAt >=` cursor
--     for the next incremental run, and the wall-clock time of the last
--     successful run — the latter is the freshness signal `tga linear
--     freshness` reads, independent of `linear_issues.fetched_at`, which is
--     also written by the unrelated per-commit-reference lookup and so cannot
--     tell "the bulk sync ran" apart from "a commit happened to mention a
--     ticket".
--
-- No existing column is modified and no data is removed.

ALTER TABLE linear_issues ADD COLUMN created_at TEXT;
ALTER TABLE linear_issues ADD COLUMN updated_at TEXT;
ALTER TABLE linear_issues ADD COLUMN started_at TEXT;
ALTER TABLE linear_issues ADD COLUMN completed_at TEXT;
ALTER TABLE linear_issues ADD COLUMN canceled_at TEXT;

CREATE TABLE IF NOT EXISTS linear_sync_cursor (
    team_key       TEXT PRIMARY KEY,
    last_synced_at TEXT NOT NULL,    -- RFC3339: `updatedAt >=` cursor for the next incremental run
    last_run_at    TEXT NOT NULL,    -- RFC3339: wall-clock time of the last successful sync invocation
    issues_synced  INTEGER NOT NULL DEFAULT 0
);
