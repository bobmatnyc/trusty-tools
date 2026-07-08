//! `tga rules` — introspect the classification rule set.
//!
//! Three subcommands cover the most common operational questions an operator
//! asks while tuning rules:
//!
//! * `tga rules list` — show every rule the engine will load with the
//!   current config (defaults + any `--rules` overrides).
//! * `tga rules show <commit-sha>` — show the verdict + method recorded
//!   for a specific commit, so the operator can answer "why was this
//!   classified as X?" without joining tables by hand.
//! * `tga rules test "<message>"` — dry-run the cascade against a single
//!   commit message and print the verdict + which tier fired. Useful for
//!   verifying a new rule before re-classifying the corpus.

use clap::{Args, Subcommand, ValueEnum};

use tga::classify::classifier::{ClassificationEngine, ClassificationEngineConfig};
use tga::classify::rules::{default_rules, Rule};
use tga::classify::taxonomy::{TaxonomyRegistry, TopLevelCategory};
use tga::core::config::Config;
use tga::core::db::Database;

/// Arguments for `tga rules`.
#[derive(Args, Debug)]
#[command(
    about = "Introspect or validate the active classification rule set.",
    long_about = "Three subcommands for tuning and debugging the classification cascade:\n\n\
  tga rules list   -- print every rule the engine will load (default + overrides)\n\
  tga rules show   -- print the verdict and method recorded for a commit SHA\n\
  tga rules test   -- dry-run the cascade against a single commit message\n\n\
Rules are loaded in priority order: manual overrides (Tier 0) > external ticket\n\
sources (Tier 1) > regex rules (Tier 2) > LLM fallback (Tier 3). This command\n\
helps answer \"why was this commit classified as X?\" and \"will my new rule fire?\".",
    after_help = "EXAMPLES:\n\
  # Show every rule currently active (built-in + custom --rules file)\n\
  tga rules list\n\n\
  # Debug the verdict for a specific commit\n\
  tga rules show abc123def456\n\n\
  # Test a commit message against the current rule set\n\
  tga rules test \"fix: resolve null pointer in auth handler\"\n\n\
TIPS:\n\
  - Use `tga rules list --rules custom.yaml` to preview a new rule file.\n\
  - `tga rules show` reads from the DB; run classify first if the commit is new."
)]
pub struct RulesArgs {
    /// Subcommand to dispatch.
    #[command(subcommand)]
    pub subcommand: RulesSubcommand,
}

/// `tga rules` subcommands.
#[derive(Subcommand, Debug)]
pub enum RulesSubcommand {
    /// Print every rule the engine will load with the current config.
    List(ListArgs),
    /// Print the verdict + method recorded for a specific commit.
    Show(ShowArgs),
    /// Dry-run the cascade against a single commit message and print
    /// the verdict + which tier fired.
    Test(TestArgs),
}

/// Arguments for `tga rules list`.
#[derive(Args, Debug)]
pub struct ListArgs {
    /// Override the rules file (defaults to `classification.rules_file`).
    #[arg(long)]
    pub rules: Option<std::path::PathBuf>,
    /// Output format. `text` prints the human-readable rule table (default);
    /// `json` emits the subcategory-to-top-level taxonomy rollup so
    /// downstream consumers (e.g. cto-reports) can consume it instead of
    /// hand-copying a taxonomy dict (issue #2205).
    #[arg(long, value_enum, default_value_t = ListFormat::Text)]
    pub format: ListFormat,
}

/// Output format for `tga rules list`.
///
/// Why: the taxonomy rollup is a stable, machine-readable contract that
/// downstream ETL pipelines want to consume programmatically rather than
/// scraping the human-readable table (issue #2205).
/// What: `Text` (default) prints the existing fixed-width rule table;
/// `Json` prints the subcategory→top-level taxonomy rollup as JSON.
/// Test: `list_json_format_emits_taxonomy_rollup`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum ListFormat {
    /// Human-readable fixed-width rule table (default).
    #[default]
    Text,
    /// Machine-readable subcategory→top-level taxonomy rollup.
    Json,
}

/// Arguments for `tga rules show`.
#[derive(Args, Debug)]
pub struct ShowArgs {
    /// Commit SHA to look up. Accepts the full 40-char SHA stored in
    /// `commits.sha`. Short SHAs are not currently supported.
    pub commit_sha: String,
}

/// Arguments for `tga rules test`.
#[derive(Args, Debug)]
pub struct TestArgs {
    /// Commit message to classify.
    pub message: String,
    /// Treat the test commit as a merge commit (affects fuzzy heuristics).
    #[arg(long, default_value_t = false)]
    pub is_merge: bool,
    /// Override the rules file (defaults to `classification.rules_file`).
    #[arg(long)]
    pub rules: Option<std::path::PathBuf>,
}

/// Dispatch entry point for the `tga rules` subcommand.
///
/// # Errors
///
/// Propagates database, rule loading, or engine build errors from the
/// individual subcommand handlers.
pub fn run(config: Config, db: &Database, args: RulesArgs) -> anyhow::Result<()> {
    match args.subcommand {
        RulesSubcommand::List(a) => list(&config, a),
        RulesSubcommand::Show(a) => show(db, a),
        RulesSubcommand::Test(a) => test(&config, a),
    }
}

/// Implementation of `tga rules list`.
fn list(config: &Config, args: ListArgs) -> anyhow::Result<()> {
    match args.format {
        ListFormat::Json => print_taxonomy_json(config),
        ListFormat::Text => print_rule_table(config, args.rules.as_deref()),
    }
}

/// Print the subcategory→top-level taxonomy rollup as JSON (issue #2205).
///
/// Why: cto-reports hand-copies TGA's `built_in_defs()` rollup as a Python
/// dict that silently drifts whenever the taxonomy changes. Emitting the
/// registry as JSON gives downstream consumers a stable, scriptable
/// contract instead.
/// What: builds the effective [`TaxonomyRegistry`] (built-ins merged with
/// any `classification.custom_categories` from config, mirroring how the
/// classification engine resolves the same registry), then prints a JSON
/// object with `subcategory_to_top_level` (name -> `as_str_snake()` parent)
/// and `top_level_categories` (the canonical 7-value list, in order).
/// Test: `list_json_format_emits_taxonomy_rollup`.
fn print_taxonomy_json(config: &Config) -> anyhow::Result<()> {
    let custom = config
        .classification
        .as_ref()
        .map(|c| c.custom_categories.clone())
        .unwrap_or_default();
    let registry = TaxonomyRegistry::new(custom);

    let rollup: std::collections::BTreeMap<&str, &str> = registry
        .all()
        .iter()
        .map(|d| (d.name.as_str(), d.parent.as_str_snake()))
        .collect();
    let top_level: Vec<&str> = TopLevelCategory::all()
        .iter()
        .map(TopLevelCategory::as_str_snake)
        .collect();

    let payload = serde_json::json!({
        "subcategory_to_top_level": rollup,
        "top_level_categories": top_level,
    });
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

/// Print the human-readable fixed-width rule table (the pre-#2205 default
/// behaviour of `tga rules list`).
fn print_rule_table(config: &Config, cli_rules: Option<&std::path::Path>) -> anyhow::Result<()> {
    let ruleset = resolve_rules(config, cli_rules)?;
    let sorted = ruleset.by_priority();
    println!(
        "Loaded {} rule(s) (version: {})",
        sorted.len(),
        ruleset.version.as_deref().unwrap_or("?")
    );
    println!("(Higher priority fires first within a tier.)\n");
    println!(
        "{:<26} {:>4}  {:<18} {:<18}  kw  re   conf",
        "id", "prio", "category", "subcategory"
    );
    println!("{}", "-".repeat(86));
    for r in sorted {
        println!(
            "{:<26} {:>4}  {:<18} {:<18}  {:>2}  {:>2}  {:>4.2}",
            r.id,
            r.priority,
            r.category,
            r.subcategory.as_deref().unwrap_or("-"),
            r.keywords.len(),
            r.patterns.len(),
            r.confidence,
        );
    }
    Ok(())
}

/// One row returned by the join query in [`show`].
///
/// Why: clippy `type_complexity` would otherwise flag the inline tuple
/// returned from `query_row`.
/// What: holds the columns selected by [`show`] from `commits` joined to
/// `classifications`.
/// Test: indirectly covered by `show_subcommand_handles_missing_commit_gracefully`.
struct ShowRow {
    category: String,
    subcategory: Option<String>,
    confidence: f64,
    method: String,
    ticket_id: Option<String>,
    message: String,
}

/// Implementation of `tga rules show <sha>`.
fn show(db: &Database, args: ShowArgs) -> anyhow::Result<()> {
    let conn = db.connection();
    let row: Option<ShowRow> = conn
        .query_row(
            "SELECT cl.category, cl.subcategory, cl.confidence, cl.method, \
                    cl.ticket_id, c.message \
             FROM commits c \
             LEFT JOIN classifications cl ON cl.id = c.classification_id \
             WHERE c.sha = ?1",
            rusqlite::params![args.commit_sha],
            |r| {
                Ok(ShowRow {
                    category: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    subcategory: r.get(1)?,
                    confidence: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                    method: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    ticket_id: r.get(4)?,
                    message: r.get(5)?,
                })
            },
        )
        .ok();

    let Some(ShowRow {
        category,
        subcategory,
        confidence,
        method,
        ticket_id,
        message,
    }) = row
    else {
        println!("No commit found with SHA {}", args.commit_sha);
        return Ok(());
    };

    println!("Commit: {}", args.commit_sha);
    println!(
        "Message: {}",
        message.lines().next().unwrap_or("").trim_end()
    );
    if method.is_empty() {
        println!("Status: not classified (no classification_id)");
        println!("Hint: run `tga classify` to populate.");
        return Ok(());
    }
    println!("Verdict:");
    println!("  category    : {category}");
    if let Some(s) = subcategory {
        println!("  subcategory : {s}");
    }
    println!("  method      : {method}");
    println!("  confidence  : {confidence:.2}");
    if let Some(t) = ticket_id {
        println!("  ticket_id   : {t}");
    }
    Ok(())
}

/// Implementation of `tga rules test "<message>"`.
fn test(config: &Config, args: TestArgs) -> anyhow::Result<()> {
    let ruleset = resolve_rules(config, args.rules.as_deref())?;
    let engine_cfg = ClassificationEngineConfig::default();
    let custom_taxonomy = config
        .classification
        .as_ref()
        .map(|c| c.custom_categories.clone())
        .unwrap_or_default();
    let jira_mappings = config
        .jira
        .as_ref()
        .map(|j| j.jira_project_mappings.clone())
        .unwrap_or_default();

    let engine = ClassificationEngine::with_taxonomy_and_mappings(
        ruleset,
        engine_cfg,
        custom_taxonomy,
        jira_mappings,
        None,
    )?;

    println!("Message: {}", args.message);
    println!("is_merge: {}", args.is_merge);
    println!();

    match engine.classify_sync(&args.message, args.is_merge) {
        Some(verdict) => {
            println!("Verdict:");
            println!("  category    : {}", verdict.category);
            if let Some(s) = &verdict.subcategory {
                println!("  subcategory : {s}");
            }
            if let Some(t) = &verdict.top_level {
                println!("  top_level   : {t:?}");
            }
            println!("  method      : {}", verdict.method.as_str());
            println!("  confidence  : {:.2}", verdict.confidence);
            if let Some(id) = &verdict.ticket_id {
                println!("  ticket_id   : {id}");
            }
        }
        None => {
            println!("No tier matched. The async LLM tier (if enabled) would run next.");
        }
    }
    Ok(())
}

/// Resolve the effective ruleset for the current config + CLI override.
///
/// Why: every `tga rules` subcommand needs the same merge logic as the
/// pipeline (`load_rules` if a path is supplied, else `default_rules`,
/// with `extend_defaults` triggering a merge). Sharing the helper keeps
/// the introspection output identical to what the pipeline actually runs.
/// What: returns the resolved `RuleSet` ready for `by_priority()` or
/// engine construction.
/// Test: indirectly exercised by the unit tests below.
fn resolve_rules(
    config: &Config,
    cli_rules: Option<&std::path::Path>,
) -> anyhow::Result<tga::classify::rules::RuleSet> {
    // Collect the effective list of rule file paths. CLI --rules overrides
    // (or prepends) config rules_files for backward compat.
    let paths: Vec<std::path::PathBuf> = if let Some(cli) = cli_rules {
        vec![cli.to_path_buf()]
    } else {
        config
            .classification
            .as_ref()
            .map(|c| c.rules_files.clone())
            .unwrap_or_default()
    };

    let ruleset = if paths.is_empty() {
        default_rules()
    } else {
        use tga::classify::rules::load_rules_multi;
        let path_refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
        let custom = load_rules_multi(&path_refs)?;
        if custom.extend_defaults {
            let mut merged = default_rules();
            let custom_ids: std::collections::HashSet<String> =
                custom.rules.iter().map(|r: &Rule| r.id.clone()).collect();
            merged.rules.retain(|r| !custom_ids.contains(&r.id));
            merged.rules.extend(custom.rules);
            merged
        } else {
            custom
        }
    };
    Ok(ruleset)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `tga rules list` is the operator's primary debugging tool for
    /// a misbehaving ruleset; the resolve helper must return the same
    /// effective ruleset the pipeline uses.
    /// What: calls `resolve_rules` with no overrides and asserts the
    /// default ruleset is returned (non-empty and contains a known id).
    /// Test: pure-function exercise.
    #[test]
    fn resolve_rules_returns_defaults_without_override() {
        let cfg = Config::default();
        let rs = resolve_rules(&cfg, None).expect("resolve");
        assert!(!rs.rules.is_empty());
        assert!(rs.rules.iter().any(|r| r.id == "cc-feat"));
    }

    /// Why: `tga rules test` is a dry-run preview; it must surface the
    /// same verdict that `tga classify` would write.
    /// What: builds an engine over the defaults and asserts a known
    /// conventional commit message classifies as expected.
    /// Test: pure exercise of `classify_sync`.
    #[test]
    fn test_subcommand_classifies_conventional_commit_message() {
        let cfg = Config::default();
        let rs = resolve_rules(&cfg, None).expect("resolve");
        let engine = ClassificationEngine::with_taxonomy_and_mappings(
            rs,
            ClassificationEngineConfig::default(),
            Vec::new(),
            std::collections::HashMap::new(),
            None,
        )
        .expect("engine");
        let v = engine
            .classify_sync("feat: add login flow", false)
            .expect("verdict");
        assert_eq!(v.category, "feature");
    }

    /// Why: when the commit is not in the DB, the show subcommand must
    /// degrade gracefully (no panic, helpful message).
    /// What: opens an empty in-memory DB and calls `show` for a SHA that
    /// doesn't exist; expects no error and no panic.
    /// Test: smoke-level.
    #[test]
    fn show_subcommand_handles_missing_commit_gracefully() {
        let db = Database::open_in_memory().expect("db");
        let args = ShowArgs {
            commit_sha: "does-not-exist".into(),
        };
        show(&db, args).expect("show should not error on missing SHA");
    }

    /// Why: issue #2205 — cto-reports needs a machine-readable taxonomy
    /// rollup instead of hand-copying a Python dict. This is the primary
    /// regression test for the new `--format json` contract.
    /// What: builds the registry the same way `print_taxonomy_json` does
    /// and asserts the same rollup a known built-in subcategory resolves
    /// to, plus that the top-level list has exactly the 7 canonical
    /// snake_case values in stable order.
    /// Test: this test itself.
    #[test]
    fn list_json_format_emits_taxonomy_rollup() {
        let registry = TaxonomyRegistry::with_builtins();
        let rollup: std::collections::BTreeMap<&str, &str> = registry
            .all()
            .iter()
            .map(|d| (d.name.as_str(), d.parent.as_str_snake()))
            .collect();

        assert_eq!(rollup.get("feature"), Some(&"feature"));
        assert_eq!(rollup.get("security"), Some(&"bugfix"));
        assert_eq!(rollup.get("bug_fix"), Some(&"bugfix"));
        assert_eq!(rollup.get("documentation"), Some(&"content"));

        let top_level: Vec<&str> = TopLevelCategory::all()
            .iter()
            .map(TopLevelCategory::as_str_snake)
            .collect();
        assert_eq!(
            top_level,
            vec![
                "feature",
                "bugfix",
                "ktlo",
                "integrations",
                "platform_work",
                "content",
                "maintenance",
            ]
        );

        // The full payload must serialize cleanly (this is what
        // `print_taxonomy_json` actually prints).
        let payload = serde_json::json!({
            "subcategory_to_top_level": rollup,
            "top_level_categories": top_level,
        });
        let s = serde_json::to_string(&payload).expect("serialize");
        assert!(s.contains("\"feature\":\"feature\""));
    }

    /// Why: `--format` must default to `Text` so existing scripts/muscle
    /// memory relying on the human-readable table are unaffected by
    /// issue #2205.
    /// What: asserts `ListFormat::default()` is `Text`.
    /// Test: this test itself.
    #[test]
    fn list_format_defaults_to_text() {
        assert_eq!(ListFormat::default(), ListFormat::Text);
    }
}
