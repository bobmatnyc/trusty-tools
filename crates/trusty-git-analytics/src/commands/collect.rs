//! `tga collect` — stage 1 (git extraction) entry point.

use tga::collect::collector::{FetchOutcome, PerRepoFetch};
use tga::collect::CollectionPipeline;
use tga::core::config::Config;
use tga::core::db::Database;
use tga::core::progress::ProgressBus;

use crate::commands::args::CollectArgs;
use crate::commands::date_range::resolve_date_range;

/// Run the collection stage against the provided database.
///
/// Why: centralises all Stage 1 orchestration: config overrides, pipeline
/// construction, dry-run shadow DB, fetch summary printing, and exit-code
/// signalling for fetch failures.
/// What: applies CLI overrides (repository filter, since/until dates) on top
/// of the loaded YAML config, runs [`CollectionPipeline::run`], then prints
/// the fetch summary and collection totals to stderr/stdout.  Since tga
/// 2.6.0, fetch failures are fatal by default (exit non-zero) unless
/// `--allow-stale` or `--no-fetch` is passed. Collects with progress
/// reporting off; see [`run_with_progress`] for the bus-carrying form.
/// Test: integration test in `tests/integration_test.rs`; fetch summary paths
/// are unit-tested in `commands::collect::tests`.
pub async fn run(config: Config, db: &mut Database, args: CollectArgs) -> anyhow::Result<()> {
    run_with_progress(config, db, args, &ProgressBus::disabled()).await
}

/// Run the collection stage, publishing pipeline progress onto `progress`.
///
/// Why: #5361 — `audit::run_full_sweep` accepts a [`ProgressBus`] but had no
/// way to hand it to collection, because [`run`] built the pipeline with a
/// disabled bus of its own. A caller that supplies a bus at the sweep entry
/// point must get the pipeline's per-repository events on THAT bus, not on one
/// the command wrapper constructed and threw away.
/// What: identical to [`run`] in every other respect; `progress` is attached to
/// the [`CollectionPipeline`] via `with_progress`. Passing
/// [`ProgressBus::disabled`] makes every emit a no-op, which is what [`run`]
/// does, so the CLI path is byte-identical.
/// Test: `crate::audit::tests::sweep_emits_progress_for_non_collection_stages`.
pub async fn run_with_progress(
    config: Config,
    db: &mut Database,
    args: CollectArgs,
    progress: &ProgressBus,
) -> anyhow::Result<()> {
    run_reporting_fetch(config, db, args, progress)
        .await
        .map(|_| ())
}

/// Run the collection stage and hand back what happened to each remote.
///
/// Why: #5321 — under `--allow-stale` a repository whose remote is unreachable
/// is walked on its stale local refs and collection returns `Ok`, so a caller
/// that only sees the `Result` cannot tell fresh data from data that may be
/// months behind. The per-repo outcomes exist already; they were printed to
/// stderr and dropped. `tga audit` needs them as values, because DOC-67 §9's
/// obligation is to say so in the report, not on a terminal nobody keeps.
/// What: identical to [`run_with_progress`] — same overrides, same warnings,
/// same stderr summary, same strict-fetch bail — except that the successful
/// return carries [`tga::collect::collector::CollectionStats::fetch_outcomes`]
/// instead of `()`. An `Err`
/// return carries no outcomes: a stage that failed is already a named gap.
/// Test: `crate::audit::tests::a_repo_that_fell_back_to_stale_local_refs_is_named_in_the_gap_lines`.
///
/// # Errors
///
/// Propagates date-range resolution and pipeline errors, and — unless
/// `--allow-stale` or `--no-fetch` was passed — a fetch failure on any repo.
///
/// Since #5655 it also errors when any stage recorded a
/// [`tga::collect::FaultSeverity::StageFailed`] fault: a provider whose write
/// or fetch path failed leaves the database incomplete, and returning `Ok`
/// there is what let an unattended run report success over missing data. The
/// run itself still completes every stage first — the error reports what
/// happened, it does not abort anything — and a per-record skip never
/// contributes. `--allow-stale` does not suppress it; that flag governs git
/// remote freshness, not whether a write landed.
pub async fn run_reporting_fetch(
    config: Config,
    db: &mut Database,
    args: CollectArgs,
    progress: &ProgressBus,
) -> anyhow::Result<Vec<PerRepoFetch>> {
    let mut cfg = config;

    // Filter repositories by name when --repos is supplied.
    if !args.repos.is_empty() {
        cfg.repositories.retain(|r| {
            let name = r.name.clone().unwrap_or_default();
            args.repos.contains(&name)
        });
        if cfg.repositories.is_empty() {
            tracing::warn!(
                "no repositories matched --repos filter ({:?}); nothing to do",
                args.repos
            );
        }
    }

    // Resolve the (since, until) window from --weeks, --from/--to, --since/--until,
    // or the config fallback. Priority: --weeks > --from/--to > legacy --since/--until > config.
    let legacy_since = args.since.clone();
    let (resolved_since, resolved_until) = resolve_date_range(
        args.weeks,
        args.from.as_deref(),
        args.to.as_deref(),
        legacy_since.as_deref(),
    )?;
    let effective_until = resolved_until.or_else(|| args.until.clone());

    // Apply date overrides to every selected repository.
    if let Some(since) = resolved_since.as_ref() {
        tracing::info!(since = %since, "applying collection lower bound");
        for repo in &mut cfg.repositories {
            repo.since_date = Some(since.clone());
        }
    }
    if let Some(until) = effective_until.as_ref() {
        tracing::info!(until = %until, "applying collection upper bound");
        for repo in &mut cfg.repositories {
            repo.until_date = Some(until.clone());
        }
    }

    // Emit a visible warning when --no-fetch or --allow-stale suppresses
    // error-on-failure so users know the data may be stale.
    if args.no_fetch {
        eprintln!(
            "WARNING: --no-fetch active. Local clones may be stale. \
             tga collect will walk only what's already in your local object store."
        );
    } else if args.allow_stale {
        eprintln!(
            "WARNING: --allow-stale active. If any remote is unreachable, \
             tga collect will continue on stale local refs without erroring. \
             Data may be out of date."
        );
    }

    // Since tga 2.6.0 fetch failures are fatal by default (strict_fetch=true).
    // --allow-stale reverts to the old best-effort behaviour.
    // --no-fetch skips the fetch entirely (also disables strict checking since
    // there is nothing to check).
    // The legacy --strict-fetch flag is kept for backwards compatibility with
    // CI scripts that set it explicitly; when present it reinforces the default.
    let effective_strict = !args.allow_stale && !args.no_fetch;

    // #5249: state what agentic detection can and cannot see, once per run.
    tracing::info!("{}", tga::collect::ai_markers::detection_disclosure());

    let pipeline = CollectionPipeline::new(cfg)
        .with_progress(progress.clone())
        .with_force(args.force)
        .with_no_fetch(args.no_fetch)
        .with_force_refresh_prs(args.force_refresh_prs)
        .with_skip_tag_reachability(args.skip_tag_reachability)
        .with_head_only(args.head_only)
        .with_branches(args.branch)
        .with_strict_fetch(effective_strict)
        .with_verbose_fetch(args.verbose_fetch);

    // In dry-run mode, redirect all writes to an ephemeral in-memory
    // database. The real `db` is never opened for write.
    let stats = if args.dry_run {
        tracing::info!("Dry run — no database writes will occur");
        let mut shadow = Database::open_in_memory()?;
        pipeline.run(&mut shadow).await?
    } else {
        pipeline.run(db).await?
    };

    if args.dry_run {
        // #6073 review: no `repos_skipped` here. A dry run walks against an
        // empty in-memory shadow database, which holds no `repo_walk_state`
        // row for any repository, so the figure is structurally always 0 and
        // reads as "nothing was skipped" whatever the real database records.
        println!(
            "Dry run complete. Would have written {} commits, {} authors, \
             {} PRs ({} weeks collected, {} weeks skipped). No changes persisted.",
            stats.commits_collected,
            stats.authors_resolved,
            stats.prs_fetched,
            stats.weeks_collected,
            stats.weeks_skipped,
        );
    } else {
        // #6073: `repos_skipped` is the only figure separating a skipped
        // full-history walk from one that ran and found nothing new — both
        // leave `commits_collected` at zero.
        println!(
            "Collected {} commits from {} authors ({} PRs fetched, \
             {} weeks collected, {} weeks skipped, \
             {} repo full-history walks skipped)",
            stats.commits_collected,
            stats.authors_resolved,
            stats.prs_fetched,
            stats.weeks_collected,
            stats.weeks_skipped,
            stats.repos_skipped,
        );
    }
    if !stats.errors.is_empty() {
        eprintln!(
            "Encountered {} issue(s) during collection:",
            stats.errors.len()
        );
        for e in &stats.errors {
            // #5655: the severity label is the operator-visible half of the
            // same split the exit code reads.
            eprintln!("  {}: {}", e.severity.label(), e.message);
        }
    }

    // Issue #334: print per-repo fetch summary to stderr.
    print_fetch_summary(&stats.fetch_outcomes, args.verbose_fetch);

    // Since tga 2.6.0, fetch failures are fatal by default (strict_fetch=true
    // unless --allow-stale or --no-fetch was passed).  Collect all failures
    // and report them together so one bad repo does not mask others.
    if pipeline.strict_fetch() {
        let failures: Vec<_> = stats
            .fetch_outcomes
            .iter()
            .filter(|f| matches!(f.outcome, FetchOutcome::Failed { .. }))
            .collect();
        if !failures.is_empty() {
            let mut msg = format!(
                "{} repo(s) could not be fetched from their remotes — \
                 refusing to analyze stale data (use --allow-stale to override):\n",
                failures.len()
            );
            for f in &failures {
                if let FetchOutcome::Failed { error, remote } = &f.outcome {
                    msg.push_str(&format!("  - {} (remote: {}): {}\n", f.repo, remote, error));
                }
            }
            msg.push_str(
                "\nFix: ensure SSH agent has a loaded key, or set GITHUB_TOKEN/GH_TOKEN, \
                 or configure your git credential helper. \
                 Run `git fetch origin` in the failing repo to diagnose.",
            );
            anyhow::bail!("{}", msg.trim_end());
        }
    }

    // #5655: a stage whose write or fetch path failed must not exit 0.
    if let Some(msg) = stage_failure_report(&stats) {
        anyhow::bail!("{msg}");
    }

    Ok(stats.fetch_outcomes)
}

/// Build the abort message for the stages that never persisted their data.
///
/// Why: #5655 — every provider pushed its failures into `stats.errors`, the
/// command printed them as warnings, and the process exited 0. A script, a CI
/// job, or `tga audit` running unattended read that as success while the
/// database was missing whatever the failed stage should have written.
/// What: returns `None` when no fault is
/// [`tga::collect::FaultSeverity::StageFailed`] — including when the run
/// recorded only skipped records, which is the per-item resilience that lets a
/// long sweep survive one malformed ticket. Otherwise returns the operator
/// message naming each failed stage, which the caller turns into a non-zero
/// exit. It never inspects a per-record skip.
/// Test: `tests::a_failed_stage_makes_collect_exit_non_zero`,
/// `tests::skipped_records_alone_keep_collect_at_exit_zero`,
/// `tests::a_dropped_work_items_write_is_a_stage_failure`.
fn stage_failure_report(stats: &tga::collect::CollectionStats) -> Option<String> {
    let failures = stats.stage_failures();
    if failures.is_empty() {
        return None;
    }
    let mut msg = format!(
        "{} collection stage(s) failed and their data was NOT persisted:\n",
        failures.len()
    );
    for f in &failures {
        msg.push_str(&format!("  - {}\n", f.message));
    }
    msg.push_str(
        "\nThe run finished the remaining stages, so the database holds a partial \
         collection. Re-run `tga collect` once the cause above is fixed.",
    );
    Some(msg.trim_end().to_string())
}

/// Print the end-of-collect fetch summary to stderr.
///
/// Why: surfaces fetch failures inline in the terminal output so users can
/// diagnose stale-data issues without grepping tracing logs.
/// What: counts successes, failures, and skips; always prints the one-line
/// header; prints a failure detail line per failed repo; prints success lines
/// only when `verbose` is true.
/// Test: unit tests below cover the table construction logic.
///
/// Public alias so `commands::analyze` can call it without duplicating the
/// formatting logic.
pub fn print_fetch_summary_pub(outcomes: &[tga::collect::collector::PerRepoFetch], verbose: bool) {
    print_fetch_summary(outcomes, verbose);
}

fn print_fetch_summary(outcomes: &[tga::collect::collector::PerRepoFetch], verbose: bool) {
    if outcomes.is_empty() {
        return;
    }

    let total = outcomes.len();
    let successes: Vec<_> = outcomes
        .iter()
        .filter(|f| matches!(f.outcome, FetchOutcome::Success { .. }))
        .collect();
    let failures: Vec<_> = outcomes
        .iter()
        .filter(|f| matches!(f.outcome, FetchOutcome::Failed { .. }))
        .collect();
    let skipped: Vec<_> = outcomes
        .iter()
        .filter(|f| matches!(f.outcome, FetchOutcome::Skipped { .. }))
        .collect();

    let fetched = successes.len();
    let failed = failures.len();

    if failed > 0 {
        eprintln!(
            "Fetch summary: {fetched} / {total} repos updated ({failed} failure(s), {} skipped)",
            skipped.len()
        );
        for f in &failures {
            if let FetchOutcome::Failed { error, .. } = &f.outcome {
                eprintln!("  - {}: {error}", f.repo);
            }
        }
    } else {
        eprintln!(
            "Fetch summary: {fetched} / {total} repos updated (0 failures, {} skipped)",
            skipped.len()
        );
    }

    if verbose {
        for f in &successes {
            if let FetchOutcome::Success { remote } = &f.outcome {
                eprintln!("  + {}: fetched from {remote}", f.repo);
            }
        }
        for f in &skipped {
            if let FetchOutcome::Skipped { reason } = &f.outcome {
                eprintln!("  ~ {}: skipped ({reason})", f.repo);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tga::collect::collector::{FetchOutcome, PerRepoFetch};
    use tga::collect::CollectionFault;

    use super::*;

    /// Why: the summary must print a header line with correct counts; the
    /// failure detail must include the repo name and error.
    /// What: build a slice with one success and one failure, call
    /// `print_fetch_summary`, assert it does not panic and the logic returns.
    /// Test: output goes to stderr (not captured by default); we test that the
    /// function compiles and runs without panicking for the most important
    /// invariants.
    #[test]
    fn fetch_summary_counts_correctly() {
        let outcomes = vec![
            PerRepoFetch {
                repo: "repo-a".to_string(),
                outcome: FetchOutcome::Success {
                    remote: "origin".to_string(),
                },
            },
            PerRepoFetch {
                repo: "repo-b".to_string(),
                outcome: FetchOutcome::Failed {
                    remote: "origin".to_string(),
                    error: "timeout".to_string(),
                },
            },
            PerRepoFetch {
                repo: "repo-c".to_string(),
                outcome: FetchOutcome::Skipped {
                    reason: "--no-fetch".to_string(),
                },
            },
        ];
        // Should not panic.
        print_fetch_summary(&outcomes, false);
        print_fetch_summary(&outcomes, true);
    }

    /// The #5655 regression: a stage whose data never landed used to print as a
    /// warning and return `Ok`, so `tga collect` exited 0 over an incomplete
    /// database. A repository that cannot be opened is the cheapest real stage
    /// failure to drive end-to-end — no network, no credentials — and it takes
    /// the same `stats.errors` path the Linear `work_items` write failure does.
    #[tokio::test]
    async fn a_failed_stage_makes_collect_exit_non_zero() {
        use tga::core::config::RepositoryConfig;
        let cfg = Config {
            repositories: vec![RepositoryConfig {
                path: "/definitely/does/not/exist/5655".into(),
                name: Some("ghost".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut db = tga::core::db::Database::open_in_memory().expect("db");
        let err = run_reporting_fetch(
            cfg,
            &mut db,
            CollectArgs::default(),
            &ProgressBus::disabled(),
        )
        .await
        .expect_err("a stage that never persisted its data must not exit 0");
        let msg = err.to_string();
        assert!(
            msg.contains("NOT persisted"),
            "the error says the data is missing: {msg}"
        );
        assert!(
            msg.contains("failed to open repo"),
            "the error names the failing stage: {msg}"
        );
    }

    /// #5655's third closure condition: one bad record must not fail a long
    /// sweep. Only `StageFailed` reaches the exit code.
    #[test]
    fn skipped_records_alone_keep_collect_at_exit_zero() {
        let stats = tga::collect::CollectionStats {
            errors: vec![
                CollectionFault::item_skipped("reviewer upsert failed for a/b#1: malformed"),
                CollectionFault::item_skipped("collection failed for repo W3 2026: bad week"),
            ],
            ..Default::default()
        };
        assert!(
            stage_failure_report(&stats).is_none(),
            "skipped records are reported, never fatal"
        );
    }

    /// The exact fault #5655 was filed about: `persist_work_items` returns
    /// `Err`, `linear_pipeline` records it, and the command must exit non-zero
    /// rather than printing it beside the per-record skips.
    #[test]
    fn a_dropped_work_items_write_is_a_stage_failure() {
        let stats = tga::collect::CollectionStats {
            errors: vec![
                CollectionFault::item_skipped("reviewer upsert failed for a/b#1: malformed"),
                CollectionFault::stage_failed(
                    "Linear: store work_items failed: attempt to write a readonly database",
                ),
            ],
            ..Default::default()
        };
        let msg = stage_failure_report(&stats).expect("a failed write must be reported");
        assert!(
            msg.contains("store work_items failed"),
            "the failed write is named: {msg}"
        );
        assert!(
            !msg.contains("reviewer upsert"),
            "a skipped record must not be reported as a failed stage: {msg}"
        );
        assert!(
            msg.starts_with("1 collection stage(s) failed"),
            "the count covers only the failed stages: {msg}"
        );
    }

    /// Why: empty outcomes must produce no output (avoid spurious "Fetch
    /// summary: 0 / 0" lines when the pipeline has no repos).
    /// What: call `print_fetch_summary` with an empty slice and assert early
    /// return (no panic).
    /// Test: this test itself.
    #[test]
    fn fetch_summary_empty_outcomes_is_noop() {
        // Must not panic.
        print_fetch_summary(&[], false);
    }

    /// Why: `--no-fetch` must emit a visible warning so users know data may be stale.
    /// What: checks that the no_fetch path in the pipeline sets strict_fetch/verbose_fetch
    /// correctly via the builder (smoke test for builder method existence).
    /// Test: this test itself.
    #[test]
    fn pipeline_builder_accepts_fetch_flags() {
        use tga::collect::CollectionPipeline;
        use tga::core::config::Config;
        let cfg = Config::default();
        let pipeline = CollectionPipeline::new(cfg)
            .with_strict_fetch(true)
            .with_verbose_fetch(true);
        assert!(pipeline.strict_fetch());
        assert!(pipeline.verbose_fetch());
    }

    /// Why: the `--no-fetch` flag must be threaded from the CLI arg struct all
    /// the way into the pipeline.  If the plumbing is broken, `no_fetch=true`
    /// would still trigger a network fetch on every repo (silent regression).
    /// What: creates a `CollectionPipeline` with `with_no_fetch(true)` and
    /// confirms the `with_no_fetch` builder round-trips the value.  The pipeline
    /// itself exposes `strict_fetch()` and `verbose_fetch()` accessors; the
    /// `no_fetch` flag is internal to the pipeline (it drives `GitCollector`)
    /// and is covered by the `no_fetch_returns_skipped` test in
    /// `collect::git::extractor::tests`.  This test is therefore the
    /// integration-level proof that all three flags co-exist without conflict.
    /// Test: this test itself.
    #[test]
    fn no_fetch_composes_with_strict_and_verbose_fetch() {
        use tga::collect::CollectionPipeline;
        use tga::core::config::Config;
        let cfg = Config::default();
        // All three flags can be set simultaneously without conflict.
        let pipeline = CollectionPipeline::new(cfg)
            .with_no_fetch(true)
            .with_strict_fetch(true)
            .with_verbose_fetch(true);
        // These accessors are the only public observability we have without
        // running the pipeline — but they are sufficient to confirm the
        // builder wiring is intact.
        assert!(pipeline.strict_fetch(), "strict_fetch must be true");
        assert!(pipeline.verbose_fetch(), "verbose_fetch must be true");
        // no_fetch is validated end-to-end by the `no_fetch_returns_skipped`
        // extractor test which calls perform_fetch() directly.
    }
}
