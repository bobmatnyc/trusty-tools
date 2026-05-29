-- Migration v19: effort percentile threshold store
--
-- Why: corpus-percentile effort binning (#445 batch C) requires persisting
-- the computed p20/p40/p60/p80 breakpoints so incremental single-commit
-- ingestion bins against the last-known distribution rather than re-scanning
-- the whole corpus on every insert.
--
-- Design: a single-row-per-dataset table keyed on a `dataset` TEXT column
-- (value "default" for the main corpus). Additional datasets (e.g. per-repo)
-- can be added later without schema changes. The five REAL columns hold the
-- four breakpoints plus a cached p100 (max score) for context.
--
-- effort_tshirt band assignment (percentile-based):
--   score < p20  → 1 (bottom quintile)
--   p20 ≤ score < p40 → 2
--   p40 ≤ score < p60 → 3
--   p60 ≤ score < p80 → 4
--   score ≥ p80  → 5 (top quintile)
--
-- Note: the static XS/S/M/L/XL `size` TEXT label in fact_commit_effort
-- continues to use score thresholds (XS≤6, S≤10, M≤14, L≤18, XL>18) and
-- is NOT changed by this migration. The percentile-based `effort_tshirt`
-- integer intentionally diverges from the label's band to provide a
-- corpus-relative ranking independent of absolute score magnitude.
CREATE TABLE IF NOT EXISTS effort_percentile_thresholds (
    dataset       TEXT    NOT NULL DEFAULT 'default',
    p20           REAL    NOT NULL,
    p40           REAL    NOT NULL,
    p60           REAL    NOT NULL,
    p80           REAL    NOT NULL,
    sample_count  INTEGER NOT NULL,
    computed_at   INTEGER NOT NULL,  -- Unix epoch seconds
    PRIMARY KEY (dataset)
);
