//! `tga backfill` — retroactive maintenance operations against the commits
//! table.
//!
//! These operations live outside the normal `collect → classify → report`
//! pipeline because they update existing rows in-place rather than
//! ingesting new data. Each subcommand supports `--dry-run`, in which case
//! it reports the number of rows that *would* change without writing.
//!
//! Uniform filter flags (`--repos`, `--weeks`, `--since`, `--until`) scope
//! the backfill to a specific slice of the database. `--branch` is **not**
//! applicable to backfill operations because commits in the database do not
//! carry branch attribution after the original collection walk.
//!
//! Sub-module layout:
//! - `types`  — clap arg structs (`BackfillArgs`, `BackfillSubcommand`, etc.) and `EffortRow`
//! - `effort` — effort scoring (db path, git path, persist, git notes, tshirt rebin)
//! - `flags`  — revert, ticket-ids, ticketed, ai-detection-commits, build SQL filter
//! - `misc`   — reachability, complexity, top-level, quality

mod effort;
mod effort_db;
mod effort_git;
mod flags;
mod misc;
mod types;

#[cfg(test)]
mod tests;

// Re-export the public CLI types so callers (`commands/mod.rs`) import from
// one location instead of digging into submodules.
pub use types::BackfillArgs;
use types::BackfillSubcommand;

use tga::core::config::Config;
use tga::core::db::Database;

/// Resolve the effective date window from global backfill filter flags.
///
/// Why: the `--weeks`, `--since`, and `--until` flags are declared globally
/// on `BackfillArgs` so they can scope any backfill subcommand uniformly.
/// What: returns `(since_rfc, until_rfc)` as optional RFC3339 strings, or
/// `(since_plain, until_plain)` if only plain ISO dates are provided.
/// Test: indirectly exercised by each backfill subcommand's date-scoped tests.
fn resolve_backfill_date_range(
    args: &BackfillArgs,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    use crate::commands::date_range::resolve_date_range;
    resolve_date_range(
        args.weeks,
        args.since.as_deref(),
        args.until.as_deref(),
        None,
    )
}

/// Dispatch entry point for the `tga backfill` subcommand.
///
/// Why: routes each backfill subcommand to its implementation, passing shared
/// state (config, db connection), the `--dry-run` flag, and the uniform
/// filter flags (--repos, --weeks, --since, --until).
/// What: matches on `args.subcommand` and calls the appropriate function.
/// Test: each variant has its own test module in `tests.rs`.
///
/// # Errors
///
/// Propagates database errors from the underlying queries.
pub async fn run(config: Config, db: &mut Database, args: BackfillArgs) -> anyhow::Result<()> {
    let (since, until) = resolve_backfill_date_range(&args)?;
    let repos = args.repos.clone();
    match args.subcommand {
        BackfillSubcommand::AiDetection => flags::backfill_ai_detection(db, args.dry_run),
        BackfillSubcommand::RevertFlags => flags::backfill_revert_flags(
            db,
            args.dry_run,
            &repos,
            since.as_deref(),
            until.as_deref(),
        ),
        BackfillSubcommand::TicketIds => {
            flags::backfill_ticket_ids(db, args.dry_run, &repos, since.as_deref(), until.as_deref())
        }
        BackfillSubcommand::Reachability => {
            misc::backfill_reachability(config, db, &repos, args.dry_run)
        }
        BackfillSubcommand::Effort(effort_args) => effort::backfill_effort(
            config,
            db,
            effort_args,
            &repos,
            since.as_deref(),
            until.as_deref(),
            args.dry_run,
        ),
        BackfillSubcommand::Complexity(complexity_args) => {
            misc::backfill_complexity(config, db, complexity_args, args.dry_run).await
        }
        BackfillSubcommand::Ticketed => {
            flags::backfill_ticketed(db, args.dry_run, &repos, since.as_deref(), until.as_deref())
        }
        BackfillSubcommand::AiDetectionCommits => flags::backfill_ai_detection_commits(
            db,
            args.dry_run,
            &repos,
            since.as_deref(),
            until.as_deref(),
        ),
        BackfillSubcommand::TopLevel => misc::backfill_top_level(db, args.dry_run),
        BackfillSubcommand::EffortTshirt => effort::backfill_effort_tshirt(db, args.dry_run),
        BackfillSubcommand::Quality => misc::backfill_quality(db, args.dry_run),
    }
}
