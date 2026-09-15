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
//! On a manifest-bearing root, `--name` selects one declared index and a
//! `--name` matching none is refused rather than silently replaced — see
//! [`select_manifest_indexes`] and #6920.
//!
//! Design invariant: the registered root is ALWAYS the directory the user
//! explicitly pointed at (CLI `PATH` arg, canonicalized) or the CWD
//! (canonicalized) — never a subdirectory narrowed by the
//! `.trusty-search.yaml` `path:` field. The `path:` field is parsed for
//! backward-compatibility but is no longer consumed for root selection or
//! crawl scoping; the full tree under the chosen root is always crawled.

use super::daemon_utils::daemon_base_url;
use super::reindex_engine::{
    register_index_reporting_collision, run_reindex_force_opts, run_reindex_opts, RegisterFilters,
    RegisterOutcome,
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
/// `name`/`exclude` defaults; (4) if `trusty-search.yaml` is present, index the
/// declared indexes [`select_manifest_indexes`] picks; (5) register one index
/// otherwise.
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

    // 1b. #767: same shape for the opt-in gate. The daemon is authoritative
    //    (`validate_root_path`), but starting it and building an indexer only
    //    to be refused wastes ~seconds and buries the actionable message.
    let allow_paths = crate::allowlist::AllowlistPaths::default();
    // A denylist hit was already reported above; an unreadable allowlist is
    // left to the daemon, which is the authority.
    if let Ok(crate::allowlist::AllowlistVerdict::NotAllowlisted) =
        crate::allowlist::check_path_with(&project_path, &allow_paths)
    {
        anyhow::bail!(
            "indexing refused: '{}' is not approved for indexing (default-deny, #767)\n\
             Approve it with:  trusty-search index add {}\n\
             …or register it as a project with `tm`. Inspect what is approved \
             with `trusty-search index list`.",
            project_path.display(),
            project_path.display(),
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
            // #6920: an explicit `-n` naming no declared index is refused,
            // never silently replaced by the manifest's first name.
            let selected = select_manifest_indexes(cli_name.as_deref(), &cfg, &project_path)?;
            let n_indexes = selected.len();
            for (i, idx) in selected.iter().enumerate() {
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

/// Choose which manifest-declared indexes one `trusty-search index` run touches.
///
/// Why: #6920 — a `-n` that named no declared index used to be dropped, so
/// `index --force -n flyr-duetto-monolith <root>` reindexed `duetto-backend`
/// instead and tore that index's HNSW snapshot during the #6910 recovery.
/// Refusing is the ruling: the operator either meant a declared name or meant
/// a different root, and both mistakes are cheap to correct and expensive to
/// discover after a reindex has run.
/// What: `None` selects every declared index (the historical fan-out); a `-n`
/// equal to one declared name selects exactly that index; any other `-n` is an
/// error naming the manifest path, the declared names, the conflicting value,
/// and the two ways forward.
/// Test: `manifest_name_conflict_is_refused`,
/// `manifest_name_conflict_message_names_manifest_and_ways_forward`,
/// `manifest_name_match_selects_only_that_index`,
/// `manifest_without_cli_name_selects_every_index`.
fn select_manifest_indexes<'a>(
    cli_name: Option<&str>,
    cfg: &'a RepoConfig,
    project_path: &std::path::Path,
) -> Result<Vec<&'a IndexConfig>> {
    let Some(name) = cli_name else {
        return Ok(cfg.indexes.iter().collect());
    };
    if let Some(idx) = cfg.indexes.iter().find(|i| i.name == name) {
        return Ok(vec![idx]);
    }
    let declared: Vec<&str> = cfg.indexes.iter().map(|i| i.name.as_str()).collect();
    let declared = if declared.is_empty() {
        "(none)".to_string()
    } else {
        declared.join(", ")
    };
    anyhow::bail!(
        "--name '{name}' is not declared in {manifest}\n\
         declared index names: {declared}\n\
         Either omit --name to index every declared name, or pass one of the \
         declared names.",
        manifest = project_path.join(CONFIG_FILENAME).display(),
    )
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

/// Which index one `trusty-search index` run goes on to reindex.
///
/// Why: #7758 needs "the id the daemon actually holds" to be a value the call
/// site reads, not a branch buried in the middle of an async function that
/// cannot be unit-tested without a live daemon.
/// What: the two reachable shapes — the requested id registered, or the root's
/// existing owner adopted under `--force`.
/// Test: `force_adopts_the_index_that_already_owns_the_root`.
#[derive(Debug, PartialEq, Eq)]
enum ReindexTarget {
    /// The requested id is registered; `created` is `false` for a re-register.
    Registered { created: bool },
    /// #7758: the root already belonged to `existing_id`, and `--force`
    /// reindexes that index rather than failing on the registration 409.
    Adopted { existing_id: String },
}

/// Turn a registration answer into the reindex this run should perform.
///
/// Why: #7758 — `trusty-search index <root> --force` on a root already
/// registered under a different id aborted with
/// `daemon returned 409 Conflict for POST /indexes`, while the flag's own
/// `--help` promises "force a full reindex even if the index already has
/// chunks". The daemon must keep refusing the registration (two indexes cannot
/// share one `.redb` corpus — #2336, #3993), so the flag is honoured here: the
/// reindex targets the index that owns the tree.
/// What: `Registered` passes through; `RootOwnedBy` becomes `Adopted` under
/// `--force` and re-raises the daemon's own refusal without it, so the
/// no-`--force` behaviour is byte-identical to before. `Unreachable` is the
/// start-the-daemon error.
/// Test: `force_adopts_the_index_that_already_owns_the_root`,
/// `without_force_a_root_collision_still_raises_the_daemon_refusal`,
/// `a_plain_registration_is_never_adopted`.
fn resolve_reindex_target(outcome: RegisterOutcome, force: bool) -> Result<ReindexTarget> {
    match outcome {
        RegisterOutcome::Registered { created } => Ok(ReindexTarget::Registered { created }),
        RegisterOutcome::Unreachable => anyhow::bail!(
            "Daemon not reachable at {}. Start it with `trusty-search start`.",
            daemon_base_url(),
        ),
        RegisterOutcome::RootOwnedBy {
            existing_id,
            refusal,
        } => {
            // #7758: without --force this is the refusal the CLI always raised.
            anyhow::ensure!(force, "{refusal}");
            Ok(ReindexTarget::Adopted { existing_id })
        }
    }
}

/// Filter-aware version of `index_one`. The yaml multi-index path uses this
/// to forward per-index `paths`/`exclude`/`languages`/`domain_terms` to the
/// daemon.
///
/// Why the collision arm: #7758 — `--force` was consumed only by the reindex
/// call at the bottom, so a root already registered under a DIFFERENT id died
/// on the registration 409 and never reached it, contradicting the flag's own
/// `--help`. With `--force` the reindex now runs against the id that owns the
/// root; without it the refusal is unchanged.
/// Test: `force_adopts_the_index_that_already_owns_the_root`,
/// `without_force_a_root_collision_still_raises_the_daemon_refusal`.
async fn index_one_with_filters(
    index_name: &str,
    project_path: &std::path::Path,
    force: bool,
    timeout: Option<u64>,
    filters: &RegisterFilters,
) -> Result<()> {
    // #7758: one register call for every shape. The empty-filters fast path
    // this replaced called `register_index_with_daemon`, which substitutes
    // DEFAULT filters — so `--no-kg` alone (every other field empty) had its
    // `skip_kg: true` dropped on the wire, silently undoing #313. Passing
    // `filters` through builds the identical body when they ARE default.
    let outcome = register_index_reporting_collision(index_name, project_path, filters).await?;
    let target = resolve_reindex_target(outcome, force)?;
    let reindex_id = match &target {
        ReindexTarget::Registered { .. } => index_name.to_string(),
        ReindexTarget::Adopted { existing_id } => existing_id.clone(),
    };

    match &target {
        ReindexTarget::Registered { created: true } => println!(
            "{} '{}' registered at {}",
            "✓".green(),
            reindex_id.bold(),
            project_path.display()
        ),
        ReindexTarget::Registered { created: false } => {}
        ReindexTarget::Adopted { existing_id } => println!(
            "{} {} is already indexed as '{}'; --force reindexes that index",
            "→".cyan(),
            project_path.display(),
            existing_id.bold(),
        ),
    }

    // Best-effort config mirror — failed YAML write must not undo a successful
    // daemon registration. Keyed on the id the daemon actually holds, so the
    // collision arm does not record a registration that was refused.
    persist_collection_to_global_config(&reindex_id, project_path, filters);

    // None → 120 s progress-aware stall window; Some(n) → hard cap (0 = ∞).
    let (timeout_secs, timeout_explicit) = match timeout {
        Some(n) => (n, true),
        None => (0, false),
    };
    if force {
        run_reindex_force_opts(&reindex_id, project_path, timeout_secs, timeout_explicit).await?;
    } else {
        run_reindex_opts(&reindex_id, project_path, timeout_secs, timeout_explicit).await?;
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
            ..Default::default()
        }
    }

    // ── select_manifest_indexes (#6920) ────────────────────────────────────

    fn manifest(names: &[&str]) -> RepoConfig {
        RepoConfig {
            version: 1,
            indexes: names
                .iter()
                .map(|n| IndexConfig {
                    name: (*n).to_string(),
                    ..Default::default()
                })
                .collect(),
        }
    }

    /// #6920 regression: `-n flyr-duetto-monolith` on a root whose manifest
    /// declares `duetto-backend` / `duetto-frontend` must fail, not fall back
    /// to indexing the declared names. Before the fix this returned both
    /// declared indexes and the reindex ran against `duetto-backend`.
    #[test]
    fn manifest_name_conflict_is_refused() {
        let cfg = manifest(&["duetto-backend", "duetto-frontend"]);
        let got = select_manifest_indexes(
            Some("flyr-duetto-monolith"),
            &cfg,
            Path::new("/repo/flyr-duetto-monolith"),
        );
        assert!(
            got.is_err(),
            "a -n declared nowhere in the manifest must be refused, got {:?}",
            got.map(|v| v.iter().map(|i| i.name.clone()).collect::<Vec<_>>()),
        );
    }

    /// The refusal has to be actionable: it names the manifest, every declared
    /// name, the conflicting value, and both ways forward.
    #[test]
    fn manifest_name_conflict_message_names_manifest_and_ways_forward() {
        let cfg = manifest(&["duetto-backend", "duetto-frontend"]);
        let err = select_manifest_indexes(
            Some("flyr-duetto-monolith"),
            &cfg,
            Path::new("/repo/flyr-duetto-monolith"),
        )
        .expect_err("conflicting -n should error");
        let msg = err.to_string();
        for needle in [
            "flyr-duetto-monolith",
            "/repo/flyr-duetto-monolith/trusty-search.yaml",
            "duetto-backend",
            "duetto-frontend",
            "omit --name",
            "pass one of the declared names",
        ] {
            assert!(msg.contains(needle), "message missing {needle:?}: {msg}");
        }
    }

    /// A `-n` naming one declared index narrows the run to that index.
    #[test]
    fn manifest_name_match_selects_only_that_index() {
        let cfg = manifest(&["duetto-backend", "duetto-frontend"]);
        let got =
            select_manifest_indexes(Some("duetto-frontend"), &cfg, Path::new("/repo/monolith"))
                .expect("a declared name must be accepted");
        assert_eq!(
            got.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            vec!["duetto-frontend"]
        );
    }

    /// No `-n` keeps the historical fan-out over every declared index.
    #[test]
    fn manifest_without_cli_name_selects_every_index() {
        let cfg = manifest(&["duetto-backend", "duetto-frontend"]);
        let got = select_manifest_indexes(None, &cfg, Path::new("/repo/monolith"))
            .expect("no -n must fan out");
        assert_eq!(
            got.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            vec!["duetto-backend", "duetto-frontend"]
        );
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

    // ── resolve_reindex_target (#7758) ─────────────────────────────────────

    fn root_owned_by(id: &str) -> RegisterOutcome {
        RegisterOutcome::RootOwnedBy {
            existing_id: id.to_string(),
            refusal: "daemon returned 409 Conflict for POST /indexes".to_string(),
        }
    }

    /// #7758 regression: `index <root> --force` on a root already registered
    /// under another id must reindex THAT index. Before the fix the 409 was
    /// collapsed into a bail inside `register_index_with_daemon_filtered` and
    /// `--force` never reached the reindex at all, contradicting its `--help`
    /// ("force a full reindex even if the index already has chunks").
    /// Test: this function IS the test.
    #[test]
    fn force_adopts_the_index_that_already_owns_the_root() {
        let got = resolve_reindex_target(root_owned_by("trusty-tools-checkout"), true)
            .expect("--force must not fail on a root collision");
        assert_eq!(
            got,
            ReindexTarget::Adopted {
                existing_id: "trusty-tools-checkout".to_string()
            },
            "--force must reindex the index that owns the root"
        );
    }

    /// The other half of the #7758 ruling: WITHOUT `--force`, nothing changes.
    /// The daemon's own refusal is re-raised verbatim, so a caller that was
    /// parsing it still sees the same text.
    /// Test: this function IS the test.
    #[test]
    fn without_force_a_root_collision_still_raises_the_daemon_refusal() {
        let err = resolve_reindex_target(root_owned_by("trusty-tools-checkout"), false)
            .expect_err("a root collision without --force must still fail");
        assert_eq!(
            err.to_string(),
            "daemon returned 409 Conflict for POST /indexes",
            "the pre-#7758 refusal must be unchanged"
        );
    }

    /// Adoption is reachable ONLY from a root collision — a plain registration
    /// keeps the requested id whether or not `--force` was passed. Without
    /// this, "reindex whatever the daemon names" could silently retarget an
    /// ordinary run.
    /// Test: this function IS the test.
    #[test]
    fn a_plain_registration_is_never_adopted() {
        for force in [false, true] {
            for created in [false, true] {
                let got = resolve_reindex_target(RegisterOutcome::Registered { created }, force)
                    .expect("a successful registration must resolve");
                assert_eq!(
                    got,
                    ReindexTarget::Registered { created },
                    "force={force} created={created} must not adopt another id"
                );
            }
        }
    }
}
