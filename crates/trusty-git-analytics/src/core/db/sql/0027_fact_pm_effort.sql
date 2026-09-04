-- Migration v27: PM effort tier — complexity score per meaningful ticket
-- (issue #3915).
--
-- The Activity / Work / Effort model (epic #3914) realizes its EFFORT tier
-- for PM tickets here. `fact_pm_work` (migration v26) answered "is this
-- ticket real management labor?"; `fact_pm_effort` answers "how much
-- complexity did producing it require?". `fact_commit_effort` (migration
-- v16) is the engineering counterpart.
--
-- SCALE SEPARATION: `effort_score` here runs 1.0–50.0 and is counted in PM
-- complexity points; `fact_commit_effort.effort_score` runs 2.5–45 and is
-- counted in commit effort points. The overlapping numeric ranges are a
-- coincidence of scaling, not a shared unit — the two must never share a
-- visualization axis. See #3917.
--
-- Design decisions:
--   * Grain and key: (work_item_id, work_item_source), matching
--     `work_items(id, source)` and `fact_pm_work`. Ticket IDs are unique per
--     PM source, not globally (see migration v5).
--   * FOREIGN KEY back to `work_items`, same reasoning as v26: every row is
--     DERIVED from a `work_items` row.
--   * ONLY tickets that `fact_pm_work` marks `is_meaningful = 1` are scored.
--     An excluded ticket gets NO row here at all — scoring boilerplate would
--     re-admit through the effort tier exactly what the work tier removed.
--     The backfill deletes rows whose ticket has since become non-meaningful,
--     so the meaningfulness gate cannot go stale.
--   * `effort_score` and `effort_bucket` are NULLABLE and `score_status`
--     says why. A ticket inside the recency floor is recorded as
--     'DEFERRED_RECENT' with a NULL score, never as a zero: issue #3915's
--     Rohit case is three epics authored the same day that had no children
--     yet, and a zero would have read as "no complexity" rather than "too
--     early to tell".
--   * The inputs the score was computed from are stored alongside it
--     (`epic_children_count`, `description_word_count`, `comment_count`,
--     `transition_count`, `story_points`) so a retune can be evaluated
--     against stored rows without re-reading upstream payloads.
--   * `story_points` is NULLABLE and carries no weight when absent. It is
--     76% NULL across four per-project custom-field IDs on the source JIRA
--     instance (see migration v23 and `core::pm_effort::extract`), so a row
--     with a NULL here is the common case, not a defect. `inputs_present`
--     names which inputs actually contributed, so a consumer can compare
--     like with like instead of assuming every score used the same signals.
--   * `formula_version` names the weight set that produced the score
--     ('pm-effort-1' for v1). A retune is a NEW version string, never an
--     edit of the v1 constants — see `core::pm_effort::thresholds`.
--   * No `formula_version` in the primary key: the table holds the CURRENT
--     score per ticket, and re-running the scorer replaces a row rather than
--     accumulating one per version. Same choice as v16 and v26.
--   * `computed_at` is a Unix timestamp (integer seconds), matching v16/v26.
--
-- Additive migration: no existing table is modified.

CREATE TABLE IF NOT EXISTS fact_pm_effort (
    work_item_id          TEXT NOT NULL,
    work_item_source      TEXT NOT NULL,     -- 'azdo' | 'jira' | 'github' | 'linear'
    pm_name               TEXT,              -- reporter display name; NULL when the payload has none
    week_key              TEXT,              -- ISO week the ticket was created, 'YYYY-Www'; NULL when unknown
    effort_score          REAL,              -- 1.0–50.0; NULL when score_status <> 'SCORED'
    effort_bucket         TEXT,              -- 'LOW' | 'MEDIUM' | 'HIGH'; NULL when not scored
    score_status          TEXT NOT NULL,     -- 'SCORED' | 'DEFERRED_RECENT'
    epic_children_count   INTEGER NOT NULL,  -- work_items naming this ticket as parent
    description_word_count INTEGER NOT NULL, -- words of plain-text description
    comment_count         INTEGER NOT NULL,  -- fact_jira_comment_detail rows
    transition_count      INTEGER NOT NULL,  -- fact_ticket_transitions rows
    story_points          REAL,              -- NULL when absent or outside the plausible range
    inputs_present        TEXT NOT NULL,     -- comma-separated contributing inputs, or 'NONE'
    age_days_at_score     INTEGER,           -- ticket age in days when scored; NULL when the creation date is unknown
    formula_version       TEXT NOT NULL DEFAULT 'pm-effort-1',
    computed_at           INTEGER NOT NULL,  -- unix timestamp (seconds)
    PRIMARY KEY (work_item_id, work_item_source),
    FOREIGN KEY (work_item_id, work_item_source) REFERENCES work_items(id, source)
);

CREATE INDEX IF NOT EXISTS idx_fact_pm_effort_bucket
    ON fact_pm_effort(effort_bucket);
CREATE INDEX IF NOT EXISTS idx_fact_pm_effort_status
    ON fact_pm_effort(score_status);
CREATE INDEX IF NOT EXISTS idx_fact_pm_effort_pm_week
    ON fact_pm_effort(pm_name, week_key);
