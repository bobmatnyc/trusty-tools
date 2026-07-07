//! Untracked/secret file sync config shape + resolution (#2196).
//!
//! Why: split out of `trusty_tools_config.rs` so that file stays under the
//! 500-SLOC production cap (mirrors how `GithubConfig`/`WatchConfig` stay
//! inline but this section, with its resolver function and tests, pushed the
//! parent over the limit). Kept as a sibling module rather than a separate
//! top-level file so `TrustyToolsConfig`/`ProjectConfig` and this section's
//! types stay conceptually one config surface.
//! What: [`UntrackedSyncConfig`] is the on-disk shape (global `untracked_sync:`
//! section or per-project `projects[].untracked_sync` override);
//! [`DEFAULT_UNTRACKED_SYNC_PATTERNS`] is the built-in `.env*` allowlist;
//! [`resolve_untracked_sync`] turns the three precedence tiers (per-project >
//! global > default) into one concrete [`ResolvedUntrackedSync`] per spawn.
//! Test: `tests` submodule.

use serde::{Deserialize, Serialize};

use super::TrustyToolsConfig;

/// Built-in default allowlist patterns for untracked/secret file sync (#2196).
///
/// Why: the dominant real-world need is `.env` files (`.env`, `.env.local`,
/// per-environment variants like `.env.production`), so an unset config must
/// still sync them — "operator does nothing" stays the north-star default.
/// What: `[".env", ".env.local", ".env.*"]`. `.env.*` is a prefix-glob
/// covering per-environment variants without also matching unrelated names.
/// Test: `tests::resolve_untracked_sync_defaults_when_unset`.
pub const DEFAULT_UNTRACKED_SYNC_PATTERNS: &[&str] = &[".env", ".env.local", ".env.*"];

/// The `untracked_sync:` section of `~/.trusty-tools/trusty-mpm/config.yaml`
/// (global) or a `projects[].untracked_sync` override (per-project), #2196.
///
/// Why: operators who use a live checkout with `.env`/secret files expect
/// those files to be present in the per-session worktree too (managed
/// sessions never copy uncommitted/untracked files by default). This section
/// lets an operator declare the sync allowlist and toggle declaratively,
/// mirroring the `GithubConfig`/`WatchConfig` optional-section convention so
/// an absent section is fully backward-compatible.
/// What: `patterns` — an allowlist of filename patterns (see
/// [`resolve_untracked_sync`] for resolution precedence and the built-in
/// default); `enabled` — `None`/`Some(true)` syncs, `Some(false)` disables
/// sync entirely for the scope this section applies to.
/// Test: `tests::untracked_sync_config_yaml_round_trip`,
/// `tests::resolve_untracked_sync_project_overrides_global`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UntrackedSyncConfig {
    /// Allowlist of filename patterns to sync from the source checkout root
    /// into the session worktree (e.g. `[".env", ".env.local", ".env.*"]`).
    ///
    /// `None` → falls through to the next precedence tier (see
    /// [`resolve_untracked_sync`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patterns: Option<Vec<String>>,

    /// Whether untracked/secret file sync runs at all for this scope.
    ///
    /// `None` → falls through to the next precedence tier; the ultimate
    /// built-in default is `true` (sync is ON by default, #2196).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// The resolved untracked-sync allowlist + enabled flag for one spawn (#2196).
///
/// Why: [`sync_untracked_files`](crate::daemon::managed_routes::inproject::untracked_sync::sync_untracked_files)
/// needs a single concrete `(patterns, enabled)` pair per spawn; bundling the
/// resolution result in one struct keeps the precedence-resolution call site
/// (`lifecycle::reserve_inproject_worktree`) a one-liner.
/// What: `patterns` is never empty (falls back to
/// [`DEFAULT_UNTRACKED_SYNC_PATTERNS`]); `enabled` defaults to `true`.
/// Test: covered by the `resolve_untracked_sync_*` tests below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedUntrackedSync {
    /// The resolved allowlist of filename patterns.
    pub patterns: Vec<String>,
    /// Whether sync should run at all.
    pub enabled: bool,
}

/// Resolve the untracked-sync allowlist + enabled flag for a project (#2196).
///
/// Why: an operator may declare patterns/enabled globally, per-project, or
/// not at all — this is the single place that turns those three tiers into
/// one concrete answer, so every call site (currently
/// `lifecycle::reserve_inproject_worktree`) agrees.
/// What: precedence is **per-project `patterns`/`enabled` (matched by
/// `repo_name` against `ProjectConfig::name`) > global
/// `config.untracked_sync` > built-in default** (`enabled = true`,
/// `patterns = `[`DEFAULT_UNTRACKED_SYNC_PATTERNS`]). `patterns` and
/// `enabled` resolve INDEPENDENTLY — e.g. a project may override only
/// `enabled` while inheriting the global `patterns`. `repo_name = None`
/// (no repo identity available) skips the per-project tier entirely.
/// Test: `tests::resolve_untracked_sync_defaults_when_unset`,
/// `tests::resolve_untracked_sync_global_overrides_default`,
/// `tests::resolve_untracked_sync_project_overrides_global`,
/// `tests::resolve_untracked_sync_disabled_by_project`.
pub fn resolve_untracked_sync(
    config: &TrustyToolsConfig,
    repo_name: Option<&str>,
) -> ResolvedUntrackedSync {
    let project_cfg = repo_name.and_then(|name| {
        config
            .projects
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| p.untracked_sync.as_ref())
    });
    let global_cfg = config.untracked_sync.as_ref();

    let patterns = project_cfg
        .and_then(|c| c.patterns.clone())
        .or_else(|| global_cfg.and_then(|c| c.patterns.clone()))
        .unwrap_or_else(|| {
            DEFAULT_UNTRACKED_SYNC_PATTERNS
                .iter()
                .map(|s| s.to_string())
                .collect()
        });

    let enabled = project_cfg
        .and_then(|c| c.enabled)
        .or_else(|| global_cfg.and_then(|c| c.enabled))
        .unwrap_or(true);

    ResolvedUntrackedSync { patterns, enabled }
}

#[cfg(test)]
mod tests;
