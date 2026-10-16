//! `tm worktrees [--json] [--no-size]` — the worktree ledger report (#8994).
//!
//! Why: the issue's report surface. `tm doctor`'s `worktree_registry` row
//! prints the same roll-up from the ledger alone; this command is also the
//! route that FEEDS the ledger the facts the row cannot take itself — the
//! backfill of pre-existing trees and the size measurement.
//! What: [`collect`] backfills from every registered project's
//! `git worktree list`, records `removed` for a live tree gone from disk and
//! from git, measures each live tree unless `--no-size`, links each tree's
//! pull request and lists stale candidates (#8994 S2c), folds the ledger and
//! returns a [`WorktreesReport`]; [`run`] prints it as text or JSON.
//! Daemon-less: it touches only `~/.trusty-mpm/`, git and `gh`, and deletes
//! nothing.
//! Test: `tm_worktrees_json_reports_count_and_gib_per_project`,
//! `tm_worktrees_records_removed_for_a_tree_gone_from_disk_and_git`,
//! `tm_worktrees_shows_the_pr_and_lists_stale_candidates`
//! (`tests/tm_worktrees_cli.rs`).

use std::path::{Path, PathBuf};

use serde::Serialize;
use trusty_mpm::core::paths::FRAMEWORK_DIR_NAME;
use trusty_mpm::core::worktree_ledger::WorktreeLedger;
use trusty_mpm::core::worktree_ledger::backfill::{
    BackfillReport, backfill_checkouts, registered_checkouts,
};
use trusty_mpm::core::worktree_ledger::fold::{ProjectSummary, fold, gib};
use trusty_mpm::core::worktree_ledger::hook_ingest::clear_sticky_ingest_failure;
use trusty_mpm::core::worktree_ledger::reconcile::{ReconcileReport, reconcile_removed};
use trusty_mpm::core::worktree_ledger::record::measure_live;
use trusty_mpm::core::worktree_ledger::stale::{StaleReason, TreeView, link_prs_and_find_stale};
use trusty_mpm::core::worktree_ledger::stale_probe::GitGhStaleProbe;

/// Flags for `tm worktrees`.
#[derive(Debug, Clone, clap::Args)]
pub(crate) struct WorktreesArgs {
    /// Print the report as JSON.
    #[arg(long)]
    pub json: bool,
    /// Skip measuring; report the sizes the ledger last recorded.
    #[arg(long)]
    pub no_size: bool,
}

/// The backfill half of the report.
#[derive(Debug, Serialize)]
pub(crate) struct BackfillSummary {
    /// `created` events appended.
    pub created: usize,
    /// `observed` events appended (agent-isolation trees).
    pub observed: usize,
    /// Trees already in the ledger.
    pub already_recorded: usize,
    /// Checkouts git could not list.
    pub unanswered: Vec<PathBuf>,
    /// The project registry could not be read, so no backfill ran.
    pub registry_error: Option<String>,
}

/// Grand totals across every project.
#[derive(Debug, Serialize)]
pub(crate) struct Totals {
    /// Live worktrees.
    pub count: usize,
    /// Sum of last measured sizes.
    pub bytes: u64,
    /// `bytes` in GiB, two decimals.
    pub gib: f64,
}

/// Everything `tm worktrees` reports.
#[derive(Debug, Serialize)]
pub(crate) struct WorktreesReport {
    /// The ledger file.
    pub ledger: PathBuf,
    /// Count and GiB per project.
    pub projects: Vec<ProjectSummary>,
    /// Totals.
    pub total: Totals,
    /// What the backfill appended.
    pub backfill: BackfillSummary,
    /// Trees measured this run; `None` under `--no-size`.
    pub measured: Option<usize>,
    /// What the reconcile pass recorded `removed`, and what it left unknown.
    pub reconcile: ReconcileReport,
    /// Live trees whose directory is gone but which the reconcile left live:
    /// git still lists them, or their repository could not be listed.
    pub missing: usize,
    /// Ledger lines that did not parse.
    pub malformed: usize,
    /// Every live tree with its PR (#8994 S2c).
    pub trees: Vec<TreeView>,
    /// The trees that are stale candidates — listed, never removed.
    pub stale_candidates: Vec<TreeView>,
    /// `pr_linked` events appended this run.
    pub pr_linked: usize,
}

/// Backfill, reconcile, optionally measure, then fold the ledger under `home`.
///
/// Why: `home` is an argument so the integration test drives a scratch home
/// through the real binary and nothing here reads the process environment.
/// What: registry → [`backfill_checkouts`] (then, when every checkout
/// answered, the sticky hook-ingest failure record is cleared) → [`reconcile_removed`] →
/// [`measure_live`] (when `measure`) → [`link_prs_and_find_stale`] → fold.
/// The reconcile runs under
/// `--no-size` too: it lists git, it never measures. A ledger error
/// propagates; an unreadable registry is reported in
/// [`BackfillSummary::registry_error`] and the rest still runs.
/// Test: `tm_worktrees_json_reports_count_and_gib_per_project`,
/// `tm_worktrees_records_removed_for_a_tree_gone_from_disk_and_git`,
/// `tm_worktrees_clears_only_a_sticky_failure_and_only_after_a_clean_backfill`,
/// `tm_worktrees_shows_the_pr_and_lists_stale_candidates`.
pub(crate) async fn collect(home: &Path, measure: bool) -> anyhow::Result<WorktreesReport> {
    let ledger = WorktreeLedger::under_home(home);
    let registry_dir = trusty_mpm::project::worktree_policy::registry_data_dir_under(
        &home.join(FRAMEWORK_DIR_NAME),
    );
    let (report, registry_error) = match registered_checkouts(&registry_dir).await {
        Ok(checkouts) => {
            let report = backfill_checkouts(&ledger, &checkouts)?;
            // #8994 S2b: a clean backfill records the tree a Bash call made
            // and the daemon missed, so it alone clears the sticky record.
            if report.unanswered.is_empty() {
                match clear_sticky_ingest_failure(&ledger) {
                    Ok(true) => eprintln!("tm worktrees: cleared the hook-ingest failure record"),
                    Ok(false) => {}
                    Err(e) => eprintln!("tm worktrees: could not clear the failure record: {e}"),
                }
            }
            (report, None)
        }
        Err(e) => (BackfillReport::default(), Some(e.to_string())),
    };
    // #8994: without this pass a reaped or removed tree stays live forever.
    let reconcile = reconcile_removed(&ledger, &fold(&ledger.read()?.events))?;
    let mut measured = None;
    if measure {
        let outcome = measure_live(&ledger, &fold(&ledger.read()?.events))?;
        measured = Some(outcome.measured);
    }
    // #8994 S2c: PR numbers and stale candidates; nothing is removed.
    let stale = link_prs_and_find_stale(
        &ledger,
        &fold(&ledger.read()?.events),
        &GitGhStaleProbe::default(),
    )?;
    let read = ledger.read()?;
    let projects = fold(&read.events).by_project();
    let bytes = projects.iter().map(|p| p.bytes).sum();
    Ok(WorktreesReport {
        ledger: ledger.path().to_path_buf(),
        total: Totals {
            count: projects.iter().map(|p| p.count).sum(),
            bytes,
            gib: gib(bytes),
        },
        projects,
        backfill: BackfillSummary {
            created: report.created,
            observed: report.observed,
            already_recorded: report.already_recorded,
            unanswered: report.unanswered,
            registry_error,
        },
        measured,
        missing: reconcile.missing(),
        reconcile,
        malformed: read.malformed,
        stale_candidates: stale.candidates().cloned().collect(),
        pr_linked: stale.pr_linked,
        trees: stale.trees,
    })
}

/// `tm worktrees` entry point.
pub(crate) async fn run(args: WorktreesArgs) -> anyhow::Result<()> {
    let home = dirs::home_dir()
        .filter(|h| h.is_absolute())
        .ok_or_else(|| anyhow::anyhow!("tm worktrees: no absolute home directory"))?;
    let report = collect(&home, !args.no_size).await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    print_text(&report);
    Ok(())
}

/// The per-tree lines and the stale candidates (#8994 S2c).
fn print_trees(report: &WorktreesReport) {
    for t in &report.trees {
        let pr = t.pr.map_or_else(|| "-".to_string(), |n| format!("PR #{n}"));
        let branch = t.branch.as_deref().unwrap_or("-");
        println!("       {pr:>9}  {branch}  {}", t.path.display());
    }
    if !report.stale_candidates.is_empty() {
        println!("stale candidates (listed only; nothing is removed):");
    }
    for t in &report.stale_candidates {
        let why = match &t.stale {
            Some(StaleReason::MergedPr { pr }) => format!("PR #{pr} merged"),
            Some(StaleReason::BranchGone) => "branch gone from origin, nothing unpushed".into(),
            None => continue,
        };
        println!("  stale candidate: {}  ({why})", t.path.display());
    }
    let unresolved = report
        .trees
        .iter()
        .filter(|t| !t.unresolved.is_empty())
        .count();
    if unresolved > 0 {
        eprintln!(
            "tm worktrees: {unresolved} tree(s) whose PR or branch lookup could not be answered \
             are not stale candidates (see --json)"
        );
    }
}

fn print_text(report: &WorktreesReport) {
    for p in &report.projects {
        let note = if p.unmeasured > 0 {
            format!("  ({} unmeasured)", p.unmeasured)
        } else {
            String::new()
        };
        println!(
            "{:>5}  {:>8.2} GiB  {}{note}",
            p.count,
            p.gib,
            p.repo.display()
        );
    }
    println!(
        "{:>5}  {:>8.2} GiB  total ({} project(s))",
        report.total.count,
        report.total.gib,
        report.projects.len()
    );
    print_trees(report);
    let b = &report.backfill;
    if b.created + b.observed > 0 {
        println!(
            "backfill: {} created, {} observed (agent isolation)",
            b.created, b.observed
        );
    }
    for checkout in &b.unanswered {
        eprintln!("tm worktrees: git could not list {}", checkout.display());
    }
    if let Some(e) = &b.registry_error {
        eprintln!("tm worktrees: project registry unreadable, no backfill ran: {e}");
    }
    if report.reconcile.removed > 0 {
        println!(
            "reconcile: {} tree(s) gone from disk and git recorded as removed",
            report.reconcile.removed
        );
    }
    for u in &report.reconcile.unlistable {
        eprintln!(
            "tm worktrees: cannot list {} ({}); its missing trees stay recorded",
            u.repo.display(),
            u.reason
        );
    }
    if report.missing > 0 {
        eprintln!(
            "tm worktrees: {} recorded tree(s) no longer on disk but still listed by git \
             or unlistable (not removed from the ledger)",
            report.missing
        );
    }
    if report.malformed > 0 {
        eprintln!(
            "tm worktrees: {} malformed ledger line(s) skipped in {}",
            report.malformed,
            report.ledger.display()
        );
    }
}
