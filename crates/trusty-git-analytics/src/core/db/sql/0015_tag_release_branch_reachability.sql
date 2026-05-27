-- Migration v15: tag and release-branch reachability (issue #279).
--
-- Adds `fact_commit_reachability` and extends it with the four tag/branch
-- columns requested by the issue. Using a separate fact table (not a
-- column on `commits`) keeps the schema additive: existing rows in `commits`
-- do not need an ALTER TABLE and the reachability pass can be re-run
-- independently of commit collection.
--
-- Additive migration:
--   * The table is created with sensible defaults so rows already implied
--     by the commit table (i.e. rows that would exist if reachability had
--     been computed for every stored commit) can simply be absent — the
--     JOIN in downstream queries is a LEFT JOIN.
--   * No backfill is performed here; `tga collect` populates the table
--     going forward.

CREATE TABLE IF NOT EXISTS fact_commit_reachability (
    -- FK into `commits.sha`.  Use TEXT because `commits.sha` is TEXT NOT NULL UNIQUE.
    commit_sha          TEXT PRIMARY KEY,

    -- Whether the commit is reachable from the repository's default branch
    -- (main / master).  Pre-existing semantics from before v15.
    on_default_branch   INTEGER NOT NULL DEFAULT 0,

    -- New in v15: tag reachability.
    -- `on_any_tag`:        true (1) if any git tag reaches this commit.
    -- `reachable_from_tags`: JSON array of tag names, e.g. '["v1.0","hotfix-3"]'.
    on_any_tag          INTEGER NOT NULL DEFAULT 0,
    reachable_from_tags TEXT    NOT NULL DEFAULT '[]',

    -- New in v15: release-branch reachability.
    -- `on_release_branch`:  true (1) if the commit is on at least one branch
    --                       matching a configured release-branch pattern.
    -- `release_branches`:   JSON array of matching branch names.
    on_release_branch   INTEGER NOT NULL DEFAULT 0,
    release_branches    TEXT    NOT NULL DEFAULT '[]',

    FOREIGN KEY(commit_sha) REFERENCES commits(sha) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_fcr_on_any_tag
    ON fact_commit_reachability(on_any_tag);
CREATE INDEX IF NOT EXISTS idx_fcr_on_release_branch
    ON fact_commit_reachability(on_release_branch);
CREATE INDEX IF NOT EXISTS idx_fcr_on_default_branch
    ON fact_commit_reachability(on_default_branch);
