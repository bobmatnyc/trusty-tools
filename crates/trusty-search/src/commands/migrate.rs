//! Handler for `trusty-search migrate mcp-vector-search`.
//!
//! Why: migrating away from mcp-vector-search has two distinct sides — the
//! project indexes (handled by reusing `convert.rs`) *and* the Claude MCP
//! configuration files that still point at the old `mcp-vector-search` server
//! command. `convert` only does indexes; `migrate` does both so a user can
//! switch tools in a single command.
//! What: `handle_migrate` orchestrates an MCP-config rewrite phase and an
//! index-migration phase, each independently skippable.
//! Test: `cargo run -- migrate mcp-vector-search --dry-run` prints the
//! settings files and projects it would touch without modifying anything.

use super::convert::{
    convert_one, fail_on_failed_rows, find_all_mvs_configs, parse_mvs_config, summary_glyph,
    ConvertStatus,
};
use anyhow::Result;
use clap::ValueEnum;
use colored::Colorize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use trusty_common::claude_config::{
    default_settings_max_depth, discover_claude_settings, mcp_server_entry, write_json_atomic,
};
use trusty_search::service::daemon_client::DaemonClient;

/// The MCP server keys (legacy spellings) we replace with `trusty-search`.
const LEGACY_MCP_KEYS: &[&str] = &["mcp-vector-search", "mcp_vector_search"];

/// The canonical key written for the migrated trusty-search MCP server.
const TRUSTY_KEY: &str = "trusty-search";

/// What the user is migrating *from*.
///
/// Why: model the migration source as an enum (validated at parse time by
/// clap) so additional sources can be added without changing the CLI surface.
/// What: a single variant today — `mcp-vector-search`.
/// Test: `cargo run -- migrate bogus` → clap rejects with a usage hint.
#[derive(Debug, Clone, ValueEnum)]
pub enum MigrateTarget {
    /// Migrate from mcp-vector-search (MCP config + project indexes)
    McpVectorSearch,
}

/// Outcome of attempting to migrate one Claude settings file.
///
/// Why: the summary table needs to distinguish a real rewrite from a no-op
/// skip (already migrated / nothing to do) and a hard failure.
/// What: enumerates the four terminal states of `migrate_config_file`.
/// Test: unit tests assert `Migrated`, `AlreadyMigrated`, and `NoChange`.
#[derive(Debug, PartialEq, Eq)]
pub enum ConfigMigrateStatus {
    /// The file contained a legacy key and was rewritten.
    Migrated,
    /// The file already contained a `trusty-search` key — left untouched.
    AlreadyMigrated,
    /// No legacy key and no trusty-search key — left untouched.
    NoChange,
    /// An IO/parse error occurred.
    Failed(String),
}

/// Result of migrating one Claude settings file (path + terminal status).
///
/// Why: pairs the file path with its outcome so the summary renderer can
/// print one line per file.
/// What: returned by `migrate_config_file`.
/// Test: unit tests inspect `status` after rewriting fixture files.
#[derive(Debug)]
pub struct ConfigMigrateResult {
    pub path: PathBuf,
    pub status: ConfigMigrateStatus,
}

/// Entry point for `trusty-search migrate`.
///
/// Why: a single command that switches a machine from mcp-vector-search to
/// trusty-search, touching both Claude MCP config and project indexes.
/// What: runs the MCP-config phase and/or the index phase depending on the
/// `--mcp-only` / `--indexes-only` flags. The MCP phase runs first, so a
/// failed index row (an `Err` from the index phase, #9214) never skips it.
/// Test: `migrate mcp-vector-search --dry-run` prints both phases' plans.
pub async fn handle_migrate(
    target: MigrateTarget,
    dry_run: bool,
    mcp_only: bool,
    indexes_only: bool,
) -> Result<()> {
    // `target` has one variant today; the match keeps future sources explicit.
    match target {
        MigrateTarget::McpVectorSearch => {}
    }

    if dry_run {
        println!(
            "{} Dry run — no files or indexes will be modified.\n",
            "·".dimmed()
        );
    }

    if !indexes_only {
        run_mcp_phase(dry_run)?;
    }

    if !mcp_only {
        if !indexes_only {
            println!();
        }
        run_index_phase(dry_run).await?;
    }

    Ok(())
}

/// MCP-config migration phase: scan + rewrite every Claude settings file.
///
/// Why: keeps the config-rewrite orchestration (scan → migrate → summarize)
/// separate from the async index phase.
/// What: locates settings files, migrates each, prints a summary table.
/// Test: covered indirectly by `--dry-run` runs and the unit tests below.
fn run_mcp_phase(dry_run: bool) -> Result<()> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?;
    println!(
        "🔍 Scanning for Claude MCP settings under {}…",
        home.display()
    );

    let files = discover_claude_settings(&home, default_settings_max_depth());
    if files.is_empty() {
        println!("{} No Claude settings files found.", "·".dimmed());
        return Ok(());
    }
    println!("{} Found {} settings file(s).\n", "·".dimmed(), files.len());

    let mut migrated = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for (i, path) in files.iter().enumerate() {
        let result = migrate_config_file(path, dry_run);
        print_config_line(i + 1, files.len(), &result);
        match result.status {
            ConfigMigrateStatus::Migrated => migrated += 1,
            ConfigMigrateStatus::AlreadyMigrated | ConfigMigrateStatus::NoChange => skipped += 1,
            ConfigMigrateStatus::Failed(_) => failed += 1,
        }
    }

    println!();
    if dry_run {
        println!(
            "{} MCP config dry run: {} would migrate, {} skipped, {} failed",
            "·".dimmed(),
            migrated,
            skipped,
            failed
        );
    } else {
        println!(
            "{} MCP config: {} migrated, {} skipped, {} failed",
            "✓".green(),
            migrated,
            skipped,
            failed
        );
    }
    Ok(())
}

/// Index-migration phase: reuse the `convert all` logic.
///
/// Why: index migration is identical to `convert all`; rather than duplicate
/// the discovery + register/reindex calls, we call the shared `convert.rs`
/// helpers.
/// What: scans `$HOME` for mcp-vector-search configs and (unless dry-run)
/// registers + reindexes each via the daemon socket (#9214).
/// Test: `migrate mcp-vector-search --indexes-only --dry-run` enumerates
/// every detected project.
async fn run_index_phase(dry_run: bool) -> Result<()> {
    println!("🔍 Scanning for mcp-vector-search project indexes…");
    let configs = find_all_mvs_configs();
    if configs.is_empty() {
        println!("{} No mcp-vector-search projects found.", "·".dimmed());
        return Ok(());
    }
    println!("{} Found {} project(s).\n", "·".dimmed(), configs.len());

    let client = if dry_run {
        // A dry run never contacts the daemon, so it resolves no client.
        None
    } else {
        // #9214: start the daemon over its socket and stay on it.
        let client = DaemonClient::resolve()?;
        super::daemon_guard::ensure_daemon_up(&client).await?;
        Some(client)
    };
    migrate_index_configs(configs, client.as_ref()).await
}

/// Register and reindex each config in `configs`, print one row each and the
/// summary. `client: None` is a dry run.
///
/// Test: `migrate_index_phase_fails_when_a_conversion_failed`,
/// `migrate_index_phase_succeeds_when_every_conversion_succeeded`.
async fn migrate_index_configs(configs: Vec<PathBuf>, client: Option<&DaemonClient>) -> Result<()> {
    let dry_run = client.is_none();
    let total = configs.len();
    let mut migrated = 0usize;
    let mut already = 0usize;
    let mut dry = 0usize;
    let mut failed = 0usize;

    for (i, config_path) in configs.into_iter().enumerate() {
        let result = match parse_mvs_config(&config_path) {
            Ok((root, name)) => convert_one(root, name, client).await,
            Err(e) => {
                println!(
                    "  {} {} {} {}",
                    format!("[{}/{}]", i + 1, total).dimmed(),
                    "✗".red(),
                    config_path.display().to_string().dimmed(),
                    format!("(parse: {e})").red()
                );
                failed += 1;
                continue;
            }
        };
        print_index_line(i + 1, total, &result);
        match result.status {
            ConvertStatus::Queued => migrated += 1,
            ConvertStatus::AlreadyRegistered => already += 1,
            ConvertStatus::DryRun => dry += 1,
            ConvertStatus::Failed(_) => failed += 1,
        }
    }

    println!();
    if dry_run {
        println!("{} Index dry run: {} project(s)", "·".dimmed(), dry);
    } else {
        println!(
            "{} Indexes: {} queued, {} already registered, {} failed",
            summary_glyph(failed),
            migrated,
            already,
            failed
        );
    }
    fail_on_failed_rows(failed, total)
}

/// Rewrite one Claude settings file, replacing any legacy mcp-vector-search
/// MCP server entry with a `trusty-search` entry.
///
/// Why: this is the load-bearing surgery — it must preserve every unrelated
/// JSON key, be idempotent, and never corrupt the file on failure. The
/// atomic-write + backup mechanics live in `trusty_common::claude_config`;
/// this function adds the migration-specific concerns (detecting legacy
/// keys, removing them, and distinguishing the four terminal states for
/// the summary table).
/// What: classifies the file (NoChange / AlreadyMigrated / migration
/// needed). When migration is needed, removes any legacy mcp-vector-search
/// keys and inserts the canonical trusty-search entry in a single atomic
/// write via `trusty_common::claude_config::write_json_atomic`. The
/// trusty-search entry shape comes from `mcp_server_entry(TRUSTY_KEY,
/// &["serve"])`, matching `patch_mcp_server`'s upsert exactly so the two
/// helpers stay in lock-step.
/// Test: `test_migrate_config_replaces_key` and `test_migrate_config_idempotent`
/// assert the rewrite and the no-op-on-already-migrated behaviour.
pub fn migrate_config_file(path: &Path, dry_run: bool) -> ConfigMigrateResult {
    let fail = |msg: String| ConfigMigrateResult {
        path: path.to_path_buf(),
        status: ConfigMigrateStatus::Failed(msg),
    };
    let result = |status| ConfigMigrateResult {
        path: path.to_path_buf(),
        status,
    };

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return fail(format!("read: {e}")),
    };
    let mut root: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => return fail(format!("parse: {e}")),
    };

    let servers = match root.get_mut("mcpServers").and_then(Value::as_object_mut) {
        Some(s) => s,
        // No mcpServers block at all — nothing to migrate.
        None => return result(ConfigMigrateStatus::NoChange),
    };

    // Idempotency: a trusty-search entry already present means a previous run
    // (or the user) already migrated this file — never double-migrate.
    if servers.contains_key(TRUSTY_KEY) {
        return result(ConfigMigrateStatus::AlreadyMigrated);
    }

    let legacy_present = LEGACY_MCP_KEYS.iter().any(|k| servers.contains_key(*k));
    if !legacy_present {
        return result(ConfigMigrateStatus::NoChange);
    }

    // Drop every legacy key and insert the canonical trusty-search entry in
    // a single atomic write. We do not call `patch_mcp_server` here because
    // it would create its own `.bak` of the already-stripped file — we want
    // the user's pre-migration content as the backup, which means a single
    // read-modify-write through `write_json_atomic`.
    for k in LEGACY_MCP_KEYS {
        servers.remove(*k);
    }
    servers.insert(
        TRUSTY_KEY.to_string(),
        mcp_server_entry(TRUSTY_KEY, &["serve"]),
    );

    if dry_run {
        return result(ConfigMigrateStatus::Migrated);
    }

    match write_json_atomic(path, &root) {
        Ok(()) => result(ConfigMigrateStatus::Migrated),
        Err(e) => fail(format!("write: {e}")),
    }
}

/// Render one MCP-config result line for the summary table.
fn print_config_line(idx: usize, total: usize, r: &ConfigMigrateResult) {
    let prefix = format!("[{idx}/{total}]");
    let path = r.path.display().to_string();
    match &r.status {
        ConfigMigrateStatus::Migrated => println!("  {} {} {}", prefix.dimmed(), "✓".green(), path),
        ConfigMigrateStatus::AlreadyMigrated => println!(
            "  {} {} {} {}",
            prefix.dimmed(),
            "↻".cyan(),
            path.dimmed(),
            "(already migrated)".dimmed()
        ),
        ConfigMigrateStatus::NoChange => println!(
            "  {} {} {} {}",
            prefix.dimmed(),
            "·".dimmed(),
            path.dimmed(),
            "(no mcp-vector-search entry)".dimmed()
        ),
        ConfigMigrateStatus::Failed(msg) => println!(
            "  {} {} {} {}",
            prefix.dimmed(),
            "✗".red(),
            path.dimmed(),
            format!("({msg})").red()
        ),
    }
}

/// Render one index-migration result line for the summary table.
fn print_index_line(idx: usize, total: usize, r: &super::convert::ConvertResult) {
    let prefix = format!("[{idx}/{total}]");
    let path = r.path.display().to_string();
    match &r.status {
        ConvertStatus::Queued => println!(
            "  {} {} {:<24} → {}",
            prefix.dimmed(),
            "✓".green(),
            r.name,
            path.dimmed()
        ),
        ConvertStatus::AlreadyRegistered => println!(
            "  {} {} {:<24} → {} {}",
            prefix.dimmed(),
            "↻".cyan(),
            r.name,
            path.dimmed(),
            "(already registered, reindexing)".dimmed()
        ),
        ConvertStatus::DryRun => println!("  {} {:<24} {}", prefix.dimmed(), r.name, path.dimmed()),
        ConvertStatus::Failed(msg) => println!(
            "  {} {} {:<24} → {} {}",
            prefix.dimmed(),
            "✗".red(),
            r.name,
            path.dimmed(),
            format!("({msg})").red()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the scan must reliably locate both global and project-level
    /// `.claude` settings files regardless of nesting depth.
    #[test]
    fn test_scan_finds_settings_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();

        // Global settings.
        let global = home.join(".claude");
        std::fs::create_dir_all(&global).expect("mkdir global");
        std::fs::write(global.join("settings.json"), "{}").expect("write global");

        // Nested project settings (settings.local.json).
        let proj = home.join("code").join("my-proj").join(".claude");
        std::fs::create_dir_all(&proj).expect("mkdir proj");
        std::fs::write(proj.join("settings.local.json"), "{}").expect("write proj");

        // A noise dir that must be skipped.
        let noise = home.join("node_modules").join(".claude");
        std::fs::create_dir_all(&noise).expect("mkdir noise");
        std::fs::write(noise.join("settings.json"), "{}").expect("write noise");

        let found = discover_claude_settings(home, default_settings_max_depth());
        assert!(
            found.contains(&global.join("settings.json")),
            "global settings missing: {found:?}"
        );
        assert!(
            found.contains(&proj.join("settings.local.json")),
            "project settings missing: {found:?}"
        );
        assert!(
            !found
                .iter()
                .any(|p| p.starts_with(home.join("node_modules"))),
            "node_modules should be skipped: {found:?}"
        );
    }

    /// Why: the core surgery — a legacy key must be removed and the canonical
    /// trusty-search key inserted, while unrelated keys survive.
    #[test]
    fn test_migrate_config_replaces_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.local.json");
        let input = serde_json::json!({
            "theme": "dark",
            "mcpServers": {
                "mcp-vector-search": {
                    "command": "mcp-vector-search",
                    "args": ["serve"]
                },
                "other-server": { "command": "other" }
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&input).unwrap()).expect("write input");

        let result = migrate_config_file(&path, false);
        assert_eq!(result.status, ConfigMigrateStatus::Migrated);

        let rewritten: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let servers = rewritten["mcpServers"].as_object().unwrap();
        assert!(
            !servers.contains_key("mcp-vector-search"),
            "legacy key should be gone"
        );
        assert!(servers.contains_key("trusty-search"), "trusty key missing");
        assert!(
            servers.contains_key("other-server"),
            "unrelated server dropped"
        );
        assert_eq!(
            rewritten["theme"], "dark",
            "unrelated top-level key dropped"
        );
        assert_eq!(servers["trusty-search"]["command"], "trusty-search");
        assert_eq!(servers["trusty-search"]["args"][0], "serve");

        // Backup preserves multi-dot filename: settings.local.json.bak
        assert!(
            path.with_file_name("settings.local.json.bak").exists(),
            "backup file missing"
        );
    }

    /// Why: a file already carrying a trusty-search entry must be left
    /// untouched so repeated `migrate` runs are safe.
    #[test]
    fn test_migrate_config_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let input = serde_json::json!({
            "mcpServers": {
                "trusty-search": {
                    "command": "trusty-search",
                    "args": ["serve"]
                }
            }
        });
        let serialized = serde_json::to_string_pretty(&input).unwrap();
        std::fs::write(&path, &serialized).expect("write input");

        let result = migrate_config_file(&path, false);
        assert_eq!(result.status, ConfigMigrateStatus::AlreadyMigrated);

        // File must be byte-for-byte unchanged.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            serialized,
            "file should be untouched"
        );
        assert!(
            !path.with_file_name("settings.json.bak").exists(),
            "no backup should be written for a skipped file"
        );
    }

    /// Write one mcp-vector-search config per `(name, body)` under `dir`.
    fn mvs_configs(dir: &Path, rows: &[(&str, Option<&str>)]) -> Vec<PathBuf> {
        rows.iter()
            .map(|(name, body)| {
                let cfg = dir.join(name).join(".mcp-vector-search");
                std::fs::create_dir_all(&cfg).expect("config dir");
                let root = dir.join(name);
                let text = body.map_or_else(
                    || serde_json::json!({ "project_root": root }).to_string(),
                    str::to_string,
                );
                let path = cfg.join("config.json");
                std::fs::write(&path, text).expect("config file");
                path
            })
            .collect()
    }

    /// #9214 (fix-bar): an index phase with a failed row — a refused create, or
    /// a config that does not parse — is an error after the summary.
    #[tokio::test]
    async fn migrate_index_phase_fails_when_a_conversion_failed() {
        use crate::commands::mock_socket::mock_daemon;
        use trusty_common::uds::server::RpcError;
        use trusty_search::service::rpc::error::CODE_CONFLICT;

        let refusing =
            mock_daemon(|_, _| Err(RpcError::new(CODE_CONFLICT, "index_root_overlap"))).await;
        let accepting =
            mock_daemon(|_, _| Ok(serde_json::json!({ "created": true, "queued": true }))).await;
        for (daemon, body) in [(&refusing, None), (&accepting, Some("{not json"))] {
            let dir = tempfile::tempdir().expect("scratch dir");
            let configs = mvs_configs(dir.path(), &[("a", body)]);
            let err = migrate_index_configs(configs, Some(&daemon.client))
                .await
                .expect_err("a failed row must fail the phase");
            assert!(err.to_string().contains("1 of 1"), "{body:?}: {err}");
        }
    }

    /// #9214: an index phase whose every row queued still succeeds.
    #[tokio::test]
    async fn migrate_index_phase_succeeds_when_every_conversion_succeeded() {
        use crate::commands::mock_socket::mock_daemon;

        let dir = tempfile::tempdir().expect("scratch dir");
        let daemon =
            mock_daemon(|_, _| Ok(serde_json::json!({ "created": true, "queued": true }))).await;
        let configs = mvs_configs(dir.path(), &[("a", None), ("b", None)]);
        migrate_index_configs(configs, Some(&daemon.client))
            .await
            .expect("every row queued");
    }
}
