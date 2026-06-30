//! CLI command handlers for the standalone managed driver.
//!
//! Why: the standalone managed driver adds a new alias-keyed registry and
//! lifecycle on top of the existing session-manager machinery (DOC-24). Keeping
//! these handlers in their own file preserves the existing handlers untouched
//! and stays under the 500-SLOC cap.
//! What: eight thin handlers — `register_cmd`, `ls_cmd`, `load_cmd`, `run_cmd`,
//! `path_cmd`, `login_cmd`, `rm_cmd`, and `update_cmd` — each accepting a
//! `&ManagedPaths` resolved by `resolve_managed_paths` in `managed_root.rs`
//! (closes #1566). `rm_cmd` deregisters an alias and removes its project dir.
//! `update_cmd` pulls the latest changes and idempotently re-deploys managed
//! config — reusing the same `load_alias` function that `load_cmd` calls.
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
/// Why: operators need a quick overview of (a) local git project paths that
/// `tm` has visited and auto-registered, and (b) the managed fleet aliases
/// registered via `tm register`.
/// What: prints two sections — "Local project aliases" (auto-populated from
/// git roots visited by `tm`) and "Managed fleet aliases" (DOC-24 GitHub-URL
/// entries). Empty sections are omitted. `--json` outputs a combined JSON
/// object with both arrays for scripted consumers.
/// Test: `cli_parses_ls` in tests.rs.
pub(crate) fn ls_cmd(paths: &ManagedPaths, json: bool) -> anyhow::Result<()> {
    let root = &paths.root;

    // Load the local project-path alias store (non-fatal if absent).
    let local_entries = trusty_mpm::core::project_aliases::ProjectAliasStore::load(root)
        .unwrap_or_default()
        .list();

    // Load the standalone managed-fleet registry.
    let registry = trusty_mpm::core::standalone::registry::ManagedRegistry::load(root)
        .with_context(|| format!("failed to load registry from {}", root.display()))?;
    let fleet_entries = registry.list();

    if json {
        // JSON output: combined object with two top-level keys.
        let local_rows: Vec<serde_json::Value> = local_entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "alias": e.alias,
                    "path": e.path,
                })
            })
            .collect();
        let fleet_rows: Vec<serde_json::Value> = fleet_entries
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
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "local_projects": local_rows,
                "managed_fleet": fleet_rows,
            }))?
        );
        return Ok(());
    }

    // Human-readable output.
    if local_entries.is_empty() && fleet_entries.is_empty() {
        println!("No aliases registered.");
        println!("  Run `tm` from a git project to auto-register a local alias.");
        println!("  Run `tm register <alias> <url>` to add a managed fleet alias.");
        return Ok(());
    }

    // ── Section 1: Local project aliases ──────────────────────────────────
    if !local_entries.is_empty() {
        println!("Local project aliases ({}):", local_entries.len());
        // Compute column widths for alignment.
        let alias_w = local_entries
            .iter()
            .map(|e| e.alias.len())
            .max()
            .unwrap_or(5)
            .max(5);
        println!("  {:<alias_w$}  PATH", "ALIAS");
        println!("  {}  {}", "─".repeat(alias_w), "─".repeat(40));
        for e in &local_entries {
            println!("  {:<alias_w$}  {}", e.alias, e.path.display());
        }
    }

    // ── Section 2: Managed fleet aliases ──────────────────────────────────
    if !fleet_entries.is_empty() {
        if !local_entries.is_empty() {
            println!();
        }
        println!("Managed fleet aliases ({}):", fleet_entries.len());
        let alias_w = fleet_entries
            .iter()
            .map(|e| e.alias.len())
            .max()
            .unwrap_or(5)
            .max(5);
        println!("  {:<alias_w$}  {:<10}  URL", "ALIAS", "LOADED");
        println!(
            "  {}  {}  {}",
            "─".repeat(alias_w),
            "─".repeat(10),
            "─".repeat(30)
        );
        for e in &fleet_entries {
            let loaded = if registry.is_loaded(&e.alias, root) {
                "yes"
            } else {
                "no"
            };
            println!("  {:<alias_w$}  {:<10}  {}", e.alias, loaded, e.url);
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

/// Handle `tm rm <alias>` — deregister and remove the project directory.
///
/// Why: operators need a clean teardown verb that pairs with `register/load`.
/// The shared `<root>/claude-config/` CLAUDE_CONFIG_DIR is intentionally
/// untouched — it is shared across all aliases and must not be wiped by a
/// single-alias removal.
/// What: removes the registry entry via [`ManagedRegistry::remove`], saves the
/// updated registry, then deletes `<root>/projects/<alias>/` with
/// `fs::remove_dir_all`. Errors clearly when the alias is unknown in the
/// registry. The project dir deletion is best-effort (succeeds even if the
/// dir was already absent — i.e. never loaded).
/// Test: `test_rm_cmd_removes_entry_and_dir`,
/// `test_rm_cmd_errors_on_unknown_alias`,
/// `test_rm_cmd_leaves_claude_config_intact` in tests_behavior_b_tests.rs.
pub(crate) fn rm_cmd(paths: &ManagedPaths, alias: &str) -> anyhow::Result<()> {
    let root = &paths.root;
    let mut registry = trusty_mpm::core::standalone::registry::ManagedRegistry::load(root)
        .with_context(|| format!("failed to load registry from {}", root.display()))?;
    registry
        .remove(alias)
        .with_context(|| format!("alias '{alias}' is not registered"))?;
    registry
        .save()
        .context("failed to save registry after rm")?;

    let project_dir = root.join("projects").join(alias);
    if project_dir.exists() {
        std::fs::remove_dir_all(&project_dir).with_context(|| {
            format!(
                "failed to remove project directory {}",
                project_dir.display()
            )
        })?;
    }
    println!(
        "removed {alias} (registry entry deleted; project dir: {})",
        project_dir.display()
    );
    Ok(())
}

/// Handle `tm update [alias]` — refresh loaded project(s).
///
/// Why: operators need an idempotent "bring this project up to date" verb that
/// mirrors what `load` does (pull + re-deploy) without requiring a full `rm`
/// and re-`load` cycle. With no alias, updates all currently-loaded aliases so
/// a single `tm update` keeps the whole fleet current.
/// What: for each target alias, calls `load_alias` (the same function `load_cmd`
/// uses) which performs `git pull --ff-only` on the existing checkout and
/// idempotently re-runs `prepare_session` + `write_marker`.
/// Single-alias path: if the alias is registered but the project dir does not
/// yet exist, errors IMMEDIATELY with a clear hint to run `tm load <alias>`
/// first (does not defer to a generic end-of-loop message).
/// No-alias path: iterates all registry entries whose project dir already
/// exists (i.e. loaded), updates each; when any fail the returned error names
/// all failed aliases by name.
/// Test: update_cmd_errors_if_alias_not_in_registry,
/// update_cmd_errors_if_not_loaded, and
/// update_cmd_all_skips_unloaded_returns_ok_when_none_loaded
/// in tests_behavior_b_tests.rs.
pub(crate) fn update_cmd(paths: &ManagedPaths, alias: Option<&str>) -> anyhow::Result<()> {
    let root = &paths.root;
    let cfg_dir = &paths.claude_config_dir;
    trusty_mpm::core::standalone::global_config::ensure_global_config_dir(root, cfg_dir)?;

    let registry = trusty_mpm::core::standalone::registry::ManagedRegistry::load(root)
        .with_context(|| format!("failed to load registry from {}", root.display()))?;

    // Single-alias fast path: validate, check loaded state, then update.
    if let Some(a) = alias {
        registry
            .get(a)
            .with_context(|| format!("alias '{a}' is not registered"))?;
        let repo_dir = root.join("projects").join(a).join("repo");
        if !repo_dir.exists() {
            anyhow::bail!(
                "alias '{a}' is registered but has not been loaded yet; \
                 run `tm load {a}` to clone and configure it first"
            );
        }
        trusty_mpm::core::standalone::load::load_alias(a, root, cfg_dir)
            .with_context(|| format!("failed to update alias '{a}'"))?;
        println!("updated {a}");
        return Ok(());
    }

    // No-alias path: update all loaded aliases.
    let targets: Vec<String> = registry
        .list()
        .into_iter()
        .filter(|e| root.join("projects").join(&e.alias).join("repo").exists())
        .map(|e| e.alias)
        .collect();

    if targets.is_empty() {
        println!("no loaded aliases found — nothing to update");
        return Ok(());
    }

    let mut failed: Vec<String> = Vec::new();
    for target in &targets {
        match trusty_mpm::core::standalone::load::load_alias(target, root, cfg_dir) {
            Ok(_) => println!("updated {target}"),
            Err(e) => {
                eprintln!("error updating '{target}': {e}");
                failed.push(target.clone());
            }
        }
    }
    if !failed.is_empty() {
        anyhow::bail!("failed to update: {}", failed.join(", "));
    }
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
