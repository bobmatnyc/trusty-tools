//! Handler for `trusty-search integrate cursor`.
//!
//! Why: getting trusty-search wired into an IDE involves several fiddly,
//! error-prone manual steps — locating `~/.cursor/mcp.json`, editing JSON
//! without clobbering other MCP servers, and writing a Cursor rules file in
//! the right `.mdc` frontmatter format. `integrate cursor` automates all of
//! it idempotently so a user can run one command and have hybrid code search
//! available in Cursor.
//! What: writes the global and/or project Cursor MCP config plus an optional
//! `.cursor/rules/trusty-search.mdc` rules file. No daemon contact — this
//! command only touches local config files.
//! Test: `cargo run -- integrate cursor --dry-run` prints the files it would
//! write without modifying anything; unit tests cover the JSON upsert and
//! backup-path logic.

use anyhow::{Context, Result};
use chrono::Local;
use clap::ValueEnum;
use colored::Colorize;
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// The canonical key written for the trusty-search MCP server in Cursor config.
const TRUSTY_KEY: &str = "trusty-search";

/// One-line description embedded in the MCP server entry.
const TRUSTY_DESCRIPTION: &str = "Hybrid code search — BM25 + vector + knowledge graph";

/// Subdirectory (relative to a `.cursor` dir) holding timestamped backups.
const BACKUP_DIR_NAME: &str = ".mcp-installer-backups";

/// Contents of the Cursor rules file written to `.cursor/rules/trusty-search.mdc`.
///
/// Why: centralizing the rules body keeps `write_cursor_rules` readable and
/// gives a single source of truth for the AI guidance Cursor injects.
const CURSOR_RULES_BODY: &str = r#"---
description: |
  trusty-search is available as an MCP tool for hybrid code search.
  Use it for semantic, lexical, and graph-expanded queries over the indexed codebase.
globs:
  - "**/*"
alwaysApply: true
---

# trusty-search Code Search

This project has trusty-search MCP tools available. Prefer them over grep for non-trivial queries.

- `search` — hybrid BM25 + vector search with KG expansion (best for most queries)
- `search_lexical` / `search_semantic` / `search_kg` — one retrieval lane each
- `grep` — regex/literal match over the indexed files, exact line numbers
- `search_similar` — find code similar to a given file/function
- `get_call_chain` — callers and callees of a symbol
- `reindex` — trigger a full reindex of this project
- `index_status` — check chunk count and index health
- `search_health` — confirm the daemon is running

## When to use

- Finding function definitions → `search "fn <name>"` with Definition intent
- Exploring callers of a function → `get_call_chain`, or `search "<name> callers"` with Usage intent
- Conceptual queries ("how does auth work") → `search_semantic`, or `search` with Conceptual intent
- A specific identifier or literal string → `grep`
- Finding similar implementations → `search_similar`
"#;

/// Which IDE the user is integrating with.
///
/// Why: model the integration target as a clap-validated enum so additional
/// IDEs can be added without changing the CLI surface.
/// What: a single variant today — `cursor`.
/// Test: `cargo run -- integrate bogus` → clap rejects with a usage hint.
#[derive(Debug, Clone, ValueEnum)]
pub enum IntegrateTarget {
    /// Integrate with the Cursor IDE (MCP config + rules file).
    Cursor,
}

/// Outcome of upserting a single Cursor MCP config file.
///
/// Why: the summary table must distinguish a real write from an idempotent
/// skip (already configured) and a dry-run preview.
/// What: enumerates the terminal states of `upsert_cursor_mcp`.
/// Test: unit tests assert `Written` and `AlreadyConfigured`.
#[derive(Debug, PartialEq, Eq)]
pub enum McpFileStatus {
    /// The file was created or updated with the trusty-search entry.
    Written,
    /// The trusty-search key was already present — file left untouched.
    AlreadyConfigured,
    /// Dry run — the file would have been written.
    WouldWrite,
}

/// Result of upserting one Cursor MCP config file (path + status + backup).
///
/// Why: pairs the file path with its outcome and any backup path so the
/// summary renderer can print one line per file.
/// What: returned by `upsert_cursor_mcp`.
/// Test: unit tests inspect `status` after upserting fixture files.
#[derive(Debug)]
pub struct McpFileResult {
    /// The MCP config file that was (or would be) written.
    pub path: PathBuf,
    /// Terminal status of the upsert.
    pub status: McpFileStatus,
    /// Backup path, when an existing file was backed up before writing.
    pub backup: Option<PathBuf>,
}

/// Outcome of writing the Cursor rules file.
///
/// Why: the rules file is a separate concern from MCP config and has its own
/// idempotency and dry-run states.
/// What: enumerates the terminal states of `write_cursor_rules`.
/// Test: unit tests assert `Written` and `AlreadyExists`.
#[derive(Debug, PartialEq, Eq)]
pub enum RulesStatus {
    /// The rules file was created.
    Written,
    /// The rules file already existed — left untouched.
    AlreadyExists,
    /// Dry run — the rules file would have been created.
    WouldWrite,
}

/// Result of writing the Cursor rules file (path + status).
///
/// Why: pairs the rules file path with its outcome for the summary renderer.
/// What: returned by `write_cursor_rules`.
/// Test: unit tests inspect `status` after writing into a temp dir.
#[derive(Debug)]
pub struct RulesResult {
    /// The rules file that was (or would be) written.
    pub path: PathBuf,
    /// Terminal status of the write.
    pub status: RulesStatus,
}

/// Entry point for `trusty-search integrate cursor`.
///
/// Why: a single command that wires trusty-search into Cursor by writing the
/// global MCP config, the project MCP config, and a project rules file —
/// each phase independently skippable.
/// What: dispatches on `target`, then runs the global-MCP, project-MCP, and
/// rules phases according to the `--global-only` / `--project-only` /
/// `--no-rules` flags. Never contacts the daemon.
/// Test: `integrate cursor --dry-run` prints every file it would write.
pub async fn handle_integrate(
    target: IntegrateTarget,
    dry_run: bool,
    global_only: bool,
    project_only: bool,
    no_rules: bool,
) -> Result<()> {
    // `target` has one variant today; the match keeps future IDEs explicit.
    match target {
        IntegrateTarget::Cursor => {}
    }

    println!("{} Integrating trusty-search with Cursor…\n", "⟳".cyan());
    if dry_run {
        println!("{} Dry run — no files will be modified.\n", "·".dimmed());
    }

    // ── Global MCP config (~/.cursor/mcp.json) ────────────────────────────
    if !project_only {
        let path = global_cursor_mcp_path()?;
        let result = upsert_cursor_mcp(&path, dry_run)?;
        print_mcp_line("Global MCP", "~/.cursor/mcp.json", &result);
    }

    // ── Project MCP config (.cursor/mcp.json in CWD) ──────────────────────
    if !global_only {
        let path = project_cursor_mcp_path()?;
        let result = upsert_cursor_mcp(&path, dry_run)?;
        print_mcp_line("Project MCP", ".cursor/mcp.json", &result);
    }

    // ── Project rules file (.cursor/rules/trusty-search.mdc) ──────────────
    if !global_only && !no_rules {
        let rules_dir = project_cursor_rules_dir()?;
        let result = write_cursor_rules(&rules_dir, dry_run)?;
        print_rules_line(".cursor/rules/trusty-search.mdc", &result);
    }

    println!();
    if dry_run {
        println!(
            "{} Dry run complete. Re-run without --dry-run to apply.",
            "·".dimmed()
        );
    } else {
        println!(
            "{} Done. Restart Cursor (or reload MCP servers via Cursor Settings → MCP) to activate.",
            "✓".green()
        );
    }
    Ok(())
}

/// Resolve `~/.cursor/mcp.json`.
///
/// Why: the global Cursor MCP config lives in the user's home directory; this
/// centralizes the path derivation and the home-dir failure case.
/// What: returns `<home>/.cursor/mcp.json`.
/// Test: covered indirectly by `integrate cursor --dry-run` runs.
fn global_cursor_mcp_path() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?;
    Ok(home.join(".cursor").join("mcp.json"))
}

/// Resolve `.cursor/mcp.json` relative to the current working directory.
///
/// Why: the project MCP config is scoped to the repo the user runs the command
/// in; this centralizes the CWD lookup and its failure case.
/// What: returns `<cwd>/.cursor/mcp.json`.
/// Test: covered indirectly by `integrate cursor --dry-run` runs.
fn project_cursor_mcp_path() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("could not determine current directory")?;
    Ok(cwd.join(".cursor").join("mcp.json"))
}

/// Resolve `.cursor/rules` relative to the current working directory.
///
/// Why: the rules file lives under the project's `.cursor/rules` dir; this
/// centralizes the CWD lookup.
/// What: returns `<cwd>/.cursor/rules`.
/// Test: covered indirectly by `integrate cursor --dry-run` runs.
fn project_cursor_rules_dir() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("could not determine current directory")?;
    Ok(cwd.join(".cursor").join("rules"))
}

/// Upsert the `trusty-search` MCP server entry into a Cursor `mcp.json` file.
///
/// Why: this is the load-bearing surgery — it must create a fresh config when
/// none exists, preserve every unrelated MCP server when one does, be
/// idempotent (skip if `trusty-search` is already present), and never corrupt
/// the file (backup → `.tmp` → atomic rename).
/// What: reads `path` (treating a missing file as `{"mcpServers":{}}`), parses
/// it as `serde_json::Value`, inserts the `trusty-search` entry under
/// `mcpServers`, and writes it back atomically with a timestamped backup of
/// any pre-existing file.
/// Test: `test_upsert_creates_fresh_config`, `test_upsert_idempotent`, and
/// `test_upsert_preserves_existing_servers`.
fn upsert_cursor_mcp(path: &Path, dry_run: bool) -> Result<McpFileResult> {
    // Read the existing file, or default to an empty mcpServers block.
    let (original, mut root): (Option<String>, Value) = match fs::read_to_string(path) {
        Ok(content) => {
            let parsed: Value = serde_json::from_str(&content)
                .with_context(|| format!("parse {}", path.display()))?;
            (Some(content), parsed)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (None, serde_json::json!({ "mcpServers": {} }))
        }
        Err(e) => return Err(anyhow::anyhow!("read {}: {e}", path.display())),
    };

    // Ensure `mcpServers` exists and is an object; create it if missing.
    if !root.is_object() {
        return Err(anyhow::anyhow!("{} is not a JSON object", path.display()));
    }
    let obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} is not a JSON object", path.display()))?;
    let servers_entry = obj
        .entry("mcpServers".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let servers = servers_entry
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("`mcpServers` in {} is not an object", path.display()))?;

    // Idempotency: a trusty-search entry already present means a previous run
    // (or the user) already configured this file — never overwrite it.
    if servers.contains_key(TRUSTY_KEY) {
        return Ok(McpFileResult {
            path: path.to_path_buf(),
            status: McpFileStatus::AlreadyConfigured,
            backup: None,
        });
    }

    servers.insert(TRUSTY_KEY.to_string(), trusty_server_entry());

    if dry_run {
        return Ok(McpFileResult {
            path: path.to_path_buf(),
            status: McpFileStatus::WouldWrite,
            backup: None,
        });
    }

    // Back up any pre-existing file before overwriting it.
    let backup = match &original {
        Some(_) => Some(backup_file(path)?),
        None => None,
    };

    write_json_atomic(path, &root)?;

    Ok(McpFileResult {
        path: path.to_path_buf(),
        status: McpFileStatus::Written,
        backup,
    })
}

/// Build the canonical `trusty-search` MCP server JSON entry for Cursor.
///
/// Why: centralizes the one true shape of the Cursor MCP entry so the upsert
/// and the tests agree.
/// What: returns
/// `{"command":"trusty-search","args":["serve"],"description":"…"}`.
/// Test: `test_upsert_creates_fresh_config` asserts the inserted value.
fn trusty_server_entry() -> Value {
    let mut entry = Map::new();
    entry.insert("command".to_string(), Value::String(TRUSTY_KEY.to_string()));
    entry.insert(
        "args".to_string(),
        Value::Array(vec![Value::String("serve".to_string())]),
    );
    entry.insert(
        "description".to_string(),
        Value::String(TRUSTY_DESCRIPTION.to_string()),
    );
    Value::Object(entry)
}

/// Write the Cursor rules file at `<dir>/trusty-search.mdc`.
///
/// Why: Cursor reads `.mdc` rules files to inject project-specific AI guidance;
/// writing one tells Cursor's agent to prefer trusty-search over grep. The
/// write must be idempotent so re-running `integrate` never clobbers a rules
/// file the user may have customized.
/// What: creates `dir` if missing, and writes `trusty-search.mdc` unless it
/// already exists.
/// Test: `test_write_rules_creates_file` and the idempotency assertion in the
/// same test module.
fn write_cursor_rules(dir: &Path, dry_run: bool) -> Result<RulesResult> {
    let path = dir.join("trusty-search.mdc");

    // Idempotency: never overwrite an existing rules file.
    if path.exists() {
        return Ok(RulesResult {
            path,
            status: RulesStatus::AlreadyExists,
        });
    }

    if dry_run {
        return Ok(RulesResult {
            path,
            status: RulesStatus::WouldWrite,
        });
    }

    fs::create_dir_all(dir).with_context(|| format!("create rules dir {}", dir.display()))?;
    fs::write(&path, CURSOR_RULES_BODY)
        .with_context(|| format!("write rules file {}", path.display()))?;

    Ok(RulesResult {
        path,
        status: RulesStatus::Written,
    })
}

/// Copy an existing file into the `.mcp-installer-backups` sibling directory.
///
/// Why: overwriting a user's `mcp.json` without a backup risks losing their
/// other MCP server config if anything goes wrong; a timestamped backup makes
/// every write recoverable.
/// What: creates `<parent>/.mcp-installer-backups/` if missing, copies `path`
/// to `mcp.json.<YYYYMMDD_HHMMSS>.backup` inside it, and returns the backup
/// path.
/// Test: `test_backup_path_format` asserts the backup path shape.
fn backup_file(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("path has no file name: {}", path.display()))?;

    let backup_dir = parent.join(BACKUP_DIR_NAME);
    fs::create_dir_all(&backup_dir)
        .with_context(|| format!("create backup dir {}", backup_dir.display()))?;

    let backup_name = format!("{file_name}.{}.backup", make_timestamp());
    let backup_path = backup_dir.join(backup_name);
    fs::copy(path, &backup_path)
        .with_context(|| format!("copy {} → {}", path.display(), backup_path.display()))?;

    Ok(backup_path)
}

/// Produce a filesystem-safe timestamp string, e.g. `20260519_143022`.
///
/// Why: backup filenames need a sortable, collision-resistant suffix that is
/// safe on every filesystem (no colons or spaces).
/// What: formats the current local time as `%Y%m%d_%H%M%S`.
/// Test: `test_make_timestamp_format` asserts the 15-char `_`-separated shape.
fn make_timestamp() -> String {
    Local::now().format("%Y%m%d_%H%M%S").to_string()
}

/// Atomically persist a JSON value: write `<path>.tmp`, then rename over `path`.
///
/// Why: a half-written `mcp.json` would break the user's Cursor MCP setup; the
/// `.tmp` + atomic-rename sequence guarantees the live file is never partially
/// written.
/// What: ensures the parent directory exists, writes pretty JSON to
/// `<path>.tmp`, then renames `.tmp` over `path`.
/// Test: `test_upsert_creates_fresh_config` confirms the post-write content.
fn write_json_atomic(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create dir {}", parent.display()))?;
    }

    let pretty = serde_json::to_string_pretty(value).context("serialize Cursor MCP config")?;

    // Append `.tmp` to the *full* file name so multi-dot names round-trip.
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("path has no file name: {}", path.display()))?;
    let tmp = path.with_file_name(format!("{file_name}.tmp"));

    fs::write(&tmp, format!("{pretty}\n"))
        .with_context(|| format!("write temp {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Render one MCP-config result line for the summary table.
fn print_mcp_line(label: &str, display_path: &str, r: &McpFileResult) {
    // Emit the resolved absolute path for `--verbose` troubleshooting; the
    // table itself shows the friendly relative form.
    tracing::debug!(target: "integrate", path = %r.path.display(), "{label} resolved");
    let label_col = format!("{label:<13}");
    let path_col = format!("{display_path:<32}");
    match &r.status {
        McpFileStatus::Written => {
            let suffix = match &r.backup {
                Some(b) => {
                    let name = b.file_name().and_then(|n| n.to_str()).unwrap_or("backup");
                    format!(" (backup: {BACKUP_DIR_NAME}/{name})")
                        .dimmed()
                        .to_string()
                }
                None => String::new(),
            };
            println!(
                "  {} {} {}{}",
                label_col.cyan(),
                path_col,
                "✓ written".green(),
                suffix
            );
        }
        McpFileStatus::WouldWrite => println!(
            "  {} {} {}",
            label_col.cyan(),
            path_col,
            "· would write".dimmed()
        ),
        McpFileStatus::AlreadyConfigured => println!(
            "  {} {} {}",
            label_col.cyan(),
            path_col,
            "· already configured (skipped)".dimmed()
        ),
    }
}

/// Render the rules-file result line for the summary table.
fn print_rules_line(display_path: &str, r: &RulesResult) {
    tracing::debug!(target: "integrate", path = %r.path.display(), "rules file resolved");
    let label_col = format!("{:<13}", "Rules");
    let path_col = format!("{display_path:<32}");
    match r.status {
        RulesStatus::Written => println!(
            "  {} {} {}",
            label_col.cyan(),
            path_col,
            "✓ written".green()
        ),
        RulesStatus::WouldWrite => println!(
            "  {} {} {}",
            label_col.cyan(),
            path_col,
            "· would write".dimmed()
        ),
        RulesStatus::AlreadyExists => println!(
            "  {} {} {}",
            label_col.cyan(),
            path_col,
            "· already exists (skipped)".dimmed()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: with no existing file, the upsert must synthesize a valid
    /// `{"mcpServers":{"trusty-search":{…}}}` config and write it.
    #[test]
    fn test_upsert_creates_fresh_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(".cursor").join("mcp.json");

        let result = upsert_cursor_mcp(&path, false).expect("upsert");
        assert_eq!(result.status, McpFileStatus::Written);
        assert!(result.backup.is_none(), "no backup for a fresh file");
        assert!(path.exists(), "config file should be written");

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let servers = written["mcpServers"].as_object().unwrap();
        assert_eq!(servers["trusty-search"]["command"], "trusty-search");
        assert_eq!(servers["trusty-search"]["args"][0], "serve");
        assert_eq!(
            servers["trusty-search"]["description"], TRUSTY_DESCRIPTION,
            "description field should be present"
        );
    }

    /// Why: a file already carrying a trusty-search entry must be left
    /// untouched so repeated `integrate` runs are safe.
    #[test]
    fn test_upsert_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("mcp.json");
        let input = serde_json::json!({
            "mcpServers": {
                "trusty-search": {
                    "command": "trusty-search",
                    "args": ["serve"]
                }
            }
        });
        let serialized = serde_json::to_string_pretty(&input).unwrap();
        fs::write(&path, &serialized).expect("write input");

        let result = upsert_cursor_mcp(&path, false).expect("upsert");
        assert_eq!(result.status, McpFileStatus::AlreadyConfigured);
        assert!(result.backup.is_none(), "no backup for a skipped file");

        // File must be byte-for-byte unchanged.
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            serialized,
            "already-configured file should be untouched"
        );
    }

    /// Why: the upsert must preserve every unrelated MCP server already
    /// present in the user's config.
    #[test]
    fn test_upsert_preserves_existing_servers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("mcp.json");
        let input = serde_json::json!({
            "mcpServers": {
                "other-server": { "command": "other", "args": ["run"] },
                "another": { "command": "another" }
            },
            "unrelatedTopLevel": "keep-me"
        });
        fs::write(&path, serde_json::to_string_pretty(&input).unwrap()).expect("write input");

        let result = upsert_cursor_mcp(&path, false).expect("upsert");
        assert_eq!(result.status, McpFileStatus::Written);
        assert!(result.backup.is_some(), "existing file should be backed up");

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let servers = written["mcpServers"].as_object().unwrap();
        assert!(
            servers.contains_key("other-server"),
            "unrelated server dropped"
        );
        assert!(servers.contains_key("another"), "unrelated server dropped");
        assert!(
            servers.contains_key("trusty-search"),
            "trusty-search entry missing"
        );
        assert_eq!(
            written["unrelatedTopLevel"], "keep-me",
            "unrelated top-level key dropped"
        );
    }

    /// Why: the backup path must land in `.mcp-installer-backups/` and follow
    /// the `mcp.json.<YYYYMMDD_HHMMSS>.backup` naming scheme.
    #[test]
    fn test_backup_path_format() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("mcp.json");
        fs::write(&path, "{}").expect("write input");

        let backup = backup_file(&path).expect("backup");

        let parent = backup.parent().expect("backup has parent");
        assert_eq!(
            parent.file_name().and_then(|n| n.to_str()),
            Some(BACKUP_DIR_NAME),
            "backup should be inside .mcp-installer-backups"
        );
        assert!(parent.is_dir(), "backup dir should exist");
        assert!(backup.exists(), "backup file should exist");

        let name = backup
            .file_name()
            .and_then(|n| n.to_str())
            .expect("backup file name");
        assert!(
            name.starts_with("mcp.json."),
            "backup name should start with `mcp.json.`: {name}"
        );
        assert!(
            name.ends_with(".backup"),
            "backup name should end with `.backup`: {name}"
        );

        // Strip the `mcp.json.` prefix and `.backup` suffix → the timestamp.
        let ts = name
            .strip_prefix("mcp.json.")
            .and_then(|s| s.strip_suffix(".backup"))
            .expect("timestamp segment");
        assert_eq!(ts.len(), 15, "timestamp should be 15 chars: {ts}");
        assert_eq!(
            ts.chars().nth(8),
            Some('_'),
            "timestamp should have `_` at index 8: {ts}"
        );
        assert!(
            ts.chars()
                .enumerate()
                .all(|(i, c)| if i == 8 { c == '_' } else { c.is_ascii_digit() }),
            "timestamp should be digits with one `_`: {ts}"
        );
    }

    /// Why: the timestamp must be a fixed-width, filesystem-safe string so
    /// backup names sort chronologically and never contain colons/spaces.
    #[test]
    fn test_make_timestamp_format() {
        let ts = make_timestamp();
        assert_eq!(ts.len(), 15, "expected `YYYYMMDD_HHMMSS`: {ts}");
        assert_eq!(ts.chars().nth(8), Some('_'), "separator at index 8: {ts}");
        assert!(!ts.contains(':'), "no colons allowed: {ts}");
        assert!(!ts.contains(' '), "no spaces allowed: {ts}");
    }

    /// Assert every tool name the Cursor rules body quotes is a real MCP tool.
    ///
    /// Why (#5138): `CURSOR_RULES_BODY` ships to every Cursor user who runs
    /// `integrate cursor`, and it named `search_code` four times — a tool the
    /// stdio dispatcher has never had. A literal-string assertion cannot catch
    /// that, because the literal and the rules body were wrong together.
    /// What: pulls every backtick-quoted bare identifier out of the rules body
    /// and checks it against `tool_descriptors()`, the same list `tools/list`
    /// serves. Quoted phrases (`search "fn <name>"`) are not bare identifiers
    /// and are skipped.
    /// Test: called by `test_write_rules_creates_file`.
    fn assert_cursor_rules_name_only_real_tools(body: &str) {
        let descriptors = trusty_search::mcp::tools::tool_descriptors();
        let real: Vec<&str> = descriptors
            .as_array()
            .expect("tool_descriptors returns an array of descriptors")
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();

        // Odd-indexed segments of a backtick split are the quoted spans.
        for quoted in body.split('`').skip(1).step_by(2) {
            let is_bare_ident = !quoted.is_empty()
                && quoted.starts_with(|c: char| c.is_ascii_lowercase())
                && quoted
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if !is_bare_ident {
                continue;
            }
            assert!(
                real.contains(&quoted),
                "#5138: Cursor rules name `{quoted}`, which is not a real \
                 trusty-search MCP tool. Real tools: {real:?}"
            );
        }
    }

    /// Why: the rules file must be created the first time and never clobbered
    /// on a second run (idempotency).
    #[test]
    fn test_write_rules_creates_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rules_dir = tmp.path().join(".cursor").join("rules");

        let result = write_cursor_rules(&rules_dir, false).expect("write rules");
        assert_eq!(result.status, RulesStatus::Written);
        let rules_path = rules_dir.join("trusty-search.mdc");
        assert!(rules_path.exists(), "rules file should be written");

        let body = fs::read_to_string(&rules_path).unwrap();
        assert!(
            body.contains("alwaysApply: true"),
            "rules frontmatter missing"
        );
        assert!(
            body.contains("`search`"),
            "rules body should name the hybrid search tool"
        );
        // #5138: the old assertion pinned `search_code`, a tool that has never
        // existed in the dispatcher — so the suite went green on rules that
        // told every Cursor user to call a nonexistent tool. Validate against
        // the real descriptor list instead of a literal.
        assert_cursor_rules_name_only_real_tools(&body);

        // Second run is idempotent: file left untouched.
        let again = write_cursor_rules(&rules_dir, false).expect("write rules again");
        assert_eq!(again.status, RulesStatus::AlreadyExists);
        assert_eq!(
            fs::read_to_string(&rules_path).unwrap(),
            body,
            "rules file should be untouched on a second run"
        );
    }
}
