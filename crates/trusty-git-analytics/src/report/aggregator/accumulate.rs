//! Single-pass row accumulation and materialisation phases for the
//! aggregator.
//!
//! Why: `Aggregator::aggregate` reads as a recipe of named phases; the row
//! scan, its accumulator structs, and the per-slice materialisers live here
//! so the orchestrator (`mod.rs`) stays within the SLOC cap.
//! What: houses `RowFlags`/`compute_row_flags`, the `*Acc` accumulator
//! structs, `accumulate_rows`, and every `materialize_*` / `build_*` helper
//! that turns accumulator state into `ReportData` slices.
//! Test: exercised end-to-end by the `aggregator_*` cases in `report::tests`.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Datelike, Utc};

use crate::collect::ai_attribution::AgenticMode;
use crate::report::models::{
    AuthorSummary, RepositorySummary, UntrackedCommit, WeeklyActivity, WeeklyCategorization,
    WeeklyMetrics,
};

use super::{compile_patterns, is_boilerplate, CommitRow, PrRow, DEFAULT_BOILERPLATE_PATTERNS};

/// Pre-pass boilerplate / revert flags per row.
///
/// Why: every later phase (DORA bugfix counting, weekly-categorization
/// boilerplate bucketing) needs these bits, and recomputing per phase
/// would scan the row vector multiple times.
/// What: bundles a parallel `is_boilerplate` / `is_revert` `Vec<bool>`
/// indexed by row position, plus the aggregate counts.
/// Test: behavior preserved — the same `is_boilerplate` / `is_revert`
/// helpers run inline previously.
pub(super) struct RowFlags {
    pub(super) is_boilerplate: Vec<bool>,
    pub(super) is_revert: Vec<bool>,
    pub(super) boilerplate_count: usize,
    pub(super) revert_count: usize,
}

/// Why: keep flag computation in one named place so the main aggregate
/// function reads as a recipe of phases.
/// What: compiles the default regex sets once, walks the rows, and returns
/// a [`RowFlags`] capturing both per-row bits and aggregate counts.
/// Test: indirectly via report tests; identical to the inline loop that
/// existed in `aggregate` before this refactor.
pub(super) fn compute_row_flags(rows: &[CommitRow]) -> RowFlags {
    let boilerplate_re = compile_patterns(DEFAULT_BOILERPLATE_PATTERNS);

    let mut is_boilerplate: Vec<bool> = Vec::with_capacity(rows.len());
    let mut is_revert: Vec<bool> = Vec::with_capacity(rows.len());
    for row in rows {
        let lines = row.insertions + row.deletions;
        is_boilerplate.push(self::is_boilerplate(&row.message, lines, &boilerplate_re));
        // Issue #377: route revert detection through the shared core helper so
        // the report-time revert rate matches the persisted `is_revert` column.
        is_revert.push(crate::core::revert::is_revert(&row.message));
    }
    let boilerplate_count = is_boilerplate.iter().filter(|b| **b).count();
    let revert_count = is_revert.iter().filter(|b| **b).count();
    RowFlags {
        is_boilerplate,
        is_revert,
        boilerplate_count,
        revert_count,
    }
}

/// Per-author running totals during accumulation.
pub(super) struct AuthorAcc {
    pub(super) name: String,
    pub(super) email: String,
    pub(super) commits: usize,
    pub(super) insertions: i64,
    pub(super) deletions: i64,
    pub(super) files_changed: i64,
    pub(super) categories: HashMap<String, usize>,
    pub(super) first: DateTime<Utc>,
    pub(super) last: DateTime<Utc>,
}

/// Per-repository running totals during accumulation.
pub(super) struct RepoAcc {
    pub(super) commits: usize,
    pub(super) authors: HashSet<String>,
    pub(super) insertions: i64,
    pub(super) deletions: i64,
    pub(super) categories: HashMap<String, usize>,
}

/// Per-(week, author, repo) running totals during accumulation.
pub(super) struct WeekAcc {
    pub(super) commits: usize,
    pub(super) insertions: i64,
    pub(super) deletions: i64,
    pub(super) categories: HashMap<String, usize>,
    /// Revert commits in this bucket (issue #377 quality metric).
    pub(super) reverts: usize,
    /// Bugfix-classified commits in this bucket (issue #377).
    pub(super) bugfixes: usize,
    /// Ticketed commits in this bucket (issue #377).
    pub(super) ticketed: usize,
    /// AI-assisted commits in this bucket (issue #445: `is_ai_assisted=1`).
    pub(super) ai_assisted: usize,
    /// Running sum of non-null complexity scores for this bucket (issue #445
    /// batch B, request #6). Used with `complexity_count` to compute the
    /// mean at materialisation time. Only LLM-classified commits contribute.
    pub(super) complexity_sum: i64,
    /// Number of commits in this bucket with a non-null complexity score.
    pub(super) complexity_count: usize,
    /// Full-agentic commits (issue #1113: `agentic_mode = 'full_agentic'`).
    pub(super) agentic_count: usize,
    /// IDE-assisted commits (issue #1113: `agentic_mode = 'ide_assisted'`).
    pub(super) ide_assisted_count: usize,
}

/// Cross-developer per-week running totals during accumulation.
#[derive(Default)]
pub(super) struct WeekTotal {
    pub(super) commits: usize,
    pub(super) categories: HashMap<String, usize>,
    pub(super) developers: HashSet<String>,
}

/// Bundle of accumulator state that the single-pass scan produces.
///
/// Why: the row scan computes many parallel histograms at once; returning
/// them as a single struct keeps the orchestration in `aggregate` readable.
/// What: groups author / repo / weekly buckets and per-developer trackers
/// alongside the period bounds and aggregate counts.
/// Test: see `Aggregator::build` tests which exercise the full pipeline.
pub(super) struct Accumulators {
    pub(super) authors: HashMap<String, AuthorAcc>,
    pub(super) repos: HashMap<String, RepoAcc>,
    pub(super) weekly: BTreeMap<(String, String, String), WeekAcc>,
    pub(super) category_total: HashMap<String, usize>,
    pub(super) week_totals: BTreeMap<String, WeekTotal>,
    pub(super) dev_weeks: HashMap<String, HashSet<String>>,
    pub(super) dev_categories: HashMap<String, HashMap<String, usize>>,
    pub(super) dev_ticketed: HashMap<String, usize>,
    pub(super) min_ts: DateTime<Utc>,
    pub(super) max_ts: DateTime<Utc>,
    pub(super) boilerplate_count: usize,
    pub(super) revert_count: usize,
}

/// Why: the row scan touches a dozen parallel histograms; isolating it in a
/// named function lets the aggregator orchestration read as a sequence of
/// phases.
/// What: runs one pass over `rows`, updating the per-author / per-repo /
/// per-week / per-developer accumulators in lockstep. Caller is `aggregate`.
/// Test: indirectly via the `aggregator_*` tests in `report::tests`; this
/// is a literal lift of the inline loop that lived in `aggregate`.
pub(super) fn accumulate_rows(rows: &[CommitRow], flags: &RowFlags) -> Accumulators {
    // Period bounds initialised to the first row's timestamp.
    let mut min_ts = rows[0].timestamp;
    let mut max_ts = rows[0].timestamp;

    let mut authors: HashMap<String, AuthorAcc> = HashMap::new();
    let mut repos: HashMap<String, RepoAcc> = HashMap::new();
    let mut weekly: BTreeMap<(String, String, String), WeekAcc> = BTreeMap::new();
    let mut category_total: HashMap<String, usize> = HashMap::new();
    let mut week_totals: BTreeMap<String, WeekTotal> = BTreeMap::new();
    let mut dev_weeks: HashMap<String, HashSet<String>> = HashMap::new();
    let mut dev_categories: HashMap<String, HashMap<String, usize>> = HashMap::new();
    let mut dev_ticketed: HashMap<String, usize> = HashMap::new();

    for (idx, row) in rows.iter().enumerate() {
        if row.timestamp < min_ts {
            min_ts = row.timestamp;
        }
        if row.timestamp > max_ts {
            max_ts = row.timestamp;
        }

        // Authors. Group by email only; pick the longest display name seen
        // as the canonical name (heuristic: longer names tend to be the full
        // "Firstname Lastname" form rather than a short login handle).
        let key = row.author_email.clone();
        let a = authors.entry(key).or_insert_with(|| AuthorAcc {
            name: row.author_name.clone(),
            email: row.author_email.clone(),
            commits: 0,
            insertions: 0,
            deletions: 0,
            files_changed: 0,
            categories: HashMap::new(),
            first: row.timestamp,
            last: row.timestamp,
        });
        if row.author_name.len() > a.name.len() {
            a.name = row.author_name.clone();
        }
        a.commits += 1;
        a.insertions += row.insertions;
        a.deletions += row.deletions;
        a.files_changed += row.files_changed;
        if row.timestamp < a.first {
            a.first = row.timestamp;
        }
        if row.timestamp > a.last {
            a.last = row.timestamp;
        }
        if let Some(cat) = &row.category {
            *a.categories.entry(cat.clone()).or_insert(0) += 1;
        }

        // Repositories.
        let r = repos
            .entry(row.repository.clone())
            .or_insert_with(|| RepoAcc {
                commits: 0,
                authors: HashSet::new(),
                insertions: 0,
                deletions: 0,
                categories: HashMap::new(),
            });
        r.commits += 1;
        r.authors.insert(row.author_email.clone());
        r.insertions += row.insertions;
        r.deletions += row.deletions;
        if let Some(cat) = &row.category {
            *r.categories.entry(cat.clone()).or_insert(0) += 1;
        }

        // Weekly. Keyed by email (not display name) so that the same identity
        // committing under multiple names lands in a single weekly bucket.
        let week = iso_week_label(&row.timestamp);
        let wkey = (week, row.author_email.clone(), row.repository.clone());
        let w = weekly.entry(wkey).or_insert_with(|| WeekAcc {
            commits: 0,
            insertions: 0,
            deletions: 0,
            categories: HashMap::new(),
            reverts: 0,
            bugfixes: 0,
            ticketed: 0,
            ai_assisted: 0,
            complexity_sum: 0,
            complexity_count: 0,
            agentic_count: 0,
            ide_assisted_count: 0,
        });
        w.commits += 1;
        w.insertions += row.insertions;
        w.deletions += row.deletions;
        if let Some(cat) = &row.category {
            *w.categories.entry(cat.clone()).or_insert(0) += 1;
        }
        // Issue #377: per-(week, engineer, repo) quality signals. `is_revert`
        // is the shared-helper verdict computed in `compute_row_flags`;
        // `bugfix` comes from the classifier category; `ticketed` from the
        // commit's ticket-reference flag.
        if flags.is_revert[idx] {
            w.reverts += 1;
        }
        if row.category.as_deref() == Some("bugfix") {
            w.bugfixes += 1;
        }
        if row.ticketed {
            w.ticketed += 1;
        }
        // Issue #445: count AI-assisted commits per (week, engineer, repo) bucket
        // so the weekly activity report can surface AI-adoption rates.
        if row.is_ai_assisted {
            w.ai_assisted += 1;
        }
        // Issue #1113: count agentic/IDE-assisted commits per bucket.
        match row.agentic_mode {
            AgenticMode::FullAgentic => w.agentic_count += 1,
            AgenticMode::IdeAssisted => w.ide_assisted_count += 1,
            AgenticMode::None => {}
        }
        // Issue #445 batch B (request #6): accumulate complexity sum so
        // materialize_weekly_activity can compute avg_complexity without a
        // second pass. Only non-null values (LLM-classified commits) contribute.
        if let Some(c) = row.complexity {
            w.complexity_sum += c;
            w.complexity_count += 1;
        }

        // Category totals.
        if let Some(cat) = &row.category {
            *category_total.entry(cat.clone()).or_insert(0) += 1;
        }

        // Cross-developer weekly totals.
        let week_label = iso_week_label(&row.timestamp);
        let wt = week_totals.entry(week_label.clone()).or_default();
        wt.commits += 1;
        wt.developers.insert(row.author_email.clone());
        // Treat boilerplate rows as a synthetic category so they show
        // up in `weekly_categorization.csv` rather than being silently
        // bucketed into whatever the classifier returned.
        if flags.is_boilerplate[idx] {
            *wt.categories.entry("boilerplate".to_string()).or_insert(0) += 1;
        } else if let Some(cat) = &row.category {
            *wt.categories.entry(cat.clone()).or_insert(0) += 1;
        } else {
            *wt.categories.entry("unclassified".to_string()).or_insert(0) += 1;
        }

        // Per-developer week / category / ticketed tracking.
        dev_weeks
            .entry(row.author_email.clone())
            .or_default()
            .insert(week_label);
        if let Some(cat) = &row.category {
            *dev_categories
                .entry(row.author_email.clone())
                .or_default()
                .entry(cat.clone())
                .or_insert(0) += 1;
        }
        if row.ticketed {
            *dev_ticketed.entry(row.author_email.clone()).or_insert(0) += 1;
        }
    }

    Accumulators {
        authors,
        repos,
        weekly,
        category_total,
        week_totals,
        dev_weeks,
        dev_categories,
        dev_ticketed,
        min_ts,
        max_ts,
        boilerplate_count: flags.boilerplate_count,
        revert_count: flags.revert_count,
    }
}

/// Why: report consumers expect authors sorted by commit count with the
/// canonical (longest-seen) display name.
/// What: drains the author accumulator into [`AuthorSummary`] rows and
/// sorts them by descending commit count.
/// Test: indirectly via `aggregator_builds_report_data`.
pub(super) fn materialize_authors(authors: HashMap<String, AuthorAcc>) -> Vec<AuthorSummary> {
    let mut summaries: Vec<AuthorSummary> = authors
        .into_values()
        .map(|a| AuthorSummary {
            name: a.name,
            email: a.email,
            commit_count: a.commits,
            insertions: a.insertions,
            deletions: a.deletions,
            files_changed: a.files_changed,
            categories: a.categories,
            first_commit: a.first.to_rfc3339(),
            last_commit: a.last.to_rfc3339(),
        })
        .collect();
    summaries.sort_by_key(|a| std::cmp::Reverse(a.commit_count));
    summaries
}

/// Why: per-repo rows in reports include the top categories for the repo,
/// sorted by frequency, so reviewers can see at a glance what work
/// dominates each codebase.
/// What: drains the repo accumulator into [`RepositorySummary`] with the
/// top-categories vector sorted descending by count; the outer Vec is
/// sorted by descending repo commit count.
/// Test: indirectly via `aggregator_builds_report_data`.
pub(super) fn materialize_repositories(repos: HashMap<String, RepoAcc>) -> Vec<RepositorySummary> {
    let mut summaries: Vec<RepositorySummary> = repos
        .into_iter()
        .map(|(name, r)| {
            let mut top: Vec<(String, usize)> = r.categories.into_iter().collect();
            top.sort_by_key(|t| std::cmp::Reverse(t.1));
            RepositorySummary {
                name,
                commit_count: r.commits,
                author_count: r.authors.len(),
                insertions: r.insertions,
                deletions: r.deletions,
                top_categories: top,
            }
        })
        .collect();
    summaries.sort_by_key(|r| std::cmp::Reverse(r.commit_count));
    summaries
}

/// Why: the weekly bucket key uses email, but reports want canonical
/// display names so a single identity reads the same across the report.
/// What: drains the weekly map into [`WeeklyActivity`] rows, resolving each
/// row's email to its canonical display name via the `email_to_name` lookup
/// built from the already-materialised author summaries. Also computes
/// `commit_count_net` (issue #660: `commit_count - revert_count`) so
/// downstream consumers get the net-new figure without recomputing it.
/// Test: indirectly via `aggregator_builds_report_data` (two weekly rows
/// for two authors in different weeks); `commit_count_net` specifically via
/// `weekly_activity_commit_count_net_excludes_reverts`.
pub(super) fn materialize_weekly_activity(
    weekly: BTreeMap<(String, String, String), WeekAcc>,
    email_to_name: &HashMap<String, String>,
    abandoned_by_week_identity: &HashMap<(String, String), usize>,
) -> Vec<WeeklyActivity> {
    weekly
        .into_iter()
        .map(|((week, email, repository), w)| {
            let author = email_to_name.get(&email).cloned().unwrap_or(email.clone());
            // Issue #377 quality score for this (week, engineer, repo) bucket.
            let (quality_score, quality_tshirt) =
                crate::core::quality::score_and_tshirt(crate::core::quality::QualityInputs {
                    commits: w.commits,
                    reverts: w.reverts,
                    bugfixes: w.bugfixes,
                    ticketed: w.ticketed,
                });
            // Best-effort abandoned-PR attribution: match the PR author login
            // against either the resolved display name or the email
            // (case-insensitive). See `build_abandoned_pr_counts` for why this
            // is heuristic. Repository is not part of the PR identity key, so
            // a week's abandoned PRs land on the engineer's first repo bucket
            // for that week — counted once via the `.remove`-style guard would
            // require mutation; instead we look up by (week, identity) and
            // accept that an engineer active in multiple repos in one week
            // sees the same abandoned count echoed per repo row. Downstream
            // joins on (week, author) so this is acceptable and documented.
            let abandoned_pr_count = abandoned_by_week_identity
                .get(&(week.clone(), author.to_lowercase()))
                .or_else(|| abandoned_by_week_identity.get(&(week.clone(), email.to_lowercase())))
                .copied()
                .unwrap_or(0);
            // Issue #445 batch B (request #6): compute the mean complexity for
            // this bucket from the running sum. Returns None when no commit has
            // a non-null complexity score (all-null → None, not 0.0, so
            // downstream consumers can distinguish "no data" from "scored 0").
            let avg_complexity = if w.complexity_count > 0 {
                Some(w.complexity_sum as f64 / w.complexity_count as f64)
            } else {
                None
            };
            WeeklyActivity {
                week,
                author,
                repository,
                commit_count: w.commits,
                insertions: w.insertions,
                deletions: w.deletions,
                categories: w.categories,
                revert_count: w.reverts,
                bugfix_count: w.bugfixes,
                ticketed_count: w.ticketed,
                quality_score,
                quality_tshirt,
                abandoned_pr_count,
                // Issue #445: AI-assisted commits in this (week, engineer, repo) bucket.
                ai_assisted_count: w.ai_assisted,
                avg_complexity,
                // Issue #1113: agentic-mode commit counts.
                agentic_count: w.agentic_count,
                ide_assisted_count: w.ide_assisted_count,
                // Issue #660: net-new commits excluding reverts. `commit_count`
                // is left untouched above so downstream consumers relying on
                // the gross total keep working unmodified.
                commit_count_net: w.commits.saturating_sub(w.reverts),
            }
        })
        .collect()
}

/// Build a `(iso_week, author_identity_lowercased) → abandoned_pr_count` map.
///
/// Why: closed-but-unmerged PRs are a strong quality signal that today is
/// impossible to compute downstream (issue #377). Counting them per engineer
/// per week lets reports surface the abandoned-PR rate.
/// What: filters `prs` to `state == "closed" && merged_at.is_none()`, buckets
/// each by the ISO week of its `created_at` (abandoned PRs have no merge/close
/// timestamp available, so creation week is the only stable anchor), and keys
/// the count by the lowercased author login.
///
/// Limitation: the PR `author` is a provider login (e.g. a GitHub handle),
/// NOT a canonical engineer email. TGA has no login→engineer mapping at
/// aggregation time, so attribution in [`materialize_weekly_activity`] is a
/// best-effort case-insensitive match of the login against the engineer's
/// display name or email. When a login matches neither, the abandoned PR is
/// counted here but cannot be attributed to a weekly-activity row and is
/// effectively dropped from the per-engineer column. A future change that
/// persists a login→author_id mapping would make this exact.
/// Test: `aggregator_counts_abandoned_prs` in `report::tests`.
pub(super) fn build_abandoned_pr_counts(prs: &[PrRow]) -> HashMap<(String, String), usize> {
    let mut out: HashMap<(String, String), usize> = HashMap::new();
    for pr in prs {
        if pr.state == "closed" && pr.merged_at.is_none() {
            let week = iso_week_label(&pr.created_at);
            *out.entry((week, pr.author.to_lowercase())).or_insert(0) += 1;
        }
    }
    out
}

/// Why: weekly metrics are the cross-developer roll-up used for trend
/// charts; bucketing per category keeps the schema fixed regardless of
/// which categories appeared in the data.
/// What: walks the week-totals map and emits one [`WeeklyMetrics`] row per
/// ISO week with named bucket counters (feature / bugfix / maintenance /
/// refactor / test / docs).
/// Test: indirectly via `aggregator_builds_report_data` (asserts the
/// weekly_metrics vector is populated).
pub(super) fn build_weekly_metrics(
    week_totals: &BTreeMap<String, WeekTotal>,
) -> Vec<WeeklyMetrics> {
    week_totals
        .iter()
        .map(|(week, wt)| WeeklyMetrics {
            week: week.clone(),
            total_commits: wt.commits,
            feature_commits: *wt.categories.get("feature").unwrap_or(&0),
            bugfix_commits: *wt.categories.get("bugfix").unwrap_or(&0),
            maintenance_commits: *wt.categories.get("maintenance").unwrap_or(&0),
            refactor_commits: *wt.categories.get("refactor").unwrap_or(&0),
            test_commits: *wt.categories.get("test").unwrap_or(&0),
            doc_commits: *wt.categories.get("documentation").unwrap_or(&0)
                + *wt.categories.get("docs").unwrap_or(&0),
            active_developers: wt.developers.len(),
            story_points: 0.0,
        })
        .collect()
}

/// Why: the `weekly_categorization.csv` report needs one row per
/// (week, change-type) with the percentage share, so consumers can build
/// stacked-bar charts of "what work happened this week".
/// What: iterates the week-totals map and emits one row per category seen
/// in each week, sorted by category name for deterministic output.
/// Test: covered by `csv_formatter_writes_new_report_files` which writes
/// the weekly_categorization CSV.
pub(super) fn build_weekly_categorization(
    week_totals: &BTreeMap<String, WeekTotal>,
) -> Vec<WeeklyCategorization> {
    let mut rows: Vec<WeeklyCategorization> = Vec::new();
    for (week, wt) in week_totals {
        let total = wt.commits as f64;
        let mut entries: Vec<(&String, &usize)> = wt.categories.iter().collect();
        entries.sort_by_key(|e| e.0);
        for (cat, count) in entries {
            rows.push(WeeklyCategorization {
                week: week.clone(),
                change_type: cat.clone(),
                commit_count: *count,
                pct_of_week: if total > 0.0 {
                    (*count as f64) * 100.0 / total
                } else {
                    0.0
                },
            });
        }
    }
    rows
}

/// Why: untracked-commit rows surface commits without a ticket reference so
/// PMs can chase down missing trackable work.
/// What: filters `rows` to those that are unticketed and not boilerplate,
/// resolves each row's author email to its canonical display name, and
/// emits rows sorted newest-first.
/// Test: covered indirectly via `csv_formatter_writes_new_report_files`
/// (writes the `untracked.csv` file from this data).
pub(super) fn build_untracked_commits(
    rows: &[CommitRow],
    email_to_name: &HashMap<String, String>,
) -> Vec<UntrackedCommit> {
    let mut out: Vec<UntrackedCommit> = rows
        .iter()
        .filter(|r| !r.ticketed && r.category.as_deref() != Some("boilerplate"))
        .filter(|r| {
            // Treat NULL category OR explicit "unclassified" as untracked.
            r.category.is_none() || r.category.as_deref() == Some("unclassified") || !r.ticketed
        })
        .map(|r| UntrackedCommit {
            sha: r.sha.clone(),
            author: email_to_name
                .get(&r.author_email)
                .cloned()
                .unwrap_or_else(|| r.author_name.clone()),
            date: r.timestamp.to_rfc3339(),
            message: r.message.lines().next().unwrap_or("").to_string(),
        })
        .collect();
    // Deterministic ordering: newest first.
    out.sort_by(|a, b| b.date.cmp(&a.date));
    out
}

/// Format an ISO week label such as `"2024-W03"` from a UTC timestamp.
///
/// Why: weekly buckets are keyed by a stable lexically-sortable string so
/// BTreeMap iteration yields chronological output without an extra sort.
/// What: returns `YYYY-W{:02}` from the timestamp's ISO week.
/// Test: exercised by every aggregator test (all weekly buckets use this).
pub(super) fn iso_week_label(ts: &DateTime<Utc>) -> String {
    let iso = ts.iso_week();
    format!("{}-W{:02}", iso.year(), iso.week())
}
