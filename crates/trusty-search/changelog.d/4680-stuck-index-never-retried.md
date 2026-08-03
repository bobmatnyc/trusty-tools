Fixed

- **An index that was never populated was recorded as needing nothing, forever
  — 221 of 222 production indexes served zero results for 44 days while
  `/health` reported fully healthy (issue
  [#4680](https://github.com/bobmatnyc/trusty-tools/issues/4680)).** Boot
  reconcile only ever asked "has the source drifted?", never "does this index
  hold any data at all?". Both of its staleness markers answer "no drift" for
  an empty index: the git path compares a HEAD SHA that restore re-derives from
  live git (so `stored == current` unconditionally — issue
  [#4391](https://github.com/bobmatnyc/trusty-tools/issues/4391)) and counted
  the index `up_to_date`; the mtime path reads `last_indexed_unix`, whose
  writer has no production callers, got `None`, and counted the index
  `skipped_no_data`. Neither branch ever re-drove the walk, on that boot or any
  later one.
  - `reconcile_one_index` now checks, *before* consulting either marker,
    whether an index claims lexical work is underway (`lexical: in_progress`,
    which the warm-boot classifier stamps for every restored empty corpus)
    while no walk has ever been driven for it in this daemon's lifetime. Such
    an index is stuck, not current, and gets a full **non-force** background
    reindex — the incremental hash cache and staged-corpus carryover both
    apply, so recovery re-walks and re-adds and never clears or rebuilds a
    corpus from scratch.
  - The retry is bounded to at most one walk per index per daemon lifetime (it
    keys off `last_walk_started_at`), so a walk that legitimately finds zero
    indexable files — everything gitignored or filtered out — is never
    re-driven in a loop. "Walk found nothing" and "walk never completed" are
    now distinguishable rather than both presenting as `chunk_count: 0`.
  - New `boot_reconcile.stuck_retried` counter on `GET /health` reports how
    many indexes were recovered this way, instead of the recovery hiding inside
    `up_to_date` / `skipped_no_data`.
- **`GET /health` reported `status: "ok"` while indexes were stuck at zero
  chunks (issue [#4680](https://github.com/bobmatnyc/trusty-tools/issues/4680)).**
  Every existing signal was structurally blind to this: `indexes` and
  `warmboot_summary.indexes_loaded` count registered index *slots*, not
  populated ones, and `indexes_corpus_failed` keys off `stages.any_failed()` —
  which a stuck index never trips, because it reports no failed lane at all,
  only an indefinite, false `"walking"`. A new `indexes_stuck_empty` field
  counts registered indexes whose lexical stage claims a walk is underway that
  has never been driven, and a non-zero count forces the top-level `status` to
  `"degraded"` so existing `status != "ok"` monitors catch it. The count is
  derived from the same predicate boot reconcile uses to decide what to
  recover, so the reported number and the recovery can never disagree.
