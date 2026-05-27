//! `tga collect` — stage 1 (git extraction) entry point.

use tga::collect::CollectionPipeline;
use tga::core::config::Config;
use tga::core::db::Database;

use crate::commands::date_range::resolve_date_range;
use crate::CollectArgs;

/// Run the collection stage against the provided database.
///
/// Applies CLI overrides (repository filter, since/until dates) on top of
/// the loaded YAML configuration before invoking
/// [`CollectionPipeline::run`].
///
/// When `args.dry_run` is set, the pipeline executes against a transient
/// in-memory SQLite database so that the real `db` is left untouched. All
/// extraction, classification rule loading, and API fetches still run so
/// the user can preview the work that *would* have been persisted.
pub async fn run(config: Config, db: &mut Database, args: CollectArgs) -> anyhow::Result<()> {
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

    let pipeline = CollectionPipeline::new(cfg)
        .with_force(args.force)
        .with_no_fetch(args.no_fetch)
        .with_force_refresh_prs(args.force_refresh_prs)
        .with_skip_tag_reachability(args.skip_tag_reachability);

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
        println!(
            "Collected {} commits from {} authors ({} PRs fetched, \
             {} weeks collected, {} weeks skipped)",
            stats.commits_collected,
            stats.authors_resolved,
            stats.prs_fetched,
            stats.weeks_collected,
            stats.weeks_skipped,
        );
    }
    if !stats.errors.is_empty() {
        eprintln!(
            "Encountered {} warnings during collection:",
            stats.errors.len()
        );
        for e in &stats.errors {
            eprintln!("  warning: {e}");
        }
    }
    Ok(())
}
