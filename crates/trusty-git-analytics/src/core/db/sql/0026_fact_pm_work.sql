-- Migration v26: PM work tier — per-ticket meaningfulness verdicts (issue #3916).
--
-- The Activity / Work / Effort model (epic #3914) realizes its WORK tier for
-- PM tickets here: `fact_pm_work` records, per `work_items` row, whether that
-- ticket represents real management labor or is boilerplate that inflates a
-- raw activity count. `fact_commit_effort` (migration v16) is the engineering
-- counterpart.
--
-- SCALE SEPARATION: PM work rows must never share a visualization axis with
-- commit-effort rows — the two are counted in incommensurable units and
-- co-plotting them produces a chart that means nothing. See #3917.
--
-- Design decisions:
--   * Grain and key: (work_item_id, work_item_source), matching
--     `work_items(id, source)`. Ticket IDs are unique per PM source, not
--     globally, so the source must be part of the key (see migration v5).
--   * FOREIGN KEY back to `work_items` — unlike `fact_commit_effort`, every
--     row here is DERIVED from a `work_items` row, so a verdict for a ticket
--     the database has never seen is a bug, not a valid ordering.
--   * `is_meaningful` is stored as INTEGER 0/1 (SQLite has no BOOLEAN).
--   * `exclusion_reason` is an enum stored as text. 'NONE' rather than NULL
--     when a ticket is meaningful, so every row carries a verdict a consumer
--     can GROUP BY without a COALESCE.
--   * `formula_version` names the threshold set that produced the verdict
--     ('pm-work-1' for v1). A retune is a NEW version string, never an edit
--     of the v1 constants — see `core::pm_work`.
--   * No `formula_version` in the primary key: the table holds the CURRENT
--     verdict per ticket, and re-running the classifier replaces a row rather
--     than accumulating one row per version. Same choice as v16.
--   * `computed_at` is a Unix timestamp (integer seconds) for easy arithmetic,
--     matching `fact_commit_effort.computed_at`.
--
-- Additive migration: no existing table is modified.

CREATE TABLE IF NOT EXISTS fact_pm_work (
    work_item_id     TEXT NOT NULL,
    work_item_source TEXT NOT NULL,     -- 'azdo' | 'jira' | 'github' | 'linear'
    pm_name          TEXT,              -- reporter display name; NULL when the source payload has none
    week_key         TEXT,              -- ISO week the ticket was created, 'YYYY-Www'; NULL when unknown
    is_meaningful    INTEGER NOT NULL,  -- 0 | 1
    exclusion_reason TEXT NOT NULL,     -- 'NONE' | 'TERSE_TITLE' | 'AUTO_GENERATED' | 'BOT_FILED'
    title_word_count INTEGER NOT NULL,
    body_word_count  INTEGER NOT NULL,
    formula_version  TEXT NOT NULL DEFAULT 'pm-work-1',
    computed_at      INTEGER NOT NULL,  -- unix timestamp (seconds)
    PRIMARY KEY (work_item_id, work_item_source),
    FOREIGN KEY (work_item_id, work_item_source) REFERENCES work_items(id, source)
);

CREATE INDEX IF NOT EXISTS idx_fact_pm_work_meaningful
    ON fact_pm_work(is_meaningful);
CREATE INDEX IF NOT EXISTS idx_fact_pm_work_reason
    ON fact_pm_work(exclusion_reason);
CREATE INDEX IF NOT EXISTS idx_fact_pm_work_pm_week
    ON fact_pm_work(pm_name, week_key);
