Added

- `tga backfill pm-work` classifies every `work_items` row's meaningfulness and persists the verdict to the new `fact_pm_work` table, the PM-side WORK tier of the Activity/Work/Effort model ([#3916](https://github.com/bobmatnyc/trusty-tools/issues/3916))
  - Deterministic v1 rules (`formula_version = "pm-work-1"`) exclude terse decomposition stubs (`TERSE_TITLE`), tickets a bot filed that nobody moved (`AUTO_GENERATED`), and tickets a bot filed that a human later transitioned (`BOT_FILED`). No LLM tier.
  - Idempotent: UPSERT on `(work_item_id, work_item_source)`, so a re-run rewrites the same rows and adds none. `--dry-run` reports the candidate count without writing.
  - PM work rows are counted in tickets and must never share a visualization axis with `fact_commit_effort` rows ([#3917](https://github.com/bobmatnyc/trusty-tools/issues/3917)).
