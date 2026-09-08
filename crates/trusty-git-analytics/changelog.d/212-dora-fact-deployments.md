Fixed

- `compute_dora()` now reads `fact_deployments` (`environment='production',
  status='success'`, filtered to the report period) for deployment frequency
  and lead time, instead of always counting merged PRs as a deploy proxy
  (#212). A repo with real production deploy history previously reported
  `deployment_frequency: 0.0` and `performance_level: "low"` identically to a
  repo with none, because `compute_dora()` never queried the table. The
  merged-PR/cycle-time proxy is still used, unchanged, when `fact_deployments`
  has zero rows for the period.
- Deployment frequency and lead time now carry independent provenance:
  `DoraMetrics` gains `deployment_frequency_source`
  (`"fact_deployments"` | `"pr_merge_proxy"` |
  `"pr_merge_proxy_query_failed"`) and `lead_time_source`
  (`"measured"` | `"proxy"` | `"unmeasurable"`). `lead_time_hours` is now
  `Option<f64>` — `None`/`null` when neither a linked deploy nor merged-PR
  data can produce a value, never a `0.0` presented as measured. A
  `fact_deployments` query failure (e.g. a pre-migration DB missing the
  table) is now logged and tagged `"pr_merge_proxy_query_failed"`, distinct
  from a table that exists with zero in-period rows. All four fields are
  serialized in `dora_summary.json` and `weekly_dora_metrics.csv`.
