Fixed

- Corpus-open failures are now classified rather than collapsed into one string.
  A transient open timeout or lock contention no longer reports "incompatible or
  corrupted format" and no longer prescribes `trusty-search index <path> --force`
  — wording that had already cost one healthy 200k-chunk index to a destructive
  rebuild. Transient states say the on-disk corpus is presumed intact and
  explicitly forbid a reindex; only a redb-reported format incompatibility or
  corruption keeps the rebuild instruction. `GET /indexes/:id/status` gains a
  `corpus_open_failure` object (`kind`, `transient`, `reason`) and reports
  `chunk_count: null` instead of a partial-looking in-memory count while the
  corpus is unopened (see
  [#4333](https://github.com/bobmatnyc/trusty-tools/issues/4333)).
- An index whose durable corpus failed to open no longer answers searches with
  `HTTP 200` and an empty result set — a total per-index outage that was
  indistinguishable from "no matches". `POST /indexes/:id/search` now returns
  `503 index_corpus_unavailable` carrying the failure classification, and
  `POST /search` excludes such indexes from the fan-out and reports them in a new
  `corpus_failed_indexes_skipped` field. An index whose eager warm-boot restore
  **times out** is now parked in the cold store — recoverable by lazy load on the
  next query — instead of being dropped from both the registry and the cold store
  for the rest of the boot. A restore that **panics** is deliberately not parked:
  it is broken rather than slow, so it keeps failing loudly instead of being
  reported as lazy/recoverable, and panics are now counted separately from
  timeouts in the warm-boot summary (see
  [#4087](https://github.com/bobmatnyc/trusty-tools/issues/4087)).
- The orphan reaper no longer defers indefinitely on ambiguous relocation
  candidates. The first ambiguous observation is stamped and logged at ERROR (so
  it reaches `errors.jsonl` / `tm doctor` rather than only the log file), and
  after a grace period — 7 days by default, tunable via
  `TRUSTY_AMBIGUOUS_ROOT_GRACE_SECS`, disabled with `0` — the stale *registration*
  is removed with a logged warning. On-disk index data is never deleted, so the
  entry stays recoverable with `trusty-search index <path>` (see
  [#4095](https://github.com/bobmatnyc/trusty-tools/issues/4095)).
