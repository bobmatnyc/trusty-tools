-- Migration v25: per-repository full-history walk bookkeeping (issue #6073).
--
-- The fully-unbounded collect path (no `since_date`, no `--weeks`) has no
-- week-level bookkeeping, so every re-run re-walks the entire history. A
-- render-stage failure in trusty-audit's nine-stage sweep therefore paid a
-- full re-collect: the issue measured this against a 594 MB extract database.
--
-- One row per repository records the tip the last COMPLETED full walk reached.
-- `head_sha` is the base a later incremental walk hides; `tips_digest` is a
-- digest over every `refs/heads/*` and `refs/remotes/*` (name, oid) pair, so a
-- side branch moving without HEAD moving still forces a walk rather than a
-- silent skip. `walk_complete` is written 0 before the walk and 1 after it
-- succeeds, so an interrupted run falls back to a full re-walk.
--
-- `walk_scope` records what the walk was ALLOWED to see: the `--branch` /
-- per-repo `branch:` filter, `--head-only`, and `skip_merges`. None of those
-- appears in `tips_digest`, which digests every ref whatever the walk covered,
-- so without this column a `--branch main` run would record a tip over refs it
-- never walked and the next full-scope run would skip — leaving every
-- side-branch commit permanently absent. A run whose scope differs from the
-- recorded one walks in full.
--
-- Additive only. An older database has no rows here after the migration runs,
-- which reads as "never walked" and produces the pre-#6073 full walk.
CREATE TABLE IF NOT EXISTS repo_walk_state (
    repository    TEXT    PRIMARY KEY,
    head_sha      TEXT    NOT NULL,
    head_ref      TEXT    NOT NULL,
    tips_digest   TEXT    NOT NULL,
    walk_scope    TEXT    NOT NULL DEFAULT '',
    walk_complete INTEGER NOT NULL DEFAULT 0,
    walked_at     TEXT    NOT NULL
);
