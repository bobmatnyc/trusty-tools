//! CLI command handlers for the standalone managed driver (`tm register/load/run/ls/path/login`).
//!
//! Why: the standalone managed driver adds a new alias-keyed registry and
//! lifecycle on top of the existing session-manager machinery (DOC-24). Keeping
//! these handlers in their own file preserves the existing handlers untouched
//! and stays under the 500-SLOC cap.
//! What: six thin handlers — `register_cmd`, `ls_cmd`, `load_cmd`, `run_cmd`,
//! `path_cmd`, and `login_cmd` — each accepting a `&ManagedPaths` resolved by
//! `resolve_managed_paths` in `managed_root.rs` (closes #1566). `login_cmd`
//! (WI-10) launches `claude auth login` under the tm-global CLAUDE_CONFIG_DIR
//! so the OAuth flow creates a keychain entry for that path, enabling future
//! `tm run` sessions to authenticate on the user's Claude Max/Pro plan.
//! Test: exercised by `cli_parses_register`, `cli_parses_ls`, etc. in tests.rs.

use anyhow::Context;

use super::managed_root::ManagedPaths;

/// Handle `tm register <alias> <url> [--force]`.
///
/// Why: the register command is the first step of the standalone lifecycle —
/// it persists the alias→URL mapping without cloning.
/// What: validates the alias, calls `ManagedRegistry::add`, saves, and prints
/// `registered <alias> → <url>` to stdout.
/// Test: `cli_parses_register` in tests.rs; logic in registry tests.
pub(crate) fn register_cmd(
    paths: &ManagedPaths,
    alias: &str,
    url: &str,
    force: bool,
) -> anyhow::Result<()> {
    let root = &paths.root;
    let mut registry = trusty_mpm::core::standalone::registry::ManagedRegistry::load(root)
        .with_context(|| format!("failed to load registry from {}", root.display()))?;
    registry
        .add(alias, url, force)
        .with_context(|| format!("failed to register alias '{alias}'"))?;
    registry.save().context("failed to save registry")?;
    println!("registered {alias} → {url}");
    Ok(())
}

/// Handle `tm ls [--json]`.
///
/// Why: operators need a quick overview of their registered aliases and which
/// ones are ready to `run` (loaded vs. unloaded).
/// What: lists all registry entries with loaded status; prints a human-readable
/// table by default or a JSON array with `--json`.
/// Test: `cli_parses_ls` in tests.rs.
pub(crate) fn ls_cmd(paths: &ManagedPaths, json: bool) -> anyhow::Result<()> {
    let root = &paths.root;
    let registry = trusty_mpm::core::standalone::registry::ManagedRegistry::load(root)
        .with_context(|| format!("failed to load registry from {}", root.display()))?;
    let entries = registry.list();

    if json {
        let rows: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "alias": e.alias,
                    "url": e.url,
                    "ref": e.git_ref,
                    "loaded": registry.is_loaded(&e.alias, root),
                    "repo_path": root.join("projects").join(&e.alias).join("repo"),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if entries.is_empty() {
        println!("No aliases registered. Use `tm register <alias> <url>` to add one.");
    } else {
        println!("{:<20} {:<10} URL", "ALIAS", "LOADED");
        println!("{}", "-".repeat(60));
        for e in &entries {
            let loaded = if registry.is_loaded(&e.alias, root) {
                "yes"
            } else {
                "no"
            };
            println!("{:<20} {:<10} {}", e.alias, loaded, e.url);
        }
    }
    Ok(())
}

/// Handle `tm load <alias>`.
///
/// Why: `load` is the idempotent step that clones (or refreshes) a registered
/// alias and generates the project-local managed configuration.
/// What: calls `load_alias`, then prints the repo path to stdout.
/// Test: `cli_parses_load` in tests.rs.
pub(crate) fn load_cmd(paths: &ManagedPaths, alias: &str) -> anyhow::Result<()> {
    let root = &paths.root;
    let cfg_dir = &paths.claude_config_dir;
    trusty_mpm::core::standalone::global_config::ensure_global_config_dir(root, cfg_dir)?;
    let repo_path = trusty_mpm::core::standalone::load::load_alias(alias, root, cfg_dir)
        .with_context(|| format!("failed to load alias '{alias}'"))?;
    println!("{}", repo_path.display());
    Ok(())
}

/// Handle `tm run <alias>`.
///
/// Why: `tm run` is the primary interactive entry point — it ensures the alias
/// is loaded and launches `claude` with full isolation via CLAUDE_CONFIG_DIR.
/// What: calls `run_alias` from the `run` module which loads if needed, checks
/// credentials, and spawns `claude` with inherited stdio.
/// Test: `cli_parses_run` in tests.rs; end-to-end requires `claude` binary.
pub(crate) fn run_cmd(paths: &ManagedPaths, alias: &str) -> anyhow::Result<()> {
    let root = &paths.root;
    let cfg_dir = &paths.claude_config_dir;
    trusty_mpm::core::standalone::global_config::ensure_global_config_dir(root, cfg_dir)?;
    trusty_mpm::core::standalone::run::run_alias(alias, root, cfg_dir)
        .with_context(|| format!("failed to run alias '{alias}'"))
}

/// Handle `tm path <alias>`.
///
/// Why: prints the stable `repo/` path so IDEs or scripts can open the project
/// directory directly (DOC-24 SPEC-STANDALONE-MPM-06).
/// What: resolves the alias's repo path via the marker file and prints it.
/// Test: `cli_parses_path` in tests.rs.
pub(crate) fn path_cmd(paths: &ManagedPaths, alias: &str) -> anyhow::Result<()> {
    let root = &paths.root;
    let repo = trusty_mpm::core::standalone::run::resolve_repo_path(alias, root)
        .with_context(|| format!("failed to resolve path for alias '{alias}'"))?;
    println!("{}", repo.display());
    Ok(())
}

/// Handle `tm login` (WI-10 — one-time keychain auth setup).
///
/// Why: `CLAUDE_CONFIG_DIR` (A9) relocates the entire `~/.claude/` tree including
/// the macOS Keychain entry used for Claude Max/Pro OAuth. A fresh
/// `~/.trusty-mpm/claude-config/` has no keychain entry, so `tm run` sessions
/// report "Not logged in". `tm login` runs `claude auth login` under the
/// tm-global `CLAUDE_CONFIG_DIR` so the OAuth flow creates a keychain entry
/// keyed to that path, enabling all future `tm run` sessions to authenticate
/// without repeating this step (keychain entry persists across sessions).
/// What: ensures the tm-global config dir exists via
/// `ensure_global_config_dir`, then spawns `claude auth login` with
/// `CLAUDE_CONFIG_DIR=<root>/claude-config` and inherited stdio so the
/// user can complete the browser/OAuth flow. Prints guidance before and after.
/// Returns an error (non-zero exit) when `claude auth login` fails or is
/// cancelled so that `tm login && tm run …` does not proceed after a failed
/// login.
/// Test: command construction is unit-tested via
/// `test_build_login_command_sets_env_and_invocation` in
/// `core::standalone::run`. Exit-code propagation is covered by reasoning: the
/// `bail!` path is reached whenever `status.success()` is false. The interactive
/// OAuth completion requires a human.
pub(crate) fn login_cmd(paths: &ManagedPaths) -> anyhow::Result<()> {
    let root = &paths.root;
    let cfg_dir = &paths.claude_config_dir;
    trusty_mpm::core::standalone::global_config::ensure_global_config_dir(root, cfg_dir)?;

    eprintln!(
        "tm login: one-time setup — authenticates managed `tm run` sessions\n\
         on your Claude Max/Pro plan via the macOS Keychain.\n\
         Config dir: {}\n\
         Launching `claude auth login` — complete the browser OAuth flow...",
        cfg_dir.display()
    );

    let status = trusty_mpm::core::standalone::run::build_login_command(cfg_dir)
        .status()
        .context("failed to spawn 'claude auth login'; is `claude` installed and on PATH?")?;

    if status.success() {
        eprintln!(
            "tm login: authentication complete.\n\
             You can now run `tm run <alias>` — sessions will authenticate\n\
             on your Claude plan automatically (no ANTHROPIC_API_KEY needed)."
        );
        Ok(())
    } else {
        eprintln!(
            "tm login: `claude auth login` exited with {status}.\n\
             If the OAuth flow was cancelled, run `tm login` again to retry.\n\
             Alternatively, export ANTHROPIC_API_KEY to use the API-key path instead."
        );
        anyhow::bail!("claude auth login failed: {status}")
    }
}
