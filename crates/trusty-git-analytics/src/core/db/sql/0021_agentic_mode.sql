-- =============================================================================
-- DOCUMENTATION / REFERENCE ONLY — this file is NOT executed by the migration
-- runner. The live migration is implemented in migrations/v21.rs (which uses
-- a PRAGMA table_info guard because SQLite has no ALTER TABLE … ADD COLUMN IF
-- NOT EXISTS). Keeping this file preserves a human-readable schema reference
-- and diff history alongside the other versioned SQL files.
-- =============================================================================

-- Migration v21: agentic_mode column for issue #1113.
--
-- Adds a canonical TEXT column `agentic_mode` to `commits` distinguishing
-- full-agentic (autonomous CLI) from IDE-assisted inline-completion commits
-- from plain human commits. Values: 'full_agentic' | 'ide_assisted' | 'none'.
--
-- Additive only — no existing columns (`is_ai_assisted`, `ai_tool`) are
-- modified and no data is removed.
--
-- Incremental-safe: INSERT OR IGNORE in the extractor means re-collecting
-- an already-seen commit SHA is a no-op; `tga backfill agentic-mode` can
-- UPDATE rows that pre-date this migration. The column defaults to 'none'
-- so existing rows that are never backfilled classify conservatively.
--
-- Downstream contract for cto-reports:
--   `tga_to_duckdb.py` must SELECT `agentic_mode` from `commits` and carry
--   it into `fact_commits.agentic_mode`. The column is TEXT NOT NULL with
--   a DEFAULT of 'none', so any existing NULL-coalesce logic is not needed.
--
-- Also adds per-engineer per-week agentic counts to `fact_weekly_engineer`
-- (new table created here, replacing the ad-hoc approach — grain matches
-- fact_weekly_quality). See issue #1113 for the full spec.

-- Step 1: agentic_mode column on commits.
ALTER TABLE commits ADD COLUMN agentic_mode TEXT NOT NULL DEFAULT 'none';
CREATE INDEX IF NOT EXISTS idx_commits_agentic_mode ON commits(agentic_mode);

-- Step 2: fact_weekly_engineer — weekly per-engineer agentic-% aggregation.
--
-- Grain: (author_email, iso_year, iso_week, repository) — same as
-- fact_weekly_quality so the two tables can be joined on the same key.
--
-- agentic_count: commits where agentic_mode = 'full_agentic' (net of reverts).
-- ide_assisted_count: commits where agentic_mode = 'ide_assisted' (net of reverts).
-- net_commits: total commits minus revert commits (matches the denominator used
--   by cto-reports for agentic %).
-- agentic_pct: agentic_count / net_commits * 100, or 0.0 when net_commits = 0.
--   Stored as REAL so downstream tools can filter on it directly.
-- formula_version: bump when the agentic_pct formula changes; currently 'v1'.
-- computed_at: Unix timestamp (seconds) of last aggregation run.
CREATE TABLE IF NOT EXISTS fact_weekly_engineer (
    author_email        TEXT    NOT NULL,
    iso_year            INTEGER NOT NULL,
    iso_week            INTEGER NOT NULL,
    repository          TEXT    NOT NULL,
    -- Commit counts (net of reverts).
    net_commits         INTEGER NOT NULL DEFAULT 0,
    agentic_count       INTEGER NOT NULL DEFAULT 0,
    ide_assisted_count  INTEGER NOT NULL DEFAULT 0,
    -- Derived percentage; 0.0 when net_commits = 0.
    agentic_pct         REAL    NOT NULL DEFAULT 0.0,
    -- Audit / schema versioning.
    formula_version     TEXT    NOT NULL DEFAULT 'v1',
    computed_at         INTEGER NOT NULL DEFAULT 0,
    -- Grain uniqueness; ON CONFLICT REPLACE enables UPSERT via INSERT OR REPLACE.
    PRIMARY KEY (author_email, iso_year, iso_week, repository)
);

CREATE INDEX IF NOT EXISTS idx_fwe_week
    ON fact_weekly_engineer (iso_year, iso_week);

CREATE INDEX IF NOT EXISTS idx_fwe_author
    ON fact_weekly_engineer (author_email);

CREATE INDEX IF NOT EXISTS idx_fwe_repo
    ON fact_weekly_engineer (repository);
