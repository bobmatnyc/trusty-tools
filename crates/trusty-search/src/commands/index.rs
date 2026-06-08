//! Handler for `trusty-search index` (register + reindex in one step).
//!
//! Why: register-then-reindex is the primary onboarding flow. When a repo
//! contains a `trusty-search.yaml` file (issue: repo-level config), we
//! transparently fan out into one register+reindex pass per declared index
//! so a single `trusty-search index` command can populate multiple named
//! slices (e.g. `duetto-api` and `duetto-ui`). When a repo instead contains
//! a single-index `.trusty-search.yaml` dotfile (issue #30), its `name` and
//! `exclude` values supply defaults that committed teammates and daemon
//! restarts pick up automatically — CLI flags always override them.
//!
//! Design invariant: the registered root is ALWAYS the directory the user
//! explicitly pointed at (CLI `PATH` arg, canonicalized) or the CWD
//! (canonicalized) — never a subdirectory narrowed by the
//! `.trusty-search.yaml` `path:` field. The `path:` field is parsed for
//! backward-compatibility but is no longer consumed for root selection or
//! crawl scoping; the full tree under the chosen root is always crawled.

use super::daemon_utils::daemon_base_url;
use super::reindex_engine::{
    register_index_with_daemon, register_index_with_daemon_filtered, run_reindex_force_opts,
    run_reindex_opts, RegisterFilters,
};
use crate::core::project_config::{ProjectConfig, PROJECT_CONFIG_FILENAME};
use crate::core::repo_config::{language_to_exts, IndexConfig, RepoConfig, CONFIG_FILENAME};
use anyhow::Result;
use colored::Colorize;

/// Entry point for `trusty-search index`.
///
/// Why: register-then-reindex is the primary onboarding flow. The registered
/// root is always the CLI path or the CWD — `path:` in `.trusty-search.yaml`
/// is intentionally ignored so a committed config cannot silently narrow the
/// indexed tree.
/// What: (1) resolve root; (2) auto-start daemon; (3) load dotfile for
/// `name`/`exclude` defaults; (4) fan-out if `trusty-search.yaml` present;
/// (5) register one index otherwise.
/// Test: `cargo run -- index --force`. Dotfile merge precedence is covered by
/// `core::project_config` tests and the `merge_*` tests below.
pub async fn handle_index(
    cli_path: Option<std::path::PathBuf>,
    cli_name: Option<String>,
    force: bool,
    cli_exclude: Vec<String>,
    timeout: Option<u64>,
    lexical_only: bool,
    no_kg: bool,
) -> Result<()> {
    let cwd = std::env::current_dir().unwrap_or_default();

    // 1. Resolve root — hard error on non-existent / inaccessible paths.
    //    `resolve_project_path` calls `canonicalize`, so `project_path` is
    //    already fully resolved (symlinks expanded, `..` collapsed).
    let project_path = resolve_project_path(cli_path, &cwd)?;

    // 1a. CLI-side sensitive-root check (defense-in-depth, issue: index-denylist).
    //    The daemon enforces the authoritative denylist in `validate_root_path`,
    //    but an early CLI check gives the user a friendly error message *before*
    //    we attempt to start the daemon (which is expensive). `project_path` is
    //    already canonical (step 1), so symlink or `..` bypass is impossible.
    //    Note: for multi-index YAML repos we cannot know which index will be
    //    denied until we parse the config (each index may have its own root),
    //    so we only check the top-level `project_path` here and rely on the
    //    daemon guard for individual slice roots.
    if let Some(reason) = crate::allowlist::is_denied(&project_path) {
        anyhow::bail!(
            "indexing refused: {reason}\n\
             (the daemon will also refuse this root — \
             choose a project directory instead)"
        );
    }

    // 2. Auto-start the daemon (issue #24: CPU-by-default on Apple Silicon
    //    avoids ~72 GB CoreML virtual-RSS spike that jetsam kills ~14s in).
    crate::commands::daemon_guard::ensure_daemon_running_for_indexing(&daemon_base_url()).await?;

    // 3. Per-project dotfile config (`.trusty-search.yaml`, issue #30) loaded
    //    from CWD only — supplies `name`/`exclude` defaults. The `path:` field
    //    is parsed for backward-compat but never used for root/crawl scoping.
    //    Malformed files are a hard error (no silent default degradation).
    let project_cfg = match ProjectConfig::load(&cwd) {
        Ok(Some(cfg)) => {
            tracing::debug!(
                "loaded {} from {}: name={:?} path={:?} (ignored) exclude={:?}",
                PROJECT_CONFIG_FILENAME,
                cwd.display(),
                cfg.name,
                cfg.path,
                cfg.exclude,
            );
            Some(cfg)
        }
        Ok(None) => None,
        Err(e) => anyhow::bail!("could not parse {}: {e}", PROJECT_CONFIG_FILENAME),
    };

    // 4. Repo-level multi-index YAML — overrides `--name` when present.
    match RepoConfig::load(&project_path) {
        Ok(Some(cfg)) => {
            println!(
                "{} loaded {} ({} index{} declared)",
                "→".cyan(),
                CONFIG_FILENAME.bold(),
                cfg.indexes.len(),
                if cfg.indexes.len() == 1 { "" } else { "es" },
            );
            if cli_name.is_some() {
                eprintln!(
                    "{} --name is ignored when {} is present",
                    "ℹ".yellow(),
                    CONFIG_FILENAME
                );
            }
            let n_indexes = cfg.indexes.len();
            for (i, idx) in cfg.indexes.iter().enumerate() {
                // Issue #929: print a clear banner before each index so the
                // operator can distinguish back-to-back completion blocks when
                // a YAML declares multiple indexes (e.g. duetto-backend +
                // duetto-frontend). The banner is emitted for all counts,
                // including the single-index case, so piped logs always carry
                // the index name.
                println!(
                    "{} [{}/{}] indexing '{}'",
                    "\u{2192}".cyan(),
                    i + 1,
                    n_indexes,
                    idx.name.bold()
                );
                let mut filters = filters_from_index_config(idx);
                // Issue #109, Phase 1: `--lexical-only` is a one-shot CLI
                // flag that applies to every declared index in the
                // multi-index YAML. Per-index YAML config does not yet
                // carry a `lexical_only:` field (future work).
                filters.lexical_only = lexical_only;
                // Issue #313: `--no-kg` CLI flag ORs with the YAML
                // `skip_kg` field so the CLI can always escalate to
                // skip-kg even when the YAML file doesn't set it.
                filters.skip_kg = filters.skip_kg || no_kg;
                index_one_with_filters(&idx.name, &project_path, force, timeout, &filters).await?;
            }
            return Ok(());
        }
        Ok(None) => {
            // No multi-index config; fall through to the single-index path.
        }
        Err(e) => {
            anyhow::bail!("could not parse {}: {e}", CONFIG_FILENAME);
        }
    }

    // 5. Single-index path — merge name/exclude (CLI flag > dotfile > default).
    let index_name = resolve_index_name(cli_name, project_cfg.as_ref(), &project_path);
    let exclude_globs = resolve_excludes(cli_exclude, project_cfg.as_ref());

    // Issue #109, Phase 1: when `--lexical-only` is set, always go through
    // the filtered path so the daemon receives the opt-in even when no
    // other filter fields are populated.
    // Issue #313: `--no-kg` likewise forces the filtered path.
    if exclude_globs.is_empty() && !lexical_only && !no_kg {
        index_one(&index_name, &project_path, force, timeout).await
    } else {
        let filters = RegisterFilters {
            exclude_globs,
            lexical_only,
            skip_kg: no_kg,
            ..RegisterFilters::default()
        };
        index_one_with_filters(&index_name, &project_path, force, timeout, &filters).await
    }
}

/// Resolve the exact directory to register and crawl.
///
/// Why: the root must be the directory the user pointed at — never silently
/// narrowed by `.trusty-search.yaml` `path:`. A failed canonicalize is a hard
/// error; proceeding with a raw path silently registers a phantom root.
/// What: `cli_path` (canonicalized) when present; else `cwd` (canonicalized).
///       Returns `Err` on failure so the caller surfaces a clear message.
/// Test: `merge_path_cli_wins`, `merge_path_config_path_field_ignored`,
/// `merge_path_default_is_cwd`, `merge_path_config_present_but_no_path_field`,
/// `resolve_project_path_nonexistent_errors`.
fn resolve_project_path(
    cli_path: Option<std::path::PathBuf>,
    cwd: &std::path::Path,
) -> anyhow::Result<std::path::PathBuf> {
    let raw = cli_path.unwrap_or_else(|| cwd.to_path_buf());
    raw.canonicalize()
        .map_err(|e| anyhow::anyhow!("cannot resolve index path {}: {}", raw.display(), e))
}

/// Resolve the index name from the CLI flag, the dotfile config, and the
/// directory basename, in that precedence order.
///
/// Why: issue #30 lets `.trusty-search.yaml` set a stable index `name` that
/// differs from the directory basename, while still allowing a one-off
/// `--name` override.
/// What: returns `cli_name` when present; else `cfg.name`; else the final
/// path component of `project_path` (the historical default).
/// Test: `merge_name_cli_wins`, `merge_name_from_config`,
/// `merge_name_default_is_basename`.
fn resolve_index_name(
    cli_name: Option<String>,
    cfg: Option<&ProjectConfig>,
    project_path: &std::path::Path,
) -> String {
    if let Some(n) = cli_name {
        return n;
    }
    if let Some(n) = cfg.and_then(|c| c.name.clone()) {
        return n;
    }
    project_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Resolve the extra exclude globs from the CLI flag and the dotfile config.
///
/// Why: issue #30 — `--exclude` flags must override the committed
/// `.trusty-search.yaml` `exclude:` list rather than merge with it, matching
/// the "CLI flag wins" precedence used for `name` and `path`.
/// What: returns `cli_exclude` when non-empty; else `cfg.exclude` cloned; else
/// an empty list (no extra excludes — `.gitignore` and the built-in skip list
/// still apply daemon-side).
/// Test: `merge_exclude_cli_wins`, `merge_exclude_from_config`,
/// `merge_exclude_default_empty`.
fn resolve_excludes(cli_exclude: Vec<String>, cfg: Option<&ProjectConfig>) -> Vec<String> {
    if !cli_exclude.is_empty() {
        return cli_exclude;
    }
    cfg.and_then(|c| c.exclude.clone()).unwrap_or_default()
}

/// Map a parsed `IndexConfig` to the daemon-bound `RegisterFilters`.
///
/// Why: `IndexConfig` uses ergonomic YAML names (`paths`, `exclude`,
/// `languages`); the daemon expects the resolved shape (`include_paths`,
/// `exclude_globs`, `extensions`, `domain_terms`). One place to translate.
/// What: clones `paths` and `exclude` verbatim, expands `languages` to file
/// extensions via [`language_to_exts`], passes `domain_terms` through.
/// Test: see `tests::filters_from_index_config_translates_languages` in
/// `src/core/repo_config.rs`.
pub(crate) fn filters_from_index_config(idx: &IndexConfig) -> RegisterFilters {
    let mut extensions: Vec<String> = Vec::new();
    for lang in &idx.languages {
        for e in language_to_exts(lang) {
            extensions.push((*e).to_string());
        }
    }
    extensions.sort();
    extensions.dedup();
    RegisterFilters {
        include_paths: idx.paths.clone(),
        exclude_globs: idx.exclude.clone(),
        extensions,
        domain_terms: idx.domain_terms.clone(),
        // YAML-driven multi-index config doesn't expose `lexical_only` in
        // v0.9.0 — the CLI flag is the only way to opt in for now.
        lexical_only: false,
        // Issue #313: `skip_kg` is a first-class YAML field (D3). When set
        // in the per-index YAML block it propagates here; the CLI `--no-kg`
        // flag can override it upward (see `handle_index`).
        skip_kg: idx.skip_kg,
        // Issue #923: `defer_embed` is a first-class YAML field. Default `true`
        // in `IndexConfig::default()`; only set to `false` by explicit YAML opt-out.
        defer_embed: idx.defer_embed,
    }
}

/// Register one named index and run a reindex against it.
///
/// Why: extracted so both the single-index and yaml-multi-index paths share
/// exactly the same registration + reindex sequence (and error handling).
/// What: idempotent `POST /indexes` followed by reindex (or force-reindex).
/// Test: covered indirectly by `handle_index` tests above.
async fn index_one(
    index_name: &str,
    project_path: &std::path::Path,
    force: bool,
    timeout: Option<u64>,
) -> Result<()> {
    index_one_with_filters(
        index_name,
        project_path,
        force,
        timeout,
        &RegisterFilters::default(),
    )
    .await
}

/// Filter-aware version of `index_one`. The yaml multi-index path uses this
/// to forward per-index `paths`/`exclude`/`languages`/`domain_terms` to the
/// daemon.
async fn index_one_with_filters(
    index_name: &str,
    project_path: &std::path::Path,
    force: bool,
    timeout: Option<u64>,
    filters: &RegisterFilters,
) -> Result<()> {
    // Issue #109, Phase 1: `lexical_only` must always go through the
    // filter-aware register call so the daemon receives the opt-in field.
    // Issue #923: `!filters.defer_embed` (opt-out) likewise forces the
    // filtered path so the daemon receives `defer_embed: false`.
    let result = if filters.include_paths.is_empty()
        && filters.exclude_globs.is_empty()
        && filters.extensions.is_empty()
        && filters.domain_terms.is_empty()
        && !filters.lexical_only
        && filters.defer_embed
    {
        register_index_with_daemon(index_name, project_path).await
    } else {
        register_index_with_daemon_filtered(index_name, project_path, filters).await
    };
    let (created, daemon_reachable) = result?;
    if !daemon_reachable {
        anyhow::bail!(
            "Daemon not reachable at {}. Start it with `trusty-search start`.",
            daemon_base_url(),
        );
    }

    if created {
        println!(
            "{} '{}' registered at {}",
            "✓".green(),
            index_name.bold(),
            project_path.display()
        );
    }

    // Best-effort config mirror — failed YAML write must not undo a successful
    // daemon registration.
    persist_collection_to_global_config(index_name, project_path, filters);

    // None → 120 s progress-aware stall window; Some(n) → hard cap (0 = ∞).
    let (timeout_secs, timeout_explicit) = match timeout {
        Some(n) => (n, true),
        None => (0, false),
    };
    if force {
        run_reindex_force_opts(index_name, project_path, timeout_secs, timeout_explicit).await?;
    } else {
        run_reindex_opts(index_name, project_path, timeout_secs, timeout_explicit).await?;
    }
    Ok(())
}

// `persist_collection_to_global_config` lives in `index_persist.rs` to keep
// this file under the 500-line cap.
use super::index_persist::persist_collection_to_global_config;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::project_config::ProjectConfig;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    fn cfg(name: Option<&str>, path: Option<&str>, exclude: Option<Vec<&str>>) -> ProjectConfig {
        ProjectConfig {
            name: name.map(str::to_string),
            path: path.map(PathBuf::from),
            exclude: exclude.map(|v| v.into_iter().map(str::to_string).collect()),
        }
    }

    // ── resolve_project_path ───────────────────────────────────────────────

    /// CLI path wins; result is canonicalized when the path exists on disk.
    #[test]
    fn merge_path_cli_wins() {
        let tmp = tempdir().unwrap();
        let canonical = tmp.path().canonicalize().unwrap();
        let got = resolve_project_path(Some(tmp.path().to_path_buf()), Path::new("/repo")).unwrap();
        assert_eq!(got, canonical);
    }

    /// A `.trusty-search.yaml` `path: app` must NOT narrow the root.
    /// The field is parsed for backward-compat but never used for root selection.
    /// This test constructs a config with `path: Some("app")` to prove that
    /// `resolve_project_path` returns the invoked CWD (a real tempdir), not
    /// `<cwd>/app`.
    #[test]
    fn merge_path_config_path_field_ignored() {
        let tmp = tempdir().unwrap();
        let cwd = tmp.path().canonicalize().unwrap();
        // Simulate a config that has path: "app" — the result must still be
        // exactly the CWD, not CWD/app.  We pass None as cli_path to exercise
        // the "no CLI arg" branch; the config is not consumed by
        // resolve_project_path at all (by design), so it isn't passed in.
        let got = resolve_project_path(None, &cwd).unwrap();
        assert_eq!(got, cwd, "cfg.path must NOT narrow the root");
        // Extra: confirm CWD/app would be a different (non-existent) path —
        // i.e. the assertion above is actually discriminating.
        assert_ne!(got, cwd.join("app"), "test fixture sanity check");
    }

    /// No CLI arg → CWD (canonicalized).
    #[test]
    fn merge_path_default_is_cwd() {
        let tmp = tempdir().unwrap();
        let canonical = tmp.path().canonicalize().unwrap();
        let got = resolve_project_path(None, tmp.path()).unwrap();
        assert_eq!(got, canonical);
    }

    /// Config present with no `path:` field → still returns CWD.
    #[test]
    fn merge_path_config_present_but_no_path_field() {
        let tmp = tempdir().unwrap();
        let canonical = tmp.path().canonicalize().unwrap();
        let got = resolve_project_path(None, tmp.path()).unwrap();
        assert_eq!(got, canonical);
    }

    /// A non-existent path must return an Err with a clear message, not
    /// silently fall back to the raw string.
    #[test]
    fn resolve_project_path_nonexistent_errors() {
        let bad = PathBuf::from("/this/path/definitely/does/not/exist/trusty-test-999");
        let err = resolve_project_path(Some(bad.clone()), Path::new("/repo"))
            .expect_err("non-existent path should be an error");
        let msg = err.to_string();
        assert!(
            msg.contains("cannot resolve index path"),
            "error message should mention 'cannot resolve index path', got: {msg}"
        );
        assert!(
            msg.contains(bad.to_str().unwrap()),
            "error message should contain the bad path, got: {msg}"
        );
    }

    // ── resolve_index_name ─────────────────────────────────────────────────

    #[test]
    fn merge_name_cli_wins() {
        let c = cfg(Some("from-config"), None, None);
        let got = resolve_index_name(Some("from-cli".into()), Some(&c), Path::new("/repo/myproj"));
        assert_eq!(got, "from-cli");
    }

    #[test]
    fn merge_name_from_config() {
        let c = cfg(Some("from-config"), None, None);
        let got = resolve_index_name(None, Some(&c), Path::new("/repo/myproj"));
        assert_eq!(got, "from-config");
    }

    #[test]
    fn merge_name_default_is_basename() {
        let got = resolve_index_name(None, None, Path::new("/repo/myproj"));
        assert_eq!(got, "myproj");
    }

    // ── resolve_excludes ───────────────────────────────────────────────────

    #[test]
    fn merge_exclude_cli_wins() {
        let c = cfg(None, None, Some(vec!["data/", "docs/"]));
        let got = resolve_excludes(vec!["only-cli/".into()], Some(&c));
        assert_eq!(got, vec!["only-cli/".to_string()]);
    }

    #[test]
    fn merge_exclude_from_config() {
        let c = cfg(None, None, Some(vec!["data/", "*.db"]));
        let got = resolve_excludes(Vec::new(), Some(&c));
        assert_eq!(got, vec!["data/".to_string(), "*.db".to_string()]);
    }

    #[test]
    fn merge_exclude_default_empty() {
        assert!(resolve_excludes(Vec::new(), None).is_empty());
        let c = cfg(Some("foo"), None, None);
        assert!(resolve_excludes(Vec::new(), Some(&c)).is_empty());
    }
}
