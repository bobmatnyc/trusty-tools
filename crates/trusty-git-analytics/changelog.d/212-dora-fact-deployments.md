Fixed

- `compute_dora()` now reads `fact_deployments` (`environment='production',
  status='success'`, filtered to the report period) for deployment frequency
  and lead time, instead of always counting merged PRs as a deploy proxy
  (#212). A repo with real production deploy history previously reported
  `deployment_frequency: 0.0` and `performance_level: "low"` identically to a
  repo with none, because `compute_dora()` never queried the table. The
  merged-PR/cycle-time proxy is still used, unchanged, when `fact_deployments`
  has zero rows for the period. `DoraMetrics` gains
  `deployment_frequency_source` (`"fact_deployments"` or `"pr_merge_proxy"`),
  surfaced in `dora_summary.json` and appended to `weekly_dora_metrics.csv`,
  so a reader can tell which source produced the numbers.
