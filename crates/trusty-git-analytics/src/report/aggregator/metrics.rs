//! Derived-metric computations for the aggregator: velocity, DORA, quality,
//! developer-activity scoring, and config/coverage diagnostics.
//!
//! Why: these phases consume the accumulator output and PR rows to produce
//! the scored slices of `ReportData`; isolating them keeps the orchestrator
//! (`mod.rs`) within the SLOC cap.
//! What: houses `compute_velocity_inputs`, `build_weekly_velocity`,
//! `compute_dora`, `compute_quality`, `build_summary`,
//! `compute_developer_activity`, the `dora_level` classifier, and the
//! coverage-drift / configured-alias diagnostics.
//! Test: covered by `aggregator_computes_summary_and_dora_and_quality` and
//! the other `aggregator_*` cases in `report::tests`.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};

use crate::core::config::Config;
use crate::core::db::Database;
use crate::report::models::{
    ActivityWeights, AuthorSummary, DeveloperActivitySummary, DoraMetrics, QualitySummary,
    ReportSummary, WeeklyVelocity,
};

use super::accumulate::{iso_week_label, RowFlags, WeekTotal};
use super::{CommitRow, DeploymentRow, PrRow};

/// Outputs of [`compute_velocity_inputs`].
pub(super) struct VelocityInputs {
    pub(super) cycle_time_avg: f64,
    pub(super) cycle_time_median: f64,
    pub(super) pr_throughput_per_week: f64,
    pub(super) pr_count: usize,
    pub(super) pr_per_week: HashMap<String, usize>,
}

/// Why: the velocity summary, weekly-velocity rows, and DORA lead-time all
/// derive from the same PR cycle-time arithmetic; computing it once keeps
/// the orchestrator readable and prevents drift between the metrics.
/// What: filters merged PRs to a sane cycle-time range (0.5–720 hours),
/// computes mean and median in hours, buckets merge timestamps by ISO week
/// for throughput, and returns the bundle.
/// Test: indirectly via `aggregator_computes_summary_and_dora_and_quality`.
pub(super) fn compute_velocity_inputs(prs: &[PrRow]) -> VelocityInputs {
    let mut cycle_times: Vec<f64> = prs
        .iter()
        .filter_map(|p| {
            p.merged_at.map(|m| {
                let secs = (m - p.created_at).num_seconds();
                (secs as f64) / 3600.0
            })
        })
        .filter(|h| *h >= 0.5 && *h <= 720.0)
        .collect();
    cycle_times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pr_count = cycle_times.len();
    let cycle_time_avg = if pr_count == 0 {
        0.0
    } else {
        cycle_times.iter().sum::<f64>() / pr_count as f64
    };
    let cycle_time_median = if pr_count == 0 {
        0.0
    } else {
        cycle_times[pr_count / 2]
    };

    let mut pr_per_week: HashMap<String, usize> = HashMap::new();
    for pr in prs {
        if let Some(merged) = pr.merged_at {
            *pr_per_week.entry(iso_week_label(&merged)).or_insert(0) += 1;
        }
    }
    let pr_throughput_per_week = if pr_per_week.is_empty() {
        0.0
    } else {
        pr_per_week.values().copied().sum::<usize>() as f64 / pr_per_week.len() as f64
    };

    VelocityInputs {
        cycle_time_avg,
        cycle_time_median,
        pr_throughput_per_week,
        pr_count,
        pr_per_week,
    }
}

/// Why: per-week velocity rows align PR throughput with active-developer
/// counts so reports can show team-level pace.
/// What: walks the week-totals map and emits one [`WeeklyVelocity`] row
/// per ISO week, joining against the `pr_per_week` lookup built by
/// [`compute_velocity_inputs`].
/// Test: covered indirectly by `csv_formatter_writes_new_report_files`
/// (writes the weekly velocity CSV).
pub(super) fn build_weekly_velocity(
    week_totals: &BTreeMap<String, WeekTotal>,
    pr_per_week: &HashMap<String, usize>,
    cycle_time_avg: f64,
) -> Vec<WeeklyVelocity> {
    week_totals
        .iter()
        .map(|(week, wt)| {
            let prs_merged = *pr_per_week.get(week).unwrap_or(&0);
            let active = wt.developers.len();
            let commits_per_dev = if active == 0 {
                0.0
            } else {
                wt.commits as f64 / active as f64
            };
            WeeklyVelocity {
                week: week.clone(),
                prs_merged,
                avg_pr_cycle_time_hours: cycle_time_avg,
                story_points: 0.0,
                commits_per_developer: commits_per_dev,
            }
        })
        .collect()
}

// #212 review round 2: the positional argument list kept growing across the
// fact_deployments fix; a call site reordering two `DateTime<Utc>` or two
// `usize` args silently compiles, so the inputs move into one named struct.
/// Grouped inputs for [`compute_dora`].
pub(super) struct DoraInputs<'a> {
    pub(super) rows: &'a [CommitRow],
    pub(super) flags: &'a RowFlags,
    pub(super) category_total: &'a HashMap<String, usize>,
    pub(super) prs: &'a [PrRow],
    pub(super) deployments: &'a [DeploymentRow],
    /// `true` when the `fact_deployments` query itself errored (e.g. a
    /// pre-migration DB missing the table) rather than simply returning no
    /// rows — [`compute_dora`] tags the proxy fallback distinctly in that
    /// case so a broken table isn't silently read as an empty one.
    pub(super) deployments_query_failed: bool,
    pub(super) period_start: DateTime<Utc>,
    pub(super) period_end: DateTime<Utc>,
    pub(super) cycle_time_avg: f64,
    // #212 review round 3: `cycle_time_avg` is 0.0 both when it's a genuine
    // average AND when zero PRs survived `compute_velocity_inputs`' 0.5-720h
    // outlier filter — `has_merged_prs` (any raw `merged_at`) missed the case
    // where merges exist but every one was filtered out, so the proxy still
    // reported a fabricated `Some(0.0)` "measured" lead time. This is the
    // post-filter count `compute_velocity_inputs` actually averaged over.
    pub(super) pr_cycle_time_count: usize,
    pub(super) total_weeks: usize,
    pub(super) revert_count: usize,
}

/// Why: DORA metrics are the standard rubric stakeholders use to score
/// engineering performance; computing them in one place keeps the four
/// values consistent with each other.
/// What: derives deployment frequency and lead time from `fact_deployments`
/// (issue #212) when the table has rows for the report period, falling back
/// to the merged-PR-count / cycle-time proxy only when it does not; derives
/// change-failure-rate from bugfix totals (clamped by revert count), and
/// MTTR from the spacing between consecutive bugfix/revert commits;
/// classifies the team via [`dora_level`].
/// Test: `aggregator_computes_summary_and_dora_and_quality` (well-formed
/// `performance_level`); the `dora_*` cases in `report::tests` covering the
/// measured/proxy/unmeasurable/query-failed provenance split.
pub(super) fn compute_dora(inputs: DoraInputs<'_>) -> DoraMetrics {
    let total_weeks_f = inputs.total_weeks.max(1) as f64;
    let total_commits = inputs.rows.len();
    let (deployment_frequency, deployment_frequency_source, lead_time_hours, lead_time_source) =
        deployment_frequency_and_lead_time(&inputs, total_weeks_f);
    let bugfix_total = inputs
        .category_total
        .get("bugfix")
        .copied()
        .unwrap_or(0)
        .max(inputs.revert_count);
    let change_failure_rate = if total_commits == 0 {
        0.0
    } else {
        bugfix_total as f64 / total_commits as f64
    };

    // MTTR approximation: average hours from a revert commit's predecessor
    // (assumed bug introduction) to the revert itself. Without a richer
    // mapping we approximate via the gap between consecutive bugfix
    // commits, capped by available data.
    let mut bugfix_ts: Vec<DateTime<Utc>> = inputs
        .rows
        .iter()
        .zip(inputs.flags.is_revert.iter())
        .filter(|(r, is_rev)| **is_rev || r.category.as_deref() == Some("bugfix"))
        .map(|(r, _)| r.timestamp)
        .collect();
    bugfix_ts.sort();
    let mttr_hours = if bugfix_ts.len() < 2 {
        0.0
    } else {
        let mut gaps: Vec<f64> = Vec::new();
        for w in bugfix_ts.windows(2) {
            let secs = (w[1] - w[0]).num_seconds().abs();
            gaps.push(secs as f64 / 3600.0);
        }
        gaps.iter().sum::<f64>() / gaps.len() as f64
    };
    // An unmeasurable lead time cannot support any tier that requires a
    // lead-time bound; treat it as unbounded (fails every tier's `lead_h`
    // check) rather than the pre-round-2 bug of silently passing as 0.0.
    let lead_h_for_level = lead_time_hours.unwrap_or(f64::INFINITY);
    let performance_level = dora_level(
        deployment_frequency,
        lead_h_for_level,
        change_failure_rate,
        mttr_hours,
    );
    DoraMetrics {
        deployment_frequency,
        lead_time_hours,
        change_failure_rate,
        mttr_hours,
        performance_level,
        deployment_frequency_source,
        lead_time_source,
    }
}

// #212: compute_dora previously derived `deploys` from `prs` unconditionally
// and never read `fact_deployments`, so a repo with real production deploy
// history reported `deployment_frequency: 0.0` identically to an empty one.
// #212 review round 2: a deploy can be measured while its lead time is not
// (an unmatched `git_sha`) — `deployment_frequency_source` and
// `lead_time_source` are resolved independently so the latter case can never
// mislabel a proxy/absent lead time as `"measured"`.
/// Resolve deployment frequency and lead time, preferring the measured
/// `fact_deployments` signal over the merged-PR-count proxy.
///
/// Why: isolating the source-selection logic keeps [`compute_dora`]'s
/// change-failure-rate / MTTR arithmetic (unaffected by this issue)
/// unentangled from the deploy-frequency fix.
/// What: filters `deployments` to `[period_start, period_end]` by
/// `triggered_at`. Empty after filtering → the pre-#212 proxy (merged-PR
/// count), tagged `"pr_merge_proxy"` (or `"pr_merge_proxy_query_failed"`
/// when `deployments_query_failed`). Non-empty → count as
/// `deployment_frequency`, tagged `"fact_deployments"`. Lead time is
/// resolved separately: the average `deploy_time - commit_time` over deploys
/// whose `git_sha` matches a commit (`"measured"`) when any resolve; else the
/// PR cycle-time proxy (`"proxy"`) when `pr_cycle_time_count > 0` — at least
/// one merged PR survived `compute_velocity_inputs`' outlier filter; else
/// `None` (`"unmeasurable"`) — never a bare `0.0` standing in for "no data".
/// Test: the `dora_*` cases in `report::tests`.
fn deployment_frequency_and_lead_time(
    inputs: &DoraInputs<'_>,
    total_weeks_f: f64,
) -> (f64, String, Option<f64>, String) {
    let period_deploys: Vec<&DeploymentRow> = inputs
        .deployments
        .iter()
        .filter(|d| {
            d.triggered_at
                .is_some_and(|t| t >= inputs.period_start && t <= inputs.period_end)
        })
        .collect();
    // #212 review round 3: gate on the POST-outlier-filter count, not "any
    // raw merged_at" — a PR set that exists but was entirely filtered out by
    // `compute_velocity_inputs`' 0.5-720h window must not proxy a fabricated
    // `Some(0.0)` lead time.
    let proxy_lead = || {
        if inputs.pr_cycle_time_count > 0 {
            (Some(inputs.cycle_time_avg), "proxy".to_string())
        } else {
            (None, "unmeasurable".to_string())
        }
    };

    if period_deploys.is_empty() {
        let deploys = inputs.prs.iter().filter(|p| p.merged_at.is_some()).count();
        let freq_source = if inputs.deployments_query_failed {
            "pr_merge_proxy_query_failed"
        } else {
            "pr_merge_proxy"
        };
        let (lead, lead_source) = proxy_lead();
        return (
            deploys as f64 / total_weeks_f,
            freq_source.to_string(),
            lead,
            lead_source,
        );
    }

    let deployment_frequency = period_deploys.len() as f64 / total_weeks_f;

    let commit_ts: HashMap<&str, DateTime<Utc>> = inputs
        .rows
        .iter()
        .map(|r| (r.sha.as_str(), r.timestamp))
        .collect();
    let mut leads: Vec<f64> = Vec::new();
    for d in &period_deploys {
        let Some(sha) = d.git_sha.as_deref() else {
            continue;
        };
        let Some(commit_time) = commit_ts.get(sha) else {
            continue;
        };
        let Some(deploy_time) = d.triggered_at.or(d.completed_at) else {
            continue;
        };
        let hours = (deploy_time - *commit_time).num_seconds() as f64 / 3600.0;
        // A deploy that precedes its own linked commit is a data error, not
        // a legitimate zero/negative lead time; drop it rather than skew the
        // average negative.
        if hours >= 0.0 {
            leads.push(hours);
        }
    }
    let (lead_time_hours, lead_time_source) = if !leads.is_empty() {
        (
            Some(leads.iter().sum::<f64>() / leads.len() as f64),
            "measured".to_string(),
        )
    } else {
        proxy_lead()
    };

    (
        deployment_frequency,
        "fact_deployments".to_string(),
        lead_time_hours,
        lead_time_source,
    )
}

/// Why: a single 0.0–1.0 quality score lets stakeholders compare teams /
/// time periods without internalising the DORA rubric.
/// What: combines bugfix-pct and revert-pct (weighted 0.4 / 0.6) into a
/// clamped score, computes defect-rate as bugfix-over-non-bugfix, and
/// packages them in a [`QualitySummary`].
/// Test: covered by `aggregator_computes_summary_and_dora_and_quality`
/// (asserts `quality_score` is in `[0.0, 1.0]`).
pub(super) fn compute_quality(
    total_commits: usize,
    category_total: &HashMap<String, usize>,
    revert_count: usize,
) -> QualitySummary {
    let bugfix_total = category_total
        .get("bugfix")
        .copied()
        .unwrap_or(0)
        .max(revert_count);
    let bugfix_pct = if total_commits == 0 {
        0.0
    } else {
        bugfix_total as f64 / total_commits as f64
    };
    let revert_pct = if total_commits == 0 {
        0.0
    } else {
        revert_count as f64 / total_commits as f64
    };
    let raw_quality = 1.0 - (bugfix_pct * 0.4) - (revert_pct * 0.6);
    let quality_score = raw_quality.clamp(0.0, 1.0);
    let non_bugfix = total_commits.saturating_sub(bugfix_total);
    let defect_rate = if non_bugfix == 0 {
        0.0
    } else {
        bugfix_total as f64 / non_bugfix as f64
    };
    QualitySummary {
        quality_score,
        revert_count,
        revert_pct,
        bugfix_pct,
        defect_rate,
    }
}

/// Why: every report needs a one-line "what does this cover" header so
/// readers can validate scope at a glance.
/// What: assembles a [`ReportSummary`] with the date range, totals, and
/// classification coverage percent.
/// Test: covered by `aggregator_computes_summary_and_dora_and_quality`
/// (asserts coverage_pct ≈ 50 with one of two commits classified).
pub(super) fn build_summary(
    rows: &[CommitRow],
    total_commits: usize,
    total_authors: usize,
    total_weeks: usize,
    min_ts: DateTime<Utc>,
    max_ts: DateTime<Utc>,
) -> ReportSummary {
    let classified_commits = rows.iter().filter(|r| r.category.is_some()).count();
    let classification_coverage_pct = if total_commits == 0 {
        0.0
    } else {
        classified_commits as f64 * 100.0 / total_commits as f64
    };
    let date_range = format!("{} .. {}", min_ts.to_rfc3339(), max_ts.to_rfc3339());
    ReportSummary {
        date_range,
        total_commits,
        total_developers: total_authors,
        total_weeks,
        classification_coverage_pct,
    }
}

/// Compute composite developer activity scores and roll-up rows.
///
/// Why: provides a single configurable number for ranking developers across
/// commits / impact / hygiene without committing to one dimension.
/// What: applies min-max normalization to each component across the period,
/// then a weighted sum per `ActivityWeights`.
/// Test: seed two authors with different commit counts; assert the higher
/// commit count yields the higher activity score.
pub(super) fn compute_developer_activity(
    authors: &[AuthorSummary],
    dev_weeks: &HashMap<String, HashSet<String>>,
    dev_categories: &HashMap<String, HashMap<String, usize>>,
    weights: &ActivityWeights,
) -> Vec<DeveloperActivitySummary> {
    if authors.is_empty() {
        return Vec::new();
    }

    // Min-max normalization helper. Returns 0.0 when all values are equal.
    fn norm(values: &[f64], idx: usize) -> f64 {
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if (max - min).abs() < f64::EPSILON {
            0.0
        } else {
            (values[idx] - min) / (max - min)
        }
    }

    let commits_v: Vec<f64> = authors.iter().map(|a| a.commit_count as f64).collect();
    let impact_v: Vec<f64> = authors
        .iter()
        .map(|a| (a.insertions + a.deletions) as f64)
        .collect();
    let complexity_v: Vec<f64> = authors
        .iter()
        .map(|a| {
            if a.commit_count == 0 {
                0.0
            } else {
                a.files_changed as f64 / a.commit_count as f64
            }
        })
        .collect();
    // PRs and ticketing are placeholders until per-developer PR aggregation
    // exists; using categories-sum as a stand-in keeps the field stable.
    let prs_v: Vec<f64> = vec![0.0; authors.len()];
    let ticketing_v: Vec<f64> = authors
        .iter()
        .map(|a| a.categories.values().copied().sum::<usize>() as f64)
        .collect();

    authors
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let score = weights.commits * norm(&commits_v, i)
                + weights.prs * norm(&prs_v, i)
                + weights.code_impact * norm(&impact_v, i)
                + weights.complexity * norm(&complexity_v, i)
                + weights.ticketing * norm(&ticketing_v, i);
            let active_weeks = dev_weeks.get(&a.email).map(|s| s.len()).unwrap_or(0);
            let avg_commits_per_week = if active_weeks == 0 {
                0.0
            } else {
                a.commit_count as f64 / active_weeks as f64
            };
            let primary_work_type = dev_categories
                .get(&a.email)
                .and_then(|m| m.iter().max_by_key(|(_, v)| **v).map(|(k, _)| k.clone()))
                .unwrap_or_else(|| "unknown".to_string());
            DeveloperActivitySummary {
                developer_id: a.email.clone(),
                display_name: a.name.clone(),
                total_commits: a.commit_count,
                active_weeks,
                avg_commits_per_week,
                primary_work_type,
                story_points_total: 0.0,
                activity_score: score,
            }
        })
        .collect()
}

/// DORA performance-level classifier.
///
/// Why: surface the four-band rubric defined in `docs/trusty-git-analytics/requirements/reporting.md`.
/// What: returns `"elite" | "high" | "medium" | "low"` based on the four DORA
/// metrics.
/// Test: feed elite-range inputs (>= 1 deploy/week, < 1h lead, < 0.15 cfr,
/// < 1h MTTR) and assert the returned label is `"elite"`.
pub(super) fn dora_level(deploys_per_week: f64, lead_h: f64, cfr: f64, mttr_h: f64) -> String {
    let elite = deploys_per_week >= 1.0 && lead_h < 1.0 && cfr < 0.15 && mttr_h < 1.0;
    if elite {
        return "elite".to_string();
    }
    let high = deploys_per_week >= 0.25 && lead_h < 168.0 && cfr < 0.30 && mttr_h < 24.0;
    if high {
        return "high".to_string();
    }
    let medium = deploys_per_week >= 0.04 && lead_h < 720.0 && cfr < 0.30 && mttr_h < 168.0;
    if medium {
        return "medium".to_string();
    }
    "low".to_string()
}

/// Parse an ISO week label of the form `"YYYY-Www"` into `(year, week)`.
///
/// Why: parsing once gives the coverage-drift check a way to look up the
/// recorded `repo_count` for the week.
/// What: returns `None` for malformed labels — callers should skip the
/// entry rather than abort the entire report.
/// Test: indirectly via `check_weekly_coverage_drift`.
pub(super) fn parse_iso_week_label(label: &str) -> Option<(i32, u32)> {
    let (year_s, week_s) = label.split_once("-W")?;
    let year: i32 = year_s.parse().ok()?;
    let week: u32 = week_s.parse().ok()?;
    Some((year, week))
}

/// Emit a warning when adjacent weekly metric rows were collected with
/// different repository counts (issue #69). Coverage drift between weeks
/// makes week-over-week deltas misleading.
///
/// Why: weekly snapshots collected at different times may have different
/// `repositories[]` rosters; without surfacing this, WoW deltas look like
/// engineering changes when they're really configuration changes.
/// What: walks consecutive `weekly_metrics` entries, looks up the recorded
/// `repo_count` per week via [`crate::core::db::repo_count_for_week`], and
/// warns when the values disagree.
/// Test: seed `collection_runs` with two weeks at different repo_counts,
/// build a report, assert a warning is logged (smoke-tested via the
/// public `Aggregator::build` path).
pub(super) fn check_weekly_coverage_drift(
    db: &Database,
    weekly_metrics: &[crate::report::models::WeeklyMetrics],
) {
    if weekly_metrics.len() < 2 {
        return;
    }
    let mut prev: Option<(String, i64)> = None;
    for wm in weekly_metrics {
        let (year, week) = match parse_iso_week_label(&wm.week) {
            Some(v) => v,
            None => continue,
        };
        let count = match crate::core::db::repo_count_for_week(db, year, week) {
            Ok(Some(n)) => n,
            // No recorded count for this week — either pre-migration data or
            // legacy `record_collection_run` calls. Skip silently; the user
            // will see normal output and we avoid noisy warnings on fresh
            // databases.
            _ => continue,
        };
        if let Some((prev_label, prev_count)) = &prev {
            if *prev_count != count {
                tracing::warn!(
                    prev_week = %prev_label,
                    prev_repo_count = prev_count,
                    week = %wm.week,
                    repo_count = count,
                    "WARNING: Week-over-week comparison may be inaccurate — W{prev} was \
                     collected with {n_prev} repos, W{cur} with {n_cur} repos. Re-run \
                     `tga collect --force --from <week-start> --to <week-end>` for the \
                     prior week to normalize coverage.",
                    prev = prev_label,
                    n_prev = prev_count,
                    cur = wm.week,
                    n_cur = count,
                );
            }
        }
        prev = Some((wm.week.clone(), count));
    }
}

/// Collect every email address referenced by the configured alias map
/// (`developer_aliases` + `team.members.email` + `team.members.aliases`)
/// for "is this author in the configured roster?" lookups.
///
/// Why: see issue #68 — when an author's canonical email is not in the
/// configured alias map they are a "phantom" identity that inflates the
/// developer count.
/// What: returns a set of lowercased email addresses; non-email aliases
/// (login handles) are filtered out so case-insensitive email comparison
/// is sufficient.
/// Test: build a `Config` with one developer_aliases entry, assert the
/// returned set contains the lowercased email.
pub(super) fn configured_alias_emails(config: &Config) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::new();
    for entries in config.developer_aliases.values() {
        for e in entries {
            if e.contains('@') {
                out.insert(e.to_lowercase());
            }
        }
    }
    if let Some(team) = &config.team {
        for m in &team.members {
            if m.email.contains('@') {
                out.insert(m.email.to_lowercase());
            }
            for a in &m.aliases {
                if a.contains('@') {
                    out.insert(a.to_lowercase());
                }
            }
        }
    }
    out
}
