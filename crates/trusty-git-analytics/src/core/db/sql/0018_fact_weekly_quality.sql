-- Migration v18: fact_weekly_quality — persist per-engineer-per-week quality
-- scores to the database so downstream warehouses can join on them without
-- re-running the aggregator (issue #445, batch B).
--
-- Grain: one row per (author_email, iso_year, iso_week, repository).
-- Quality is recomputed by the aggregator each time a report runs and
-- UPSERTed here so the table always reflects the corrected ticketed logic
-- introduced by batch A (migration v17).
--
-- All additive — no existing columns are modified, no data is removed.

CREATE TABLE IF NOT EXISTS fact_weekly_quality (
    -- Author identity key (canonical email, matching commits.author_email /
    -- authors.canonical_email). Stable across renames.
    author_email     TEXT    NOT NULL,
    -- ISO-8601 year component of the week label (YYYY).
    iso_year         INTEGER NOT NULL,
    -- ISO-8601 week number (1–53).
    iso_week         INTEGER NOT NULL,
    -- Repository scope — matches commits.repository. Included so per-repo
    -- quality aggregations are possible downstream without a separate join.
    repository       TEXT    NOT NULL,
    -- Composite quality score in [0.0, 1.0] (higher is better).
    -- Formula: 0.35*(1-revert_rate) + 0.40*(1-bugfix_rate) + 0.25*ticket_rate.
    quality_score    REAL    NOT NULL,
    -- T-shirt bucket 1–5 derived from quality_score (5 = best).
    quality_tshirt   INTEGER NOT NULL,
    -- Raw input counts that feed the formula (auditable by downstream).
    revert_count     INTEGER NOT NULL DEFAULT 0,
    bugfix_count     INTEGER NOT NULL DEFAULT 0,
    ticketed_count   INTEGER NOT NULL DEFAULT 0,
    commit_count     INTEGER NOT NULL DEFAULT 0,
    -- Formula version string ("v1") so consumers can detect future changes.
    formula_version  TEXT    NOT NULL DEFAULT 'v1',
    -- Unix timestamp (seconds) when this row was last computed.
    computed_at      INTEGER NOT NULL DEFAULT 0,

    -- Grain uniqueness: one row per (author, year, week, repo).
    -- ON CONFLICT REPLACE enables UPSERT semantics at the application layer
    -- via INSERT OR REPLACE.
    PRIMARY KEY (author_email, iso_year, iso_week, repository)
);

-- Index to support weekly slices (by week label) for reporting queries.
CREATE INDEX IF NOT EXISTS idx_fwq_week
    ON fact_weekly_quality (iso_year, iso_week);

-- Index to support per-author longitudinal queries.
CREATE INDEX IF NOT EXISTS idx_fwq_author
    ON fact_weekly_quality (author_email);

-- Index to support per-repo quality filtering.
CREATE INDEX IF NOT EXISTS idx_fwq_repo
    ON fact_weekly_quality (repository);
