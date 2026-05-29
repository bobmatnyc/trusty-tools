# Output Generation (Stage 3)

Stage 3 of the pipeline: aggregate cache contents into output artifacts. Formats include
CSV (per-metric files), JSON (machine-readable summaries), and Markdown (narrative writeups).

> Note: This file is named `output-generation.md` for tooling compatibility. The Python
> predecessor refers to this stage as "reporting".

## DORA Metrics

The four DORA "Accelerate" metrics:

### Deployment Frequency
- Detection: configurable regex patterns matching commits / PRs identified as deploys
  (default: matches against tags `v\d+\.\d+`, branch merges to `main`, presence of CI
  release workflows)
- Output: deploys/day, deploys/week, deploys/month per repo

### Lead Time for Changes
- Definition: first commit on a feature branch → merge / deploy timestamp
- Computed per merged PR
- Output: median + p90 hours per repo and per developer

### Change Failure Rate
- Detection: `quality_report.revert_patterns` (default `["^revert", "rollback", "hotfix"]`)
- Computed as `revert_count / total_deploys`
- Output: percentage per repo, rolling 30-day window

### Mean Time To Recovery (MTTR)
- Definition: deploy failure detected (revert commit timestamp) − failed deploy timestamp
- Output: median + p90 hours per repo

### Performance Levels

| Level | Deploy Freq | Lead Time | Change Fail Rate | MTTR |
|-------|-------------|-----------|------------------|------|
| Elite | on demand (multiple/day) | < 1 hour | 0–15% | < 1 hour |
| High | weekly–monthly | 1 day–1 week | 16–30% | < 1 day |
| Medium | monthly–6 months | 1 week–1 month | 16–30% | < 1 day |
| Low | < monthly | > 6 months | > 30% | > 1 week |

---

## Velocity Metrics

### PR Cycle Time
- Definition: `merged_at` − `created_at` in hours
- Outlier filter: `velocity.cycle_time_min_hours` to `cycle_time_max_hours` (default 0.5h–720h)
- Output: avg, median per engineer per ISO week

### PR Throughput
- Merged PRs per ISO week per engineer

### PR Revision Rate
- Avg `revision_count` per merged PR per engineer

### Story Points / Week
- Sum of `story_points` from commits or correlated JIRA tickets

### Top N Developers by Activity
- Composite score (see Activity Scoring below)
- Default N = 10

---

## Activity Scoring

Composite score with configurable weights from `activity_scoring`:

```
score = w_commits * norm(commits)
      + w_prs * norm(prs_merged)
      + w_code_impact * norm(lines_changed)
      + w_complexity * norm(avg_complexity)
      + w_ticketing * norm(story_points)
```

Weights must sum to 1.0. Defaults:

| Weight | Default |
|--------|---------|
| `commits_weight` | 0.22 |
| `prs_weight` | 0.26 |
| `code_impact_weight` | 0.26 |
| `complexity_weight` | 0.11 |
| `ticketing_weight` | 0.15 |

`norm()` applies min-max normalization scoped to the period.

---

## Quality Output

When `quality_report.enabled: true`:

### Revert Detection
- Patterns from `revert_patterns` matched against commit messages
- Per-developer revert count and rate

### Risk Profile
- Aggregate `risk_level` (low/medium/high) from `qualitative_commits` per developer

### Code Review Signals
- `avg revision_count` per PR
- `total change_requests received` per developer
- Warning when `revision_count >= quality_report.min_revision_warning` (default 3)

### Composite Quality Score (0–1)
- Weighted combination of revert rate, risk level, review signals
- Output per developer and per team

---

## Boilerplate Filter

When `boilerplate_filter.enabled: true`:

- Detects developers whose commits appear to be auto-generated code pushes
- Thresholds:
  - `avg_lines_per_commit > avg_lines_per_commit_threshold` (default 500)
  - `total_lines > total_lines_threshold` (default 10000)
- Actions:
  - `flag`: mark in output, include all data
  - `exclude_from_averages`: include in raw data, exclude from team averages
  - `exclude`: exclude entirely from output artifacts

---

## Output Files

Files are written to `output.directory` (default `./reports`). Filenames embed a date stamp
or ISO range.

### CSV Files

| Filename | Contents |
|----------|----------|
| `weekly_metrics_{date}.csv` | Per-developer per-week aggregations |
| `developer_activity_summary_{date}.csv` | Top-line developer stats for the period |
| `summary_{date}.csv` | Overall pipeline summary |
| `developers_{date}.csv` | Canonical developer list with aliases |
| `untracked_commits_{date}.csv` | Commits without ticket references |
| `weekly_categorization_{date}.csv` | Commit counts by `change_type` per week |
| `weekly_velocity_{date}.csv` | Velocity metrics per developer per week |
| `weekly_dora_metrics_{date}.csv` | DORA metrics per repo per week |

CSV delimiter and encoding configurable via `output.csv_delimiter` and `output.csv_encoding`.

### Markdown Files

| Filename | Contents |
|----------|----------|
| `narrative_report_{range}.md` | LLM-summarized narrative of the period |
| `database_qualitative_report_{range}.md` | Per-domain qualitative breakdown |

Generated via `tera` template engine. Templates ship in `tga-report/templates/`.

### JSON Files

| Filename | Contents |
|----------|----------|
| `velocity_summary.json` | Velocity metrics structured for dashboards |
| `weekly_summary.json` | Weekly rollups |
| `quality_summary.json` | Quality output data |
| `comprehensive_export_{date}.json` | Full export for downstream consumers |

### Coverage and unresolved-author fields

`ReportData` carries three top-level integers that surface scope and
identity-resolution health alongside the headline totals:

| Field | Type | Meaning |
|-------|------|---------|
| `repository_coverage` | `usize` | Number of distinct `commits.repository` values observed in this run. Use this to detect undercounting when the configured `repositories[]` roster is narrower than the actual portfolio (issue #67). |
| `unresolved_authors` | `usize` | Count of distinct author identities (name/email pairs) that did not resolve to a configured canonical team member (issue #68). Guards against "phantom developers" silently inflating active-developer counts. |
| `unresolved_author_commits` | `usize` | Count of commits whose author identity could not be resolved to a canonical team member (issue #68). These commits still appear in totals but are tracked separately. |

All three fields are emitted in `comprehensive_export_{date}.json`.

---

## Anonymization

When `output.anonymize_enabled: true` (or `--anonymize` flag):

- Developer names replaced with `dev_001`, `dev_002`, … (stable per canonical_id within run)
- Email addresses removed
- Mapping file `anonymization_map_{date}.json` written to a non-shared directory
- All other data fields preserved

---

## Performance Considerations

- Heavy aggregations run via `rusqlite` SQL queries (push computation to SQLite engine)
- CSV writes use `csv::Writer` buffered to disk (no full in-memory accumulation)
- JSON exports stream via `serde_json::to_writer` over `BufWriter`
- Markdown rendering: `tera::Tera::render` once per template, ~10–100 ms per output
