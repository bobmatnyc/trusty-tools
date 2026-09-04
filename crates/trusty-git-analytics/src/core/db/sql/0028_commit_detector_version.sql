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

-- The re-classification pass runs
--   WHERE ai_detector_version < ? ORDER BY ai_detector_version, id LIMIT ?
-- on every collect. The index is COMPOSITE, and its column order is the query's
-- ORDER BY, so one index range walk satisfies both the filter and the ordering:
-- `SEARCH commits USING INDEX idx_commits_ai_detector_version
-- (ai_detector_version<?)` with no temp b-tree and no table scan.
--
-- A single-column index on `ai_detector_version` alone is NOT enough. ANALYZE
-- records `200000 200000` for it — one distinct value across the whole table —
-- so the planner reads an index lookup as returning every row, and whether it
-- picks the index or a rowid scan is a coin-flip that varies by SQLite build.
-- With `ORDER BY id` it must then sort, which is a temp b-tree over the whole
-- table. The query pins this index with `INDEXED BY` for the same reason: it
-- turns "the planner will probably use it" into a prepare-time error if it
-- cannot, so the settled-corpus cost cannot silently regress to a full scan.
-- Test: `collect::reclassify::tests::the_scan_uses_the_index_on_a_populated_database`.
--
-- DROP first: an earlier build of this branch created the same index name over
-- one column, and a database that already recorded migration 28 would never
-- re-run this file.
DROP INDEX IF EXISTS idx_commits_ai_detector_version;
CREATE INDEX idx_commits_ai_detector_version
    ON commits(ai_detector_version, id);
