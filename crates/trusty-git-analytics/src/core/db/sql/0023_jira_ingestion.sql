-- Migration v23: JIRA ingestion ownership (issue #3966).
--
-- TGA becomes the owner of JIRA status-transition and comment ingestion,
-- upstream of the cto-reports fact tables (supersedes cto-reports#404/#405).
-- Two new fact tables land here:
--
--   fact_ticket_transitions  -- one row per JIRA changelog status transition
--   fact_jira_comment_detail -- one row per JIRA comment
--
-- Field-coverage constraints observed on the source JIRA instance
-- (documented here so downstream consumers do not have to rediscover them
-- by querying — see issue #3966 "Field-Coverage Constraints"):
--   * `epic_key` is NOT modeled in either table below. Decomposition on this
--     instance is via `parent_key`, not epics — `epic_key` is 100% NULL at
--     the source. Any future ticket-dimension/snapshot table should carry
--     `parent_key`, not `epic_key`.
--   * `acceptance_criteria` (customfield_10017) is 0% populated at the
--     source and is therefore NOT modeled anywhere in this migration.
--   * `story_points` is a ticket-level field (not a transition/comment-level
--     fact), so it is out of scope for these two grains entirely. It is
--     76% NULL across 4 different custom-field IDs on this instance — see
--     the doc comment on `JiraClient::get_story_point_field` in
--     `collect/jira/client.rs`, which discovers only ONE field by name; a
--     future ticket-snapshot table populating story points must account for
--     per-project custom-field-id drift, not a single global lookup.
--
-- Design decisions:
--   * `fact_ticket_transitions` grain: one row per
--     (ticket_key, transitioned_at, to_status) rather than
--     (ticket_key, to_status) — a ticket can revisit the same `to_status`
--     multiple times (e.g. reopened tickets), so the transition timestamp
--     must be part of the key to avoid collapsing repeat transitions.
--   * `fact_jira_comment_detail` grain: one row per (ticket_key, comment_id).
--     JIRA comment IDs are unique per-instance already, but scoping the key
--     by ticket_key keeps lookups cheap and mirrors the transitions table's
--     shape.
--   * Both tables carry `project_key` denormalized for cheap filtering
--     without a join back to a ticket dimension (mirrors `repository` on
--     `fact_commit_effort`, see migration v16).
--   * `synced_at` (unix seconds) on both tables is the freshness-assertion
--     signal (`tga jira freshness`) — freshness is measured against
--     MAX(synced_at), NOT MAX(transitioned_at)/MAX(created_at), because a
--     historical backfill would otherwise make a *stale, no-longer-running*
--     sync look "fresh" forever. This is exactly the failure mode called
--     out in the issue as the "Effort-Scoring Cron Gap" precedent.
--   * No FOREIGN KEY back to `work_items` — the JIRA sync path is
--     independent of (and upstream of) the commit-ticket linkage tables, and
--     may ingest tickets that no commit ever references.
--
-- Additive migration: no existing table is modified.

CREATE TABLE IF NOT EXISTS fact_ticket_transitions (
    ticket_key      TEXT NOT NULL,
    project_key     TEXT NOT NULL,
    from_status     TEXT,             -- NULL for a ticket's initial creation state
    to_status       TEXT NOT NULL,
    transitioned_at TEXT NOT NULL,    -- RFC3339 timestamp of the transition
    author          TEXT,             -- NULL when JIRA reports no changelog author
    synced_at       INTEGER NOT NULL, -- unix seconds: when tga wrote this row
    PRIMARY KEY (ticket_key, transitioned_at, to_status)
);

CREATE INDEX IF NOT EXISTS idx_fact_ticket_transitions_project
    ON fact_ticket_transitions(project_key);
CREATE INDEX IF NOT EXISTS idx_fact_ticket_transitions_synced_at
    ON fact_ticket_transitions(synced_at);
CREATE INDEX IF NOT EXISTS idx_fact_ticket_transitions_ticket
    ON fact_ticket_transitions(ticket_key);

CREATE TABLE IF NOT EXISTS fact_jira_comment_detail (
    ticket_key   TEXT NOT NULL,
    comment_id   TEXT NOT NULL,
    project_key  TEXT NOT NULL,
    author       TEXT,              -- NULL when JIRA reports no comment author
    created_at   TEXT NOT NULL,     -- RFC3339 timestamp
    body_len     INTEGER NOT NULL,  -- length of the comment body; see client.rs
                                     -- doc comment for the ADF-vs-plain-text caveat
    synced_at    INTEGER NOT NULL,  -- unix seconds: when tga wrote this row
    PRIMARY KEY (ticket_key, comment_id)
);

CREATE INDEX IF NOT EXISTS idx_fact_jira_comment_detail_project
    ON fact_jira_comment_detail(project_key);
CREATE INDEX IF NOT EXISTS idx_fact_jira_comment_detail_synced_at
    ON fact_jira_comment_detail(synced_at);

-- Incremental-sync cursor bookkeeping, keyed by project key (mirrors the
-- `collection_runs` pattern used by git collection). A full `--backfill`
-- run still updates this row on success so the next incremental run has a
-- correct starting point.
CREATE TABLE IF NOT EXISTS jira_sync_cursor (
    project_key    TEXT PRIMARY KEY,
    last_synced_at TEXT NOT NULL,    -- RFC3339: JQL `updated >=` cursor for the next incremental run
    last_run_at    TEXT NOT NULL,    -- RFC3339: wall-clock time of the last successful sync invocation
    tickets_synced INTEGER NOT NULL DEFAULT 0
);
