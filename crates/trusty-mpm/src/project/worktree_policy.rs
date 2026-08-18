//! The registry-keyed per-project worktree-isolation decision (#3455, #4300).
//!
//! Why: `Project::worktree` is a property of the PROJECT RECORD, so the
//! question "does this origin get a per-session worktree?" belongs next to
//! the record — not inside the daemon's spawn route, where #3455 first put it
//! (`daemon::managed_routes::launch_on_main::worktree_enabled_for_origin`,
//! `pub(super)`-scoped to `daemon::managed_routes`). That scoping was correct
//! while the daemon was the only decision point; #4300 proved it was not —
//! `tm launch` and the daemon-unreachable bare-`tm` fallback both provision
//! worktrees IN THE CLI PROCESS, could not reach the check, and silently
//! ignored the opt-out. Hoisting the decision here (rather than merely
//! relaxing the daemon module's visibility) keeps the CLI from reaching into
//! `daemon::` for what is a registry query, and gives the out-of-process
//! callers a supported entry point that reads the same `projects.json` the
//! daemon does.
//! What: [`worktree_enabled_in`] is the pure core (a matched project's
//! [`Project::worktree_enabled`], else the `true` default);
//! [`worktree_enabled_for_origin`] is the in-process form used by the daemon,
//! which already holds a live [`ProjectRegistry`];
//! [`worktree_enabled_for_origin_at`] is the out-of-process form used by the
//! `tm` CLI, which has no daemon state and loads the registry from disk;
//! [`registry_data_dir_under`]/[`registry_data_dir`] name the on-disk
//! directory ONCE so the daemon and the CLI can never read different files.
//! Every fallible step resolves to `true` (isolation ON, the pre-#3455
//! behaviour) so an unreadable registry can never silently strip a project of
//! its worktrees.
//! Test: `worktree_enabled_for_origin_defaults_true_when_unregistered`,
//! `worktree_enabled_for_origin_honors_registered_false`,
//! `worktree_enabled_at_reads_registry_from_disk`,
//! `worktree_enabled_at_defaults_true_when_registry_absent`,
//! `registry_dir_name_is_frozen` in `worktree_policy_tests.rs`.

use std::path::{Path, PathBuf};

use super::record::{Project, repo_url_matches};
use super::registry::ProjectRegistry;
use crate::core::project_config::{DispatchIsolation, load_or_report};

/// Directory name, under the framework root, holding `projects.json`.
///
/// Why: the daemon (`DaemonState::project_registry`) and the CLI
/// (`registry_data_dir`) must agree on this byte-for-byte; a divergence would
/// make the CLI read an empty registry and silently answer `true` for every
/// project — re-opening #4300 without any test noticing.
/// What: `"project-registry"`.
/// Test: `registry_dir_name_is_frozen`.
pub const REGISTRY_DIR_NAME: &str = "project-registry";

/// Resolve the project-registry data directory under an explicit framework root.
///
/// Why: the daemon holds its framework root as state and must not re-derive it
/// from `$HOME`; taking it as a parameter keeps [`registry_data_dir`] (the
/// `$HOME`-derived CLI form) a thin wrapper rather than a second source of
/// truth.
/// What: `<framework_root>/project-registry`.
/// Test: `registry_dir_name_is_frozen`.
pub fn registry_data_dir_under(framework_root: &Path) -> PathBuf {
    framework_root.join(REGISTRY_DIR_NAME)
}

/// Resolve the project-registry data directory for a process with no daemon state.
///
/// Why: `tm launch` and the daemon-unreachable guided fallback run entirely in
/// the CLI process and still must honour the opt-out (#4300). They have no
/// `DaemonState`, so they resolve the same framework root the daemon would.
/// What: `FrameworkPaths::default().root` joined with [`REGISTRY_DIR_NAME`].
/// Test: `registry_dir_name_is_frozen` pins the directory name; the
/// `$HOME`-derived root itself is covered by `core::paths`' own tests.
pub fn registry_data_dir() -> PathBuf {
    registry_data_dir_under(&crate::core::paths::FrameworkPaths::default().root)
}

/// Pure core: is worktree isolation enabled for `origin` given `projects`?
///
/// Why: isolating the matching rule from all I/O makes the two defaults that
/// matter — "no match ⇒ `true`" and "match ⇒ [`Project::worktree_enabled`]" —
/// directly assertable, and lets both the daemon and the CLI share one
/// definition of "the same project".
/// What: finds the first project whose `repo_url` matches `origin` under
/// [`repo_url_matches`] (which normalises SSH vs HTTPS and a `.git` suffix via
/// `parse_github_path`) and returns its `worktree_enabled()`; returns `true`
/// when nothing matches.
/// Test: `worktree_enabled_in_defaults_true`, `worktree_enabled_in_honors_false`,
/// `worktree_enabled_in_matches_ssh_form`.
pub fn worktree_enabled_in(projects: &[Project], origin: &str) -> bool {
    projects
        .iter()
        .find(|p| repo_url_matches(&p.repo_url, origin))
        .map(|p| p.worktree_enabled())
        .unwrap_or(true)
}

/// Resolve worktree isolation for `origin` against a live in-process registry.
///
/// Why: the daemon already holds a `ProjectRegistry`; re-loading it from disk
/// per spawn would drop the shared reload/lock discipline `ProjectStore`
/// provides.
/// What: delegates to [`worktree_enabled_in`] over `registry.list()`, treating
/// an unreadable registry as `true` (isolation ON — no regression).
/// Test: `worktree_enabled_for_origin_defaults_true_when_unregistered`,
/// `worktree_enabled_for_origin_honors_registered_false`.
pub async fn worktree_enabled_for_origin(registry: &ProjectRegistry, origin: &str) -> bool {
    let Ok(projects) = registry.list().await else {
        return true;
    };
    worktree_enabled_in(&projects, origin)
}

/// Resolve worktree isolation for `origin` by reading `projects.json` directly.
///
/// Why (#4300): the `tm` CLI provisions worktrees in its own process — `tm
/// launch` never asks the daemon at all, and the guided fallback runs
/// precisely BECAUSE the daemon is unreachable — so neither can be served by
/// [`worktree_enabled_for_origin`]. `ProjectStore` is explicitly designed for
/// multi-process readers (see its module `Concurrency` note), so an
/// out-of-process read is the supported way in, not a workaround.
/// What: loads a [`ProjectRegistry`] from `registry_dir` and delegates to
/// [`worktree_enabled_for_origin`]; a registry that fails to load (absent,
/// unreadable, malformed) resolves to `true` — an unreadable registry must
/// never strip a project of the isolation it had before #3455.
/// Test: `worktree_enabled_at_reads_registry_from_disk`,
/// `worktree_enabled_at_defaults_true_when_registry_absent`,
/// `worktree_enabled_at_defaults_true_on_malformed_registry`.
pub async fn worktree_enabled_for_origin_at(registry_dir: &Path, origin: &str) -> bool {
    match ProjectRegistry::load(registry_dir).await {
        Ok(registry) => worktree_enabled_for_origin(&registry, origin).await,
        Err(e) => {
            tracing::warn!(
                dir = %registry_dir.display(),
                "worktree opt-out: project registry unreadable ({e}); \
                 defaulting to worktree isolation ON"
            );
            true
        }
    }
}

/// The project's own answer, read from its committed `.trusty-mpm.toml` (#5207).
///
/// Why: the registry (`projects.json`) is MACHINE-GLOBAL, so "this repo launches
/// on main" had to be re-declared by every operator on every host and could
/// never be reviewed. The project itself is the right place to state a property
/// of its own workflow, and a committed file travels with the clone. This is
/// split from [`worktree_enabled_for_project`] so the layer can be asserted on
/// its own, without a registry.
/// What: `Some(v)` when the project ships a config that sets `worktree`; `None`
/// when the file is absent, sets no `worktree` key, or was REJECTED (an
/// unknown key or a bad type — [`load_or_report`] logs it and yields nothing).
/// A project that declines to decide falls through to the registry.
/// Test: `worktree_project_config_overrides_registry`,
/// `worktree_project_config_absent_falls_back_to_registry`,
/// `worktree_project_config_malformed_falls_back_to_registry`.
pub fn worktree_override_in_project(project_dir: &Path) -> Option<bool> {
    load_or_report(project_dir).and_then(|cfg| cfg.worktree)
}

/// Does this project isolate the agents it DISPATCHES, or leave them in the
/// checkout? (#5814, ADR-0051.)
///
/// Why: ADR-0048 decision 1's worktree grant is mechanical and never reads the
/// dispatch prompt, so a project that wants none of it has no way to say so. It
/// says so here, in the same committed file that already carries the SESSION
/// worktree decision — see [`DispatchIsolation`] for why the two are separate
/// fields rather than one. Resolution lives beside
/// [`worktree_override_in_project`] because both answer "what worktree policy
/// does this project declare"; a second reader of `.trusty-mpm.toml` elsewhere
/// would be a second definition of that question.
///
/// **Fail-closed, and this is the whole point of the function.** Every arm that
/// is not an affirmative, successfully-parsed `main-checkout` resolves to
/// [`DispatchIsolation::Worktree`]: an absent file, an unreadable one, a
/// malformed one, one carrying an unknown key or an unknown token, and one that
/// simply sets no `dispatch_isolation`. The failure this direction prevents is
/// silent — an agent writing into a shared checkout raises no error at any step
/// — so a default, an error, or a `false` must never be what drops isolation.
/// [`load_or_report`] supplies the first four arms by rejecting a bad file
/// WHOLESALE and logging at `error`; `unwrap_or_default()` supplies the last.
/// What: `project_dir` is the MAIN CHECKOUT ROOT, not the caller's cwd — the
/// config file sits at the checkout root and a dispatch made from a
/// subdirectory must read the same declaration. Callers resolve it with
/// [`crate::core::project_aliases::main_checkout_root`].
/// Test: `dispatch_isolation_reads_an_opted_out_project`,
/// `dispatch_isolation_defaults_to_worktree_without_a_config`,
/// `dispatch_isolation_defaults_to_worktree_on_a_malformed_config`,
/// `dispatch_isolation_defaults_to_worktree_on_an_unknown_token`,
/// `dispatch_isolation_defaults_to_worktree_on_an_unreadable_config`,
/// `dispatch_isolation_defaults_to_worktree_when_the_key_is_absent`.
pub fn dispatch_isolation_for_project(project_dir: &Path) -> DispatchIsolation {
    // #5814: only an affirmative, successfully-read opt-out lifts the ADR-0048
    // grant; every other outcome keeps isolation on.
    let decided = load_or_report(project_dir)
        .and_then(|cfg| cfg.dispatch_isolation)
        .unwrap_or_default();
    if decided == DispatchIsolation::MainCheckout {
        tracing::debug!(
            dir = %project_dir.display(),
            "dispatch isolation: this project's .trusty-mpm.toml exempts dispatched agents \
             from the ADR-0048 worktree grant"
        );
    }
    decided
}

/// Resolve worktree isolation for a project that EXISTS ON DISK (#5207).
///
/// Why: the in-project spawn branch has already resolved the local checkout
/// before it asks this question, so it can consult the project's own committed
/// config — the highest-precedence layer. The clone-based branch cannot: there
/// is no project directory yet, which is why
/// [`worktree_enabled_for_origin`] remains the entry point there rather than
/// being replaced by this one.
/// What: precedence, highest first — the project's `.trusty-mpm.toml`
/// (`worktree`), then the machine-global registry record, then the built-in
/// `true`. Every fallible step still resolves toward `true`, so no failure can
/// silently strip a project of the isolation it had before #3455.
/// Test: `worktree_project_config_overrides_registry`,
/// `worktree_project_config_can_force_isolation_on`,
/// `worktree_project_config_absent_falls_back_to_registry`,
/// `worktree_project_config_malformed_falls_back_to_registry`,
/// `worktree_defaults_true_with_neither_layer`.
pub async fn worktree_enabled_for_project(
    project_dir: &Path,
    registry: &ProjectRegistry,
    origin: &str,
) -> bool {
    // #5207: the project's committed config outranks the machine-global
    // registry, so the repo's own workflow wins over one operator's local state.
    if let Some(decided) = worktree_override_in_project(project_dir) {
        tracing::debug!(
            dir = %project_dir.display(),
            worktree = decided,
            "worktree policy: decided by the project's committed .trusty-mpm.toml"
        );
        return decided;
    }
    worktree_enabled_for_origin(registry, origin).await
}

#[cfg(test)]
#[path = "worktree_policy_tests.rs"]
mod tests;
