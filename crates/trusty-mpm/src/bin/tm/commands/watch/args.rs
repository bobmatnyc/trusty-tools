//! Effective-settings resolution for `tm watch` — CLI flags over config defaults.
//!
//! Why: `tm watch poll|listen <project>` takes several settings (the routing
//! label, the poll interval, the issue state) that may come from a CLI flag OR
//! the `watch:` section of `~/.trusty-tools/trusty-mpm/config.yaml` (#1220). The
//! precedence must be applied in exactly one tested place so `poll` and `listen`
//! cannot diverge, and so "CLI flag overrides config" is an asserted invariant
//! rather than scattered `unwrap_or` calls.
//! What: [`RawWatchArgs`] is the parsed CLI surface (all overrides optional);
//! [`ResolvedWatch`] is the fully-resolved settings the loops consume;
//! [`resolve`] merges a `RawWatchArgs` with a [`WatchConfig`] applying the
//! precedence **CLI flag > config value > built-in default** and resolves the
//! `<project>` token (either a direct `owner/repo` or a name falling back to the
//! configured `repo`). [`DEFAULT_LABEL`] / [`DEFAULT_INTERVAL_SECS`] name the
//! built-in defaults.
//! Test: `resolve_*` in the sibling `tests.rs`.

use trusty_common::github_path::parse_github_path;

use trusty_mpm::core::trusty_tools_config::WatchConfig;

use super::github::IssueState;

/// Built-in default routing label.
///
/// Why: the design fixes `tm-agent` as the default routing label (issues carrying
/// it are picked up); naming it once keeps the CLI default and the docs aligned.
/// What: `"tm-agent"`.
/// Test: `resolve_uses_default_label_when_unset`.
pub(crate) const DEFAULT_LABEL: &str = "tm-agent";

/// Built-in default `listen` poll interval, in seconds.
///
/// Why: a sensible cadence that is gentle on the GitHub API yet responsive; the
/// design fixes it at 60s. Named once so the CLI default and config fallback agree.
/// What: `60`.
/// Test: `resolve_uses_default_interval_when_unset`.
pub(crate) const DEFAULT_INTERVAL_SECS: u64 = 60;

/// The parsed `tm watch` CLI surface before config merging.
///
/// Why: separating the raw (all-optional) CLI overrides from the resolved
/// settings keeps the precedence logic in [`resolve`] and makes the merge
/// unit-testable without clap.
/// What: the `<project>` token plus the optional `--label` / `--interval-secs` /
/// `--state` overrides. `project` is the verbatim positional (`owner/repo` or a
/// name); `None` overrides mean "fall back to config then default".
/// Test: drives every `resolve_*` test.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RawWatchArgs {
    /// The verbatim `<project>` positional (`owner/repo` or a registered name).
    pub(crate) project: String,
    /// `--label` override; `None` → config then [`DEFAULT_LABEL`].
    pub(crate) label: Option<String>,
    /// `--interval-secs` override; `None` → config then [`DEFAULT_INTERVAL_SECS`].
    pub(crate) interval_secs: Option<u64>,
    /// `--state` override; `None` → [`IssueState::Open`].
    pub(crate) state: Option<IssueState>,
}

/// Fully-resolved `tm watch` settings consumed by the poll/listen loops.
///
/// Why: once precedence is applied the loops want concrete, non-optional values
/// (a resolved `owner/repo`, a label, an interval, a state) so they never re-run
/// the merge or re-resolve the project.
/// What: the resolved `repo` (`owner/repo`), the routing `label`, the listen
/// `interval_secs`, and the `state` selection.
/// Test: produced and asserted by every `resolve_*` test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedWatch {
    /// The resolved board repository as `owner/repo`.
    pub(crate) repo: String,
    /// The routing label; only issues carrying it are picked up.
    pub(crate) label: String,
    /// The `listen`-mode poll interval, in seconds.
    pub(crate) interval_secs: u64,
    /// Which issues to consider (open vs all).
    pub(crate) state: IssueState,
}

/// Resolve the `<project>` token into an `owner/repo` board repository.
///
/// Why: the spec lets `<project>` be a direct `owner/repo` OR a registered
/// project name resolved via config. A direct `owner/repo` must win as-is; any
/// other token (a bare name, or anything that does not parse to two segments)
/// falls back to the configured `watch.repo`. Surfacing a clear error when
/// neither yields a repo stops the loop from running against a guessed board.
/// What: returns the trimmed `project` when it parses to a clean `owner/repo`
/// (two non-empty segments and no extra path noise); otherwise returns the
/// configured `repo`; otherwise an actionable error.
/// Test: `resolve_project_direct_owner_repo`, `resolve_project_name_uses_config`,
/// `resolve_project_unresolvable_errors`.
fn resolve_project(project: &str, config: &WatchConfig) -> anyhow::Result<String> {
    let token = project.trim();
    if looks_like_owner_repo(token) {
        return Ok(token.to_string());
    }
    if let Some(repo) = config
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(repo.to_string());
    }
    anyhow::bail!(
        "could not resolve project `{project}` — pass an `owner/repo` (e.g. \
         `bobmatnyc/trusty-tools`) or set `watch.repo` in \
         ~/.trusty-tools/trusty-mpm/config.yaml"
    )
}

/// Decide whether a token is a direct `owner/repo` reference.
///
/// Why: `resolve_project` must distinguish a literal `owner/repo` from a bare
/// registered-project name so the former is used verbatim and the latter falls
/// back to config. A `<owner>/<repo>` has exactly two non-empty, slash-separated
/// segments with no host/scheme noise.
/// What: returns true when `parse_github_path` yields a path whose rendered
/// `owner/repo` equals the lower-cased input — i.e. the token is already a clean
/// two-segment identity, not a URL or a single bare name.
/// Test: `looks_like_owner_repo_*`.
fn looks_like_owner_repo(token: &str) -> bool {
    if !token.contains('/') || token.contains("://") || token.contains(':') {
        return false;
    }
    // Exactly two non-empty segments.
    let segments: Vec<&str> = token.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() != 2 {
        return false;
    }
    // And it must round-trip through the canonical parser (rejects empty/odd).
    parse_github_path(token).is_some()
}

/// Merge raw CLI args with config defaults into [`ResolvedWatch`].
///
/// Why: the single place the precedence **CLI flag > config value > built-in
/// default** is applied, so `poll` and `listen` share identical resolution and
/// the rule is asserted once.
/// What: resolves the project to an `owner/repo`, then for each setting takes the
/// CLI override if present, else the config value, else the built-in default.
/// `state` has no config field (it is a per-run choice) so it falls straight
/// through to [`IssueState::Open`].
/// Test: `resolve_cli_overrides_config`, `resolve_config_used_when_no_cli`,
/// `resolve_uses_default_label_when_unset`, `resolve_uses_default_interval_when_unset`.
pub(crate) fn resolve(raw: &RawWatchArgs, config: &WatchConfig) -> anyhow::Result<ResolvedWatch> {
    let repo = resolve_project(&raw.project, config)?;

    let label = raw
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            config
                .label
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| DEFAULT_LABEL.to_string());

    let interval_secs = raw
        .interval_secs
        .or(config.interval_secs)
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_INTERVAL_SECS);

    let state = raw.state.unwrap_or_default();

    Ok(ResolvedWatch {
        repo,
        label,
        interval_secs,
        state,
    })
}
