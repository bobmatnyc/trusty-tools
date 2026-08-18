Breaking

- `/health`'s `warmboot_summary.indexes_corpus_failed` now counts only indexes
  whose durable corpus failed to open. It was computed from
  `IndexStages::any_failed()`, so an index whose SEMANTIC lane died — corpus
  perfectly healthy — incremented a counter named for corpus-open failures.
  Two investigations in one day read the name literally, checked every index's
  `corpus_open_failure` (correctly `null`), found nothing, and dismissed a live
  count of `1` as a stale boot snapshot; the real cause was one index with
  `stages.semantic = "failed"`. The count is now read from
  `CodeIndexer::corpus_open_failed`, the same flag `GET /indexes/:id/status`
  reports, so the two surfaces can no longer disagree (issue #5927).
- A new `warmboot_summary.indexes_stage_failed` carries the any-lane count
  under a name that states what it measures. It is a strict superset of
  `indexes_corpus_failed` — a corpus-open failure fails every lane — and it is
  what forces `warm_boot_degraded` and the top-level `status: "degraded"`, so
  no daemon that used to report degraded stops doing so. A consumer that reads
  `indexes_corpus_failed` as "any lane failed" should move to the new key;
  monitors polling `warm_boot_degraded` or `status` need no change.
