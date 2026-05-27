//! `tga classify` — stage 2 (classification cascade) entry point.

use tga::classify::ClassificationPipeline;
use tga::core::config::{ClassificationConfig, Config};
use tga::core::db::Database;

use crate::ClassifyArgs;

/// Run the classification stage over previously-collected commits.
///
/// Why: wire CLI flags (`--rules`, `--use-llm`, `--force`, `--since`,
/// `--no-external`) into the [`ClassificationPipeline`] without exposing the
/// pipeline internals in `main.rs`.
/// What: mutates the `ClassificationConfig` section of the loaded YAML config
/// to honor each override, then builds and runs the pipeline.
/// Test: integration-tested via pipeline unit tests in `classify::pipeline::tests`.
pub async fn run(config: Config, db: &mut Database, args: ClassifyArgs) -> anyhow::Result<()> {
    let mut cfg = config;

    // Ensure a classification section exists so overrides have somewhere to land.
    if cfg.classification.is_none() && (args.rules.is_some() || args.use_llm) {
        cfg.classification = Some(ClassificationConfig::default());
    }
    if let Some(ref mut c) = cfg.classification {
        if let Some(rules) = args.rules {
            c.rules_file = Some(rules);
        }
        if args.use_llm {
            c.use_llm = true;
        }
        // When --no-external is passed, suppress all external classification
        // sources for this run regardless of what the rules file configures.
        if args.no_external {
            c.no_external = true;
        }
    }

    // --since without --force is a no-op (default flow already skips
    // classified rows). Flag this rather than silently ignoring it so the
    // operator notices the missing `--force`.
    if args.since.is_some() && !args.force {
        tracing::warn!(
            "--since was supplied without --force; ignoring it. \
             Pass --force to re-classify commits already in the DB."
        );
    }

    let pipeline = ClassificationPipeline::new(cfg)
        .with_force(args.force)
        .with_since(args.since.clone());

    // Backfill mode: fill in only the missing complexity scores and return,
    // leaving existing category/confidence/method verdicts untouched.
    if args.backfill_complexity {
        let updated = pipeline.backfill_complexity(db).await?;
        println!("Backfilled complexity for {updated} commit(s)");
        return Ok(());
    }

    let stats = pipeline.run(db).await?;

    println!(
        "Classified {}/{} commits ({:.1}% coverage)",
        stats.classified, stats.total_commits, stats.coverage_pct
    );
    if !stats.by_method.is_empty() {
        println!("By method:");
        for (method, count) in &stats.by_method {
            println!("  {method}: {count}");
        }
    }
    if !stats.by_category.is_empty() {
        println!("By category:");
        for (category, count) in &stats.by_category {
            println!("  {category}: {count}");
        }
    }
    Ok(())
}
