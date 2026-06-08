//! Handler for `trusty-memory migrate kuzu-memory`.
//!
//! Why: users currently running the kuzu-memory MCP server need a one-command
//! switch to trusty-memory that rewrites every Claude settings file referring
//! to the legacy server, with the same idempotency / atomic-write semantics as
//! `trusty-search migrate mcp-vector-search`. Doing this by hand across global
//! and per-project settings files is error-prone, so the binary owns it.
//! What: `handle_migrate` walks every discovered Claude settings file via
//! `trusty_common::claude_config`, swaps any `kuzu-memory` / `kuzu_memory`
//! `mcpServers` entry for a canonical `trusty-memory` entry, and prints a
//! summary table. `--dry-run` prints the plan without writing. `--config-only`
//! is accepted for parity with `trusty-search migrate` (today the migration
//! has no other phase so it is effectively a no-op flag).
//! Test: unit tests cover (a) a vanilla rewrite preserving unrelated keys,
//! (b) idempotency when a `trusty-memory` entry is already present, and
//! (c) the no-op case when no legacy key is present.
//!
//! Run manually with: `cargo run -p trusty-memory -- migrate kuzu-memory --dry-run`.

use anyhow::Result;
use clap::ValueEnum;
use colored::Colorize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use trusty_common::claude_config::{
    default_settings_max_depth, discover_claude_settings, mcp_server_entry, write_json_atomic,
};

/// The MCP server keys (both spellings) we replace with `trusty-memory`.
///
/// Why: we have seen both `kuzu-memory` and `kuzu_memory` in the wild in
/// `mcpServers` blocks; treat them as equivalent legacy aliases.
/// What: a static slice scanned for membership inside each settings file.
/// Test: `test_migrate_config_replaces_dashed_key` and
/// `test_migrate_config_replaces_underscored_key` cover both forms.
const LEGACY_MCP_KEYS: &[&str] = &["kuzu-memory", "kuzu_memory"];

/// The canonical key written for the migrated trusty-memory MCP server.
const TRUSTY_KEY: &str = "trusty-memory";

/// What the user is migrating *from*.
///
/// Why: model the migration source as an enum (validated at parse time by
/// clap) so additional sources can be added later without changing the
/// CLI surface.
/// What: two variants today — `kuzu-memory` (config migration) and
/// `kuzu-data` (data migration from a kuzu-memory `store.redb`).
/// Test: `cargo run -p trusty-memory -- migrate bogus` → clap rejects with
/// a usage hint.
#[derive(Debug, Clone, ValueEnum)]
pub enum MigrateTarget {
    /// Migrate from kuzu-memory (rewrites Claude `mcpServers` entries).
    KuzuMemory,
    /// Import entity/relation data from a kuzu-memory `store.redb` file into
    /// a trusty-memory palace.
    #[value(name = "kuzu-data")]
    KuzuData,
}

/// Outcome of attempting to migrate one Claude settings file.
///
/// Why: the summary table distinguishes a real rewrite from an
/// already-migrated no-op, a "no relevant key" skip, and a hard failure.
/// What: enumerates the four terminal states of `migrate_config_file`.
/// Test: unit tests assert `Migrated`, `AlreadyMigrated`, and `Skipped`.
#[derive(Debug, PartialEq, Eq)]
pub enum ConfigMigrateStatus {
    /// The file contained a legacy key and was rewritten.
    Migrated,
    /// The file already contained a `trusty-memory` key — left untouched.
    AlreadyMigrated,
    /// No legacy key (and no `trusty-memory` key) — nothing to do.
    Skipped,
    /// An I/O or parse error occurred.
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

/// Entry point for `trusty-memory migrate`.
///
/// Why: a single command that switches a machine from kuzu-memory to
/// trusty-memory. `kuzu-memory` rewrites every Claude MCP settings file.
/// `kuzu-data` imports entity/relation data from a kuzu-memory `store.redb`
/// into a target palace.
/// What: dispatches to the appropriate handler based on the `target` variant.
/// The `_config_only` flag is accepted for CLI parity with
/// `trusty-search migrate` but only applies to `kuzu-memory` (the config
/// migration).
/// Test: `migrate kuzu-memory --dry-run` enumerates without writing.
pub fn handle_migrate(
    target: MigrateTarget,
    dry_run: bool,
    _config_only: bool,
    kuzu_from: Option<std::path::PathBuf>,
    kuzu_palace: Option<String>,
    kuzu_limit: Option<usize>,
) -> Result<()> {
    match target {
        MigrateTarget::KuzuMemory => {
            if dry_run {
                println!("{} Dry run — no files will be modified.\n", "·".dimmed());
            }
            run_config_phase(dry_run)
        }
        MigrateTarget::KuzuData => {
            let from = kuzu_from
                .ok_or_else(|| anyhow::anyhow!("migrate kuzu-data requires --from <store.redb>"))?;
            let palace = kuzu_palace
                .ok_or_else(|| anyhow::anyhow!("migrate kuzu-data requires --palace <name>"))?;
            crate::commands::kuzu_migrate::handle_kuzu_data_migrate(
                &from, &palace, dry_run, kuzu_limit,
            )
        }
    }
}

/// Scan + rewrite every Claude settings file.
///
/// Why: keeps the orchestration (scan → migrate → summarize) separate from
/// the per-file surgery in `migrate_config_file`.
/// What: locates settings files, migrates each, prints a summary table.
/// Test: covered indirectly by `--dry-run` runs and the unit tests below.
fn run_config_phase(dry_run: bool) -> Result<()> {
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
    let mut already = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for (i, path) in files.iter().enumerate() {
        let result = migrate_config_file(path, dry_run);
        print_config_line(i + 1, files.len(), &result);
        match result.status {
            ConfigMigrateStatus::Migrated => migrated += 1,
            ConfigMigrateStatus::AlreadyMigrated => already += 1,
            ConfigMigrateStatus::Skipped => skipped += 1,
            ConfigMigrateStatus::Failed(_) => failed += 1,
        }
    }

    println!();
    if dry_run {
        println!(
            "{} MCP config dry run: {} would migrate, {} already migrated, {} skipped, {} failed",
            "·".dimmed(),
            migrated,
            already,
            skipped,
            failed
        );
    } else {
        println!(
            "{} MCP config: {} migrated, {} already migrated, {} skipped, {} failed",
            "✓".green(),
            migrated,
            already,
            skipped,
            failed
        );
    }
    Ok(())
}

/// Rewrite one Claude settings file, replacing any legacy kuzu-memory MCP
/// server entry with a `trusty-memory` entry.
///
/// Why: this is the load-bearing surgery — it must preserve every unrelated
/// JSON key, be idempotent across repeated runs, and never corrupt the file
/// on failure (atomic write + `.bak` backup, courtesy of
/// `trusty_common::claude_config::write_json_atomic`).
/// What: parses the file as `serde_json::Value`, swaps the key inside
/// `mcpServers`, then atomically rewrites the file.
/// Test: `test_migrate_config_replaces_dashed_key`,
/// `test_migrate_config_replaces_underscored_key`, and
/// `test_migrate_config_idempotent` cover the rewrite, the alternate
/// spelling, and the no-op-on-already-migrated paths.
pub fn migrate_config_file(path: &Path, dry_run: bool) -> ConfigMigrateResult {
    let fail = |msg: String| ConfigMigrateResult {
        path: path.to_path_buf(),
        status: ConfigMigrateStatus::Failed(msg),
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
        None => {
            return ConfigMigrateResult {
                path: path.to_path_buf(),
                status: ConfigMigrateStatus::Skipped,
            }
        }
    };

    // Idempotency: a trusty-memory entry already present means a previous run
    // (or the user) already migrated this file — never double-migrate.
    if servers.contains_key(TRUSTY_KEY) {
        return ConfigMigrateResult {
            path: path.to_path_buf(),
            status: ConfigMigrateStatus::AlreadyMigrated,
        };
    }

    let legacy_present = LEGACY_MCP_KEYS.iter().any(|k| servers.contains_key(*k));
    if !legacy_present {
        return ConfigMigrateResult {
            path: path.to_path_buf(),
            status: ConfigMigrateStatus::Skipped,
        };
    }

    // Drop every legacy key and insert the canonical trusty-memory entry.
    for k in LEGACY_MCP_KEYS {
        servers.remove(*k);
    }
    servers.insert(
        TRUSTY_KEY.to_string(),
        mcp_server_entry(TRUSTY_KEY, &["serve", "--stdio"]),
    );

    if dry_run {
        return ConfigMigrateResult {
            path: path.to_path_buf(),
            status: ConfigMigrateStatus::Migrated,
        };
    }

    match write_json_atomic(path, &root) {
        Ok(()) => ConfigMigrateResult {
            path: path.to_path_buf(),
            status: ConfigMigrateStatus::Migrated,
        },
        Err(e) => fail(format!("write: {e}")),
    }
}

/// Render one config-migration result line for the summary table.
///
/// Why: keeps colorised, aligned output away from the orchestration logic.
/// What: one line per file with a status glyph.
/// Test: not unit-tested (pure formatting); covered by manual smoke runs.
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
        ConfigMigrateStatus::Skipped => println!(
            "  {} {} {} {}",
            prefix.dimmed(),
            "·".dimmed(),
            path.dimmed(),
            "(no kuzu-memory entry)".dimmed()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the core surgery — a `kuzu-memory` key must be removed and the
    /// canonical trusty-memory key inserted, while unrelated keys survive.
    #[test]
    fn test_migrate_config_replaces_dashed_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.local.json");
        let input = serde_json::json!({
            "theme": "dark",
            "mcpServers": {
                "kuzu-memory": {
                    "command": "kuzu-memory",
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
            !servers.contains_key("kuzu-memory"),
            "legacy key should be gone"
        );
        assert!(servers.contains_key("trusty-memory"), "trusty key missing");
        assert!(
            servers.contains_key("other-server"),
            "unrelated server dropped"
        );
        assert_eq!(
            rewritten["theme"], "dark",
            "unrelated top-level key dropped"
        );
        assert_eq!(servers["trusty-memory"]["command"], "trusty-memory");
        assert_eq!(servers["trusty-memory"]["args"][0], "serve");
        assert_eq!(servers["trusty-memory"]["args"][1], "--stdio");

        // Backup preserves multi-dot filename: settings.local.json.bak
        assert!(
            path.with_file_name("settings.local.json.bak").exists(),
            "backup file missing"
        );
    }

    /// Why: the alternate `kuzu_memory` spelling must be treated identically.
    #[test]
    fn test_migrate_config_replaces_underscored_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let input = serde_json::json!({
            "mcpServers": {
                "kuzu_memory": { "command": "kuzu-memory", "args": ["serve"] }
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&input).unwrap()).expect("write input");

        let result = migrate_config_file(&path, false);
        assert_eq!(result.status, ConfigMigrateStatus::Migrated);

        let rewritten: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let servers = rewritten["mcpServers"].as_object().unwrap();
        assert!(!servers.contains_key("kuzu_memory"));
        assert!(servers.contains_key("trusty-memory"));
    }

    /// Why: a file already carrying a `trusty-memory` entry must be left
    /// untouched so repeated `migrate` runs are safe.
    #[test]
    fn test_migrate_config_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let input = serde_json::json!({
            "mcpServers": {
                "trusty-memory": {
                    "command": "trusty-memory",
                    "args": ["serve", "--stdio"]
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

    /// Why: a settings file with no `kuzu-memory` entry (and no `trusty-memory`
    /// entry) is reported as `Skipped`, not `Migrated`, and not modified.
    #[test]
    fn test_migrate_config_skips_when_no_legacy_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let input = serde_json::json!({
            "mcpServers": {
                "some-other-server": { "command": "x" }
            }
        });
        let serialized = serde_json::to_string_pretty(&input).unwrap();
        std::fs::write(&path, &serialized).expect("write input");

        let result = migrate_config_file(&path, false);
        assert_eq!(result.status, ConfigMigrateStatus::Skipped);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            serialized,
            "file must be untouched"
        );
    }

    /// Why: `--dry-run` must report what *would* change without writing the
    /// file to disk or producing a backup.
    #[test]
    fn test_migrate_config_dry_run_does_not_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let input = serde_json::json!({
            "mcpServers": {
                "kuzu-memory": { "command": "kuzu-memory", "args": ["serve"] }
            }
        });
        let serialized = serde_json::to_string_pretty(&input).unwrap();
        std::fs::write(&path, &serialized).expect("write input");

        let result = migrate_config_file(&path, true);
        assert_eq!(result.status, ConfigMigrateStatus::Migrated);

        // File on disk must be byte-for-byte unchanged.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            serialized,
            "dry run must not write the file"
        );
        assert!(
            !path.with_file_name("settings.json.bak").exists(),
            "dry run must not produce a backup"
        );
    }
}
