//! Handler for `trusty-search index` (register + reindex in one step).
//!
//! Why: register-then-reindex is the primary onboarding flow. When a repo
//! contains a `trusty-search.yaml` file (issue: repo-level config), we
//! transparently fan out into one register+reindex pass per declared index
//! so a single `trusty-search index` command can populate multiple named
//! slices (e.g. `duetto-api` and `duetto-ui`). When a repo instead contains
//! a single-index `.trusty-search.yaml` dotfile (issue #30), its `name`,
//! `path`, and `exclude` values supply defaults that committed teammates and
//! daemon restarts pick up automatically — CLI flags always override them.

use super::daemon_utils::daemon_base_url;
use super::reindex_engine::{
    register_index_with_daemon, register_index_with_daemon_filtered, run_reindex_force_opts,
    run_reindex_opts, RegisterFilters,
};
use crate::config::{CollectionConfig, GlobalConfig};
use crate::core::project_config::{ProjectConfig, PROJECT_CONFIG_FILENAME};
use crate::core::repo_config::{language_to_exts, IndexConfig, RepoConfig, CONFIG_FILENAME};
use anyhow::Result;
use colored::Colorize;

/// Entry point for `trusty-search index`.
///
/// Why: register-then-reindex is the primary onboarding flow. With a
/// `trusty-search.yaml` present, this dispatches into a multi-index pass;
/// with a single-index `.trusty-search.yaml` dotfile present (issue #30) it
/// uses that file's `name`/`path`/`exclude` as defaults; otherwise it falls
/// back to the built-in single-index behaviour.
/// What:
/// 1. Auto-start the daemon if needed.
/// 2. Load `<cwd>/.trusty-search.yaml` (issue #30) and merge: CLI arg wins
///    over config-file value wins over built-in default. Config `path` is
///    resolved relative to the config file's directory (the CWD).
/// 3. Look for `<path>/trusty-search.yaml`. If present, ignore `--name` and
///    register+reindex each declared index sequentially.
/// 4. Otherwise, register one index with the merged name/path/exclude.
///
/// Test: `cargo run -- index --force` against a healthy daemon prints the
/// registration line then drives the SSE progress bar. With a yaml at
/// `<path>/trusty-search.yaml`, it iterates each declared name. With a
/// `.trusty-search.yaml` dotfile in CWD, the merge precedence is exercised by
/// `core::project_config` unit tests plus the `merge_*` tests below.
///
/// `cli_path` is the optional positional `PATH` argument; `cli_name` /
/// `cli_exclude` are the optional `--name` / `--exclude` flags.
/// `timeout` is the user-supplied `--timeout` value: `None` means "use the
/// progress-aware stall default"; `Some(n)` means "hard cap at n seconds"
/// (with n=0 meaning wait forever).
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

    // 1. Per-project dotfile config (`.trusty-search.yaml`, issue #30). Loaded
    //    from the CWD only — it supplies defaults for the index name, the
    //    subdirectory to index, and extra exclude globs. A malformed file is a
    //    hard error so a config typo never silently degrades to defaults.
    let project_cfg = match ProjectConfig::load(&cwd) {
        Ok(Some(cfg)) => {
            tracing::debug!(
                "loaded {} from {}: name={:?} path={:?} exclude={:?}",
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

    // 2. Resolve PATH: CLI positional arg wins; else config `path` resolved
    //    relative to the config file's directory (CWD); else the CWD itself.
    let project_path = resolve_project_path(cli_path, project_cfg.as_ref(), &cwd);

    // 0. Auto-start the daemon if needed. `index` is useless without it,
    //    so we proactively boot it rather than dump a confusing connection
    //    error on the user.
    //
    //    Why CPU-by-default here (issue #24): on Apple Silicon the CoreML
    //    EP session-init alone allocates ~72 GB of virtual RSS, which macOS
    //    jetsam treats as memory pressure and SIGKILLs ~14s in — before any
    //    files are indexed. `ensure_daemon_running_for_indexing` propagates
    //    `--device cpu` to the auto-spawned daemon (overridable via
    //    `TRUSTY_INDEX_DEVICE=auto|gpu`). Already-running daemons are not
    //    affected; this only changes the auto-spawn behaviour.
    crate::commands::daemon_guard::ensure_daemon_running_for_indexing(&daemon_base_url()).await?;

    // 3. Repo-level config detection. `trusty-search.yaml` at the project root
    //    declares one or more named indexes; when present it overrides the
    //    `--name` flag and we register each declared slice in turn.
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
            for idx in &cfg.indexes {
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

    // Single-index path. Merge name and exclude with the same precedence as
    // the path resolution above: CLI flag > `.trusty-search.yaml` value >
    // built-in default.
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

/// Resolve the directory to index from the CLI arg, the dotfile config, and
/// the CWD, in that precedence order.
///
/// Why: issue #30 lets a committed `.trusty-search.yaml` point at a
/// subdirectory (`path: app`) so teammates need not retype it; an explicit
/// CLI `PATH` must still win, and a config `path` is written relative to the
/// config file's directory rather than the process CWD-at-call-time.
/// What: returns `cli_path` verbatim when present; else `config_dir`-joined
/// `cfg.path` when the config supplies one; else `config_dir` itself.
/// Test: `merge_path_cli_wins`, `merge_path_config_relative`,
/// `merge_path_default_is_cwd`.
fn resolve_project_path(
    cli_path: Option<std::path::PathBuf>,
    cfg: Option<&ProjectConfig>,
    config_dir: &std::path::Path,
) -> std::path::PathBuf {
    if let Some(p) = cli_path {
        return p;
    }
    if let Some(rel) = cfg.and_then(|c| c.path.as_ref()) {
        return config_dir.join(rel);
    }
    config_dir.to_path_buf()
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
    let result = if filters.include_paths.is_empty()
        && filters.exclude_globs.is_empty()
        && filters.extensions.is_empty()
        && filters.domain_terms.is_empty()
        && !filters.lexical_only
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

    // Mirror the registration into `~/.config/trusty-search/config.yaml` so
    // (a) `index remove` has a canonical entry to drop, and (b) the daemon's
    // auto-discovery on the next restart sees the collection as a first-class
    // user-declared entry rather than guessing it from filesystem markers.
    // Best-effort: a failed YAML write must not undo the successful daemon
    // registration, so we only warn and continue.
    persist_collection_to_global_config(index_name, project_path, filters);

    // Unpack the user's optional timeout into (timeout_secs, timeout_explicit).
    // None → progress-aware stall default (120 s stall window, no hard cap).
    // Some(n) → hard cap at n seconds (0 = wait forever).
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

/// Write (or update) an entry in `~/.config/trusty-search/config.yaml`.
///
/// Why: issue #40 — the YAML config is the user-facing source of truth for
/// indexed projects. Every successful `trusty-search index` invocation must
/// add/update its matching `collections:` entry so a daemon restart preserves
/// the registration and `index remove` has a row to drop. Failures here are
/// non-fatal because the daemon-side registration already succeeded.
/// What: loads the existing config (creating an empty one if missing), upserts
/// a `CollectionConfig` matching `name`/`path` plus the filter-derived
/// `exclude`/`extensions`/`domain_terms`, and saves atomically. Warnings are
/// emitted via `tracing::warn!` so daemon logs surface them without polluting
/// stdout.
/// Test: covered indirectly by `config::tests::roundtrip_preserves_fields`
/// (round-trip) and `config::tests::upsert_replaces_by_name` (idempotency);
/// CLI smoke tested by running `trusty-search index` twice and inspecting the
/// resulting YAML.
fn persist_collection_to_global_config(
    index_name: &str,
    project_path: &std::path::Path,
    filters: &RegisterFilters,
) {
    let mut cfg = match GlobalConfig::load() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("could not load global config to record index '{index_name}': {e:#}");
            return;
        }
    };
    cfg.upsert_collection(CollectionConfig {
        name: index_name.to_string(),
        path: project_path.to_path_buf(),
        extensions: filters.extensions.clone(),
        exclude: filters.exclude_globs.clone(),
        domain_terms: filters.domain_terms.clone(),
    });
    if let Err(e) = cfg.save() {
        tracing::warn!("could not save global config after registering '{index_name}': {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::project_config::ProjectConfig;
    use std::path::{Path, PathBuf};

    fn cfg(name: Option<&str>, path: Option<&str>, exclude: Option<Vec<&str>>) -> ProjectConfig {
        ProjectConfig {
            name: name.map(str::to_string),
            path: path.map(PathBuf::from),
            exclude: exclude.map(|v| v.into_iter().map(str::to_string).collect()),
        }
    }

    // ── resolve_project_path ───────────────────────────────────────────────

    #[test]
    fn merge_path_cli_wins() {
        let c = cfg(None, Some("app"), None);
        let got = resolve_project_path(
            Some(PathBuf::from("/explicit/cli")),
            Some(&c),
            Path::new("/repo"),
        );
        assert_eq!(got, PathBuf::from("/explicit/cli"));
    }

    #[test]
    fn merge_path_config_relative() {
        let c = cfg(None, Some("app"), None);
        let got = resolve_project_path(None, Some(&c), Path::new("/repo"));
        assert_eq!(got, PathBuf::from("/repo/app"));
    }

    #[test]
    fn merge_path_default_is_cwd() {
        let got = resolve_project_path(None, None, Path::new("/repo"));
        assert_eq!(got, PathBuf::from("/repo"));
    }

    #[test]
    fn merge_path_config_present_but_no_path_field() {
        let c = cfg(Some("foo"), None, None);
        let got = resolve_project_path(None, Some(&c), Path::new("/repo"));
        assert_eq!(got, PathBuf::from("/repo"));
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
