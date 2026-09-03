-- Migration v28: ai_detector_version column for issue #6748.
--
-- `tga collect` classifies a commit once, at walk time, and persists the
-- verdict. Nothing re-reads a stored row when the marker set changes, so every
-- commit ingested before a detector fix keeps the old verdict forever — the
-- failure reported in duettoresearch/cto-reports#140, where ~700 commits
-- carrying a literal AI trailer are stored with is_ai_assisted = 0.
--
-- This column records which detector generation produced the stored verdict
-- (see `collect::ai_markers::DETECTOR_VERSION`). The DEFAULT of 0 is the point
-- of the migration: SQLite backfills every existing row with it, and 0 is lower
-- than any shipped generation, so an existing database is read as entirely
-- stale and re-classified on the next `tga collect`.
--
-- Additive only — no existing column is modified and no data is removed.
--
-- Downstream contract for cto-reports: `fact_commits` need not carry this
-- column. It is bookkeeping for the re-classification pass, not a metric.

ALTER TABLE commits ADD COLUMN ai_detector_version INTEGER NOT NULL DEFAULT 0;

-- The re-classification pass scans `WHERE ai_detector_version < ?` on every
-- collect. On a corpus of hundreds of thousands of commits that is a full table
-- scan per run once the backfill has settled; the index makes the settled case
-- a no-op lookup.
CREATE INDEX IF NOT EXISTS idx_commits_ai_detector_version
    ON commits(ai_detector_version);
