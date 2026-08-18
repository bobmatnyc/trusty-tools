Fixed

- A trusty-search index whose semantic or graph lane failed over a healthy
  corpus no longer disappears from a degraded review's stamped reason.
  trusty-search #5927 narrowed `warmboot_summary.indexes_corpus_failed` to real
  corpus-open failures and moved the any-lane count to a new
  `indexes_stage_failed` key. `degraded_reason` read only the old key, so after
  that narrowing a lane failure built an empty clause list — which
  `serving_state` classifies as the benign network-mount case and reports as
  `Serving`. The wire mirror now carries `indexes_stage_failed` and reports the
  cohort it names, minus the corpus cohort so no index is counted twice, with
  its own consequence: those indexes answer with lexical results only.
