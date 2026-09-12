//! `tm repair savings-ledger` — quarantine test-written savings rows (#7569).
//!
//! Why: the operator's ledger carries 228 rows that unit tests appended
//! through the #7514 resolver bug, and they dominate the `💸` statusline
//! average. #7514's fix stops new ones; nothing retracts these. The repair
//! lands here rather than under `tm doctor --fix` because it needs the
//! dry-run-by-default / `--apply` pair `tm doctor` has no surface for, and
//! because `tm repair session-store` already established this verb as the home
//! for a one-shot, back-up-then-rewrite recovery of a persisted file.
//! What: [`repair_savings_ledger`] resolves the framework root, plans the
//! repair, prints the verdict, and — only with `--apply` — performs it.
//! Since #7658 the same plan/apply pair also collapses exact-duplicate
//! `instruction-compression` rows — one live session accumulated 312 copies of
//! one measurement — keeping the earliest copy of each `(session_id, basis)`.
//! Test: `repair_savings_ledger_dry_run_writes_nothing`,
//! `repair_savings_ledger_applies_and_is_idempotent`; the collapse rule itself
//! is `plan_collapses_duplicate_rows`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::core::savings::savings_log_in;
use trusty_mpm::core::savings_repair::{LedgerPlan, Reason, apply, count_markers, plan};

/// How many matching rows the dry run spells out before summarising.
const SAMPLE_ROWS: usize = 5;

/// Resolve the framework root this command operates under.
///
/// Why: `--root` is what lets an operator rehearse the repair against a copy,
/// and what lets the behaviour tests run without touching a real ledger.
/// What: `--root` when given, else [`FrameworkPaths::default`].
/// Test: `repair_savings_ledger_dry_run_writes_nothing`.
pub(crate) fn resolve_root(explicit: Option<String>) -> PathBuf {
    match explicit {
        Some(root) => PathBuf::from(root),
        None => FrameworkPaths::default().root,
    }
}

/// `tm repair savings-ledger` handler.
///
/// Why: see the module doc.
/// What: prints the planned verdict — counts, the first [`SAMPLE_ROWS`]
/// matching rows, and the line numbers needing structural repair — then stops
/// unless `apply` is set. With `apply` it moves the matched rows to a
/// timestamped sidecar and rewrites the ledger atomically; with `markers` it
/// also moves the `usage/no-fold-warned/` directory aside, and only then is the
/// marker directory read at all. A clean ledger is reported as such and left
/// alone, which is a success rather than an error. A row the live producer
/// appended mid-repair is carried across and reported, since it was never
/// classified — see [`trusty_mpm::core::savings_repair::apply`].
/// Test: `repair_savings_ledger_dry_run_writes_nothing`,
/// `repair_savings_ledger_applies_and_is_idempotent`.
pub(crate) fn repair_savings_ledger(
    root: Option<String>,
    apply_it: bool,
    markers: bool,
) -> Result<()> {
    let root = resolve_root(root);
    let ledger = savings_log_in(&root);
    let planned =
        plan(&ledger).with_context(|| format!("reading savings ledger {}", ledger.display()))?;

    report(&ledger, &planned);
    if markers {
        // #7569: counted only when asked for, so an unreadable marker directory
        // cannot fail a plain dry run of the ledger.
        let marker_count = count_markers(&root)
            .with_context(|| format!("counting markers under {}", root.display()))?;
        println!("  markers:          {marker_count} file(s) under usage/no-fold-warned/");
    }

    if !apply_it {
        println!("--dry-run (default): nothing was written; re-run with --apply to repair");
        return Ok(());
    }

    if planned.is_clean() {
        println!("ledger is already clean — nothing was written");
    } else {
        let applied = apply(&ledger, &planned, chrono::Utc::now())
            .with_context(|| format!("repairing savings ledger {}", ledger.display()))?;
        println!(
            "moved {} row(s) to {}",
            applied.quarantined,
            applied.quarantine.display()
        );
        println!("rewrote the ledger with {} row(s)", applied.kept);
        if applied.carried > 0 {
            // #7569: a live producer appended while the repair ran.
            println!(
                "carried {} row(s) appended during the repair — re-run to classify them",
                applied.carried
            );
        }
    }

    if markers {
        match trusty_mpm::core::savings_repair::quarantine_markers(&root, chrono::Utc::now())
            .with_context(|| format!("quarantining markers under {}", root.display()))?
        {
            Some((moved, count)) => {
                println!("moved {count} marker(s) to {}", moved.display());
            }
            None => println!("no usage/no-fold-warned/ directory — nothing to move"),
        }
    }
    Ok(())
}

/// Print the planned verdict.
///
/// What: the counts, a sample of the matching rows, and the physical lines that
/// need structural repair — the three things an operator needs to authorise the
/// apply.
/// Test: `repair_savings_ledger_dry_run_writes_nothing`.
fn report(ledger: &std::path::Path, planned: &LedgerPlan) {
    let kept = planned.kept().count();
    let quarantined = planned.quarantined().count();
    println!("savings ledger {}", ledger.display());
    println!(
        "  read:             {} line(s), {} row(s)",
        planned.lines_read,
        kept + quarantined
    );
    println!("  would keep:       {kept} row(s)");
    // #7658: `duplicate` is the third class — repeats of one
    // instruction-compression measurement, collapsed to the earliest copy.
    println!(
        "  would quarantine: {quarantined} row(s) — {} test-fixture, {} malformed, {} duplicate",
        planned.count_of(Reason::TestFixture),
        planned.count_of(Reason::Malformed),
        planned.count_of(Reason::Duplicate)
    );
    // #7569: these lines held more or less than one parseable row, which is a
    // different finding from the `malformed` row count two lines above — most
    // of them hold two VALID rows with no separator.
    println!(
        "  split/repaired:   {}",
        number_list(&planned.repaired_lines)
    );
    println!("  blank lines:      {}", number_list(&planned.blank_lines));

    for fragment in planned.quarantined().take(SAMPLE_ROWS) {
        let reason = fragment.quarantine.map_or("?", Reason::label);
        println!(
            "    line {:<6} {reason:<13} {}",
            fragment.line, fragment.text
        );
    }
    if quarantined > SAMPLE_ROWS {
        println!("    … and {} more", quarantined - SAMPLE_ROWS);
    }
}

/// Render line numbers for the report, or `none`.
fn number_list(numbers: &[usize]) -> String {
    if numbers.is_empty() {
        return "none".to_string();
    }
    numbers
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
#[path = "repair_savings_ledger_tests.rs"]
mod repair_savings_ledger_tests;
