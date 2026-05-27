-- Migration v16: per-commit empirical effort scores (tga backfill effort).
--
-- Adds `fact_commit_effort` to persist the v1 effort formula results:
--   score = α·log₂(LoC+1) + β·log₂(files+1) + δ·tests_factor
--   v1 coefficients: α=1.0, β=1.5, γ=0.0 (deferred), δ=1.0
--
-- Design decisions:
--   * Separate fact table — `commits` is not altered, so the backfill can be
--     re-run independently and at any cadence.
--   * PRIMARY KEY (sha, repository) — same commit SHA can appear in multiple
--     repos in a multi-repo workspace (fork/mirror scenarios).
--   * `formula_version` anticipates a v2 that adds cyclomatic complexity (γ
--     term). Re-running the backfill with formula_version='v2' will insert new
--     rows alongside existing v1 rows, enabling score-evolution tracking.
--   * No FOREIGN KEY on `commits.sha` — the effort table may be populated
--     before (or independently of) the commit collection phase.
--   * `computed_at` is a Unix timestamp (integer seconds) for easy arithmetic.
--
-- Additive migration: no existing table is modified.

CREATE TABLE IF NOT EXISTS fact_commit_effort (
    sha             TEXT NOT NULL,
    repository      TEXT NOT NULL,
    size            TEXT NOT NULL,           -- 'XS' | 'S' | 'M' | 'L' | 'XL'
    score           REAL NOT NULL,
    loc             INTEGER NOT NULL,        -- insertions + deletions
    files           INTEGER NOT NULL,
    test_loc        INTEGER NOT NULL,
    tests_factor    REAL NOT NULL,
    formula_version TEXT NOT NULL DEFAULT 'v1',
    computed_at     INTEGER NOT NULL,        -- unix timestamp (seconds)
    PRIMARY KEY (sha, repository)
);

CREATE INDEX IF NOT EXISTS idx_fact_commit_effort_size
    ON fact_commit_effort(size);
CREATE INDEX IF NOT EXISTS idx_fact_commit_effort_repo
    ON fact_commit_effort(repository);
CREATE INDEX IF NOT EXISTS idx_fact_commit_effort_score
    ON fact_commit_effort(score);
