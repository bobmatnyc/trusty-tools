//! Launch-time policy-label ensure (issue #3726, PM-brief decision; retired
//! lifecycle pair removed in #6914).
//!
//! Why: workstream activity is tracked on GitHub via a `ws/<session-name>`
//! label — explicitly NOT milestones, which stay reserved for epics/releases
//! (a GitHub repo allows only ONE milestone per issue, a slot epics/releases
//! already need; milestones are also a heavyweight lifecycle object, while a
//! workstream is ephemeral and freely renamable; labels, by contrast, are
//! multi-valued, cheap to create, and filterable exactly like the existing
//! `trusty-mpm` convention label). For the PM brief's `--label ws/<name>`
//! convention to work on the FIRST issue/PR a session ever files, the label
//! must already exist in the repo — `gh issue edit --add-label` fails on an
//! unknown label. This module makes label creation part of session launch
//! itself, so no PM ever hits that failure.
//! What: [`ensure_workstream_label`] is the launch-time entry point — given
//! the (optional) `repo_url` a session was spawned from, its workspace
//! directory, and its tmux session name, it derives the target
//! `<owner>/<repo>` (preferring the passed `repo_url`, falling back to the
//! workspace's own `git remote get-url origin`), and idempotently creates
//! `ws/<session-name>` via `gh label create --force`. Every non-GitHub-remote
//! case (no origin, non-GitHub host, `gh` missing/unauthenticated/timed-out) is
//! a logged, non-fatal skip — [`LabelOutcome`] is returned for callers/tests
//! that want to observe the branch taken, but production call sites (session
//! launch) MUST treat every variant as fire-and-forget: this must NEVER block
//! or fail a session launch.
//! The same launch hook ensures every OTHER policy label too, from the shared
//! [`crate::core::policy_labels`] table. Before #6914 this module carried its
//! own second table and was still creating a retired `in-progress` / `blocked`
//! pair the `status:*` lifecycle replaced.
//! Test: `owner_repo_from_ssh_origin`, `owner_repo_from_https_origin`,
//! `owner_repo_rejects_non_github_host`, `ensure_skips_when_no_origin`,
//! `ensure_skips_when_non_github`, `ensure_skips_when_name_blank`,
//! `ensure_creates_label_via_runner`, `ensure_reports_runner_failure` drive the
//! pure derivation + a scripted [`GhLabelRunner`] fake — no live `gh`/network.
//! The launch-path composition and its failure contract are covered by
//! `launch_labels_ensure_the_policy_set`,
//! `launch_labels_never_create_the_retired_lifecycle_pair`,
//! `convention_label_is_created_without_force`,
//! `convention_label_runs_even_when_workstream_label_fails`,
//! `workstream_label_runs_even_when_convention_label_fails`,
//! `launch_labels_survive_total_gh_failure`,
//! `launch_labels_skip_cleanly_on_non_github_remote`,
//! `launch_labels_ensure_convention_label_when_session_name_is_blank`,
//! `launch_labels_match_builtin_when_the_block_is_absent`,
//! `launch_labels_include_configured_extra_labels`,
//! `launch_labels_skip_entirely_when_ensure_labels_is_false`.
//!
//! #6918 made the set CONFIGURABLE without adding a second table: launch reads
//! [`crate::core::policy_labels::policy_labels_configured`] with the resolved
//! `agents.ticketing` block, and `agents.ticketing.ensure_labels: false` turns
//! the launch-time ensure off entirely.

use std::path::Path;
use std::time::Duration;

use trusty_common::github_path::GithubPath;

use crate::core::gh_account::run_bounded;
use crate::core::policy_labels::{self, PolicyLabel};
use crate::core::trusty_tools_config::{ResolvedTicketing, TrustyToolsConfig, resolve_ticketing};

/// Bound for the `gh label create` call this module makes at session launch.
///
/// Why: a launch must never hang on a wedged/slow `gh` (stuck credential
/// helper, offline network). 5s mirrors `gh_account`'s `GH_ENFORCE_TIMEOUT` —
/// generous enough for a real call, short enough to never meaningfully delay
/// a launch that already performs a git clone and multiple deploy steps.
const GH_LABEL_TIMEOUT: Duration = Duration::from_secs(5);

/// Outcome of one [`ensure_workstream_label`] call.
///
/// Why: production call sites only log this, but the unit tests (and any
/// future `tm doctor` surfacing) need a typed way to assert which branch was
/// taken instead of scraping log text.
/// What: one variant per skip reason, plus `Ensured` (the `gh` call
/// succeeded) and `GhFailed` (a `gh` call was attempted and failed/timed out).
/// Test: every variant is asserted by name in this module's tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelOutcome {
    /// The label was created/updated via `gh label create --force`.
    Ensured,
    /// `session_name` was empty/whitespace — nothing to label with.
    SkippedBlankName,
    /// Neither the supplied `repo_url` nor the workspace's own git origin
    /// resolved to a remote URL.
    SkippedNoOrigin,
    /// The resolved origin is not a `github.com` remote.
    SkippedNonGithub,
    /// A `gh` call was attempted but failed (non-zero exit, spawn error, or
    /// timed out past [`GH_LABEL_TIMEOUT`]) — covers "gh missing",
    /// "unauthenticated", and "offline" alike, since all three surface as an
    /// unsuccessful `gh` invocation.
    GhFailed,
}

/// Ensure the `ws/<session_name>` label exists on the session's GitHub repo.
///
/// Why: see the module docs — this is the launch-time hook that makes the PM
/// brief's `--label ws/<name>` convention usable from a session's very first
/// issue/PR.
/// What: derives `<owner>/<repo>` (via [`owner_repo_from_origin`] on
/// `repo_url` when present, else by reading `workspace_dir`'s
/// `remote.origin.url`), then creates the shared
/// [`policy_labels::workstream_label`] through [`RealGhRunner`], bounded by
/// [`GH_LABEL_TIMEOUT`]. Every failure path is a logged, non-fatal skip — see
/// [`LabelOutcome`].
/// Test: `ensure_creates_label_via_runner`, `ensure_skips_when_no_origin`,
/// `ensure_skips_when_non_github`, `ensure_skips_when_name_blank`.
pub fn ensure_workstream_label(
    repo_url: Option<&str>,
    workspace_dir: &Path,
    session_name: &str,
) -> LabelOutcome {
    ensure_workstream_label_with(&RealGhRunner, repo_url, workspace_dir, session_name)
}

/// [`ensure_workstream_label`] with the `gh` invocation seam injected, so
/// tests can drive it with a scripted [`GhLabelRunner`] instead of a live
/// `gh`.
fn ensure_workstream_label_with<R: GhLabelRunner>(
    runner: &R,
    repo_url: Option<&str>,
    workspace_dir: &Path,
    session_name: &str,
) -> LabelOutcome {
    // #6914: the ws/ label's name, color and description come from the shared
    // policy table, not from a second copy in this module.
    let Some(label) = policy_labels::workstream_label(session_name) else {
        tracing::debug!("workstream label: blank session name — skipping");
        return LabelOutcome::SkippedBlankName;
    };

    let gh_path = match resolve_github_repo(repo_url, workspace_dir) {
        Ok(gh_path) => gh_path,
        Err(skip) => {
            tracing::debug!(?skip, "workstream label: repo not resolvable — skipping");
            return skip;
        }
    };
    let repo = gh_path.rel_path();

    if create_policy_label(runner, &repo, &label) {
        tracing::info!(repo = %repo, label = %label.name, "workstream label ensured");
        LabelOutcome::Ensured
    } else {
        tracing::debug!(
            repo = %repo,
            label = %label.name,
            "workstream label: `gh label create` failed/unavailable — skipping (non-fatal)"
        );
        LabelOutcome::GhFailed
    }
}

/// Create one policy label on `repo`, best-effort, applying the shared
/// force rule.
///
/// Why: `--force` refreshes an existing label's color and description. That is
/// right for `ws/`, which is trusty-mpm's own namespace, and wrong for every
/// other policy label — those are ordinary repo labels a project may already
/// own and have styled, and forcing would silently rewrite that on every
/// launch. [`policy_labels::is_owned_namespace`] is the single place that rule
/// lives.
/// What: builds the argv with [`policy_labels::create_label_argv`] and returns
/// whether `gh` succeeded. A create that fails because the label already exists
/// is the expected steady state on the non-forced path, indistinguishable here
/// from a real `gh` failure and equally harmless.
/// Test: `ensure_creates_label_via_runner`,
/// `convention_label_is_created_without_force`.
fn create_policy_label<R: GhLabelRunner>(runner: &R, repo: &str, label: &PolicyLabel) -> bool {
    let force = policy_labels::is_owned_namespace(&label.name);
    runner.create_label(repo, label, force)
}

/// Every launch-time label ensure, in one call: the shared policy set from
/// [`policy_labels::policy_labels`].
///
/// Why: the daemon's spawn paths need a single fire-and-forget entry point, and
/// the ensures must be independent — a `gh` failure on any one must leave the
/// rest attempted and must never propagate to the launch.
/// What: runs [`ensure_workstream_label_with`] for the `ws/` label (discarding
/// its outcome, already logged), then resolves the repo again and creates every
/// remaining policy label. Nothing here returns or panics on a failed `gh`
/// call.
/// Test: `launch_labels_ensure_the_policy_set`,
/// `convention_label_runs_even_when_workstream_label_fails`,
/// `launch_labels_survive_total_gh_failure`.
fn ensure_launch_labels_with<R: GhLabelRunner>(
    runner: &R,
    ticketing: &ResolvedTicketing,
    repo_url: Option<&str>,
    workspace_dir: &Path,
    session_name: &str,
) {
    // #6918: an operator who set `agents.ticketing.ensure_labels: false` turned
    // OFF the launch-time ensure. `tm issue seed-labels` still seeds on demand.
    if !ticketing.ensure_labels {
        tracing::debug!("launch labels: agents.ticketing.ensure_labels is false — skipping");
        return;
    }
    let _ = ensure_workstream_label_with(runner, repo_url, workspace_dir, session_name);
    let Ok(gh_path) = resolve_github_repo(repo_url, workspace_dir) else {
        return;
    };
    let repo = gh_path.rel_path();
    // #6914: the ws/ label above is already ensured; everything else in the
    // shared policy set follows, each independently best-effort.
    // #6918: read through the config-aware call so `agents.ticketing` applies.
    for label in policy_labels::policy_labels_configured(ticketing, Some(session_name)) {
        if policy_labels::is_owned_namespace(&label.name) {
            continue;
        }
        if create_policy_label(runner, &repo, &label) {
            tracing::info!(repo = %repo, label = %label.name, "policy label created");
        } else {
            tracing::debug!(
                repo = %repo,
                label = %label.name,
                "policy label not created (already exists, or `gh` unavailable) — non-fatal"
            );
        }
    }
}

/// Resolve the `<owner>/<repo>` a session's labels belong to.
///
/// Why: both launch-time ensures need the same target, derived the same way —
/// one implementation, so the github.com-only rule cannot drift between them.
/// What: prefers a non-blank `repo_url`, falling back to `workspace_dir`'s own
/// `remote.origin.url`. `Err` carries the [`LabelOutcome`] skip the caller
/// should report.
/// Test: `ensure_skips_when_no_origin`, `ensure_skips_when_non_github`.
fn resolve_github_repo(
    repo_url: Option<&str>,
    workspace_dir: &Path,
) -> Result<GithubPath, LabelOutcome> {
    let origin = repo_url
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| origin_url_from_workspace(workspace_dir))
        .ok_or(LabelOutcome::SkippedNoOrigin)?;
    owner_repo_from_origin(&origin).ok_or(LabelOutcome::SkippedNonGithub)
}

/// Fire-and-forget [`ensure_launch_labels_with`] on the blocking thread pool.
///
/// Why: the ensures shell out to `gh` (bounded by
/// [`GH_LABEL_TIMEOUT`], but still up to a few seconds on a slow/offline
/// host); the daemon's `spawn_managed_*` branches call this at the identical
/// point (right before launching the runtime) across all three spawn shapes
/// (cloned, in-project, local-path-redirected). Detaching via
/// `tokio::task::spawn` + `spawn_blocking` — rather than `.await`ing inline —
/// guarantees it adds ZERO latency to the launch critical path, matching
/// `daemon::managed_routes::lifecycle`'s existing convention for blocking
/// `gh` calls made from async handlers (see that module's `resolve_gh_env`
/// doc comment).
/// What: clones the three owned inputs, spawns a detached task that runs
/// [`ensure_launch_labels_with`] on the blocking thread pool, and logs (at
/// debug) if the spawned task itself could not be joined — the ensure calls
/// already log their own outcomes internally. Must be called from inside a
/// tokio runtime (every call site is an async `spawn_managed_*` handler).
/// Test: [`ensure_launch_labels_with`]'s own unit tests cover the pure logic
/// this wraps; this wrapper is a thin, side-effect-only dispatch with
/// nothing pure to assert beyond "doesn't block", which the
/// `handler_spawn_*`/`resume_managed_*` integration tests already exercise
/// by not timing out.
pub fn spawn_workstream_label_ensure(
    repo_url: Option<String>,
    workspace_dir: std::path::PathBuf,
    session_name: String,
) {
    tokio::task::spawn(async move {
        if let Err(e) = tokio::task::spawn_blocking(move || {
            // #6918: a malformed `agents.ticketing` block must never fail a
            // launch — log it and ensure the built-in policy set instead.
            let ticketing = resolve_ticketing(&TrustyToolsConfig::load()).unwrap_or_else(|e| {
                tracing::warn!("agents.ticketing is invalid ({e}); using the built-in standard");
                ResolvedTicketing::default()
            });
            ensure_launch_labels_with(
                &RealGhRunner,
                &ticketing,
                repo_url.as_deref(),
                &workspace_dir,
                &session_name,
            )
        })
        .await
        {
            tracing::debug!("workstream label ensure task failed to join: {e}");
        }
    });
}

/// Read `remote.origin.url` from `dir` via `git config`, best-effort.
///
/// Why: the fallback path when no `repo_url` was threaded through the spawn
/// (mirrors [`trusty_common::github_path::derive_github_path`]'s own
/// `git config --get remote.origin.url` call, but this module needs the RAW
/// URL string first so it can apply its own github.com-host check before
/// parsing owner/repo).
/// What: `None` on any git failure (not a repo, no origin, git absent) or an
/// empty result.
fn origin_url_from_workspace(dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!url.is_empty()).then_some(url)
}

/// Parse a git remote URL into an `owner/repo` pair, but ONLY when its host is
/// `github.com` — unlike [`trusty_common::github_path::parse_github_path`],
/// which accepts any host.
///
/// Why: the PM-brief convention and `gh label create` both assume a
/// `github.com` repo; a GitLab/Bitbucket/self-hosted origin must skip
/// cleanly rather than have `gh` (silently pointed at the wrong host by its
/// own defaults) fail confusingly.
/// What: extracts the host from either SSH scp-syntax
/// (`git@github.com:owner/repo.git`) or a URL
/// (`https://github.com/owner/repo`, `ssh://git@github.com/owner/repo`),
/// case-insensitively compares it to `github.com`, and on a match delegates
/// owner/repo extraction to [`trusty_common::github_path::parse_github_path`].
/// Returns `None` for any other host or an unparseable input.
/// Test: `owner_repo_from_ssh_origin`, `owner_repo_from_https_origin`,
/// `owner_repo_from_https_origin_without_dot_git`,
/// `owner_repo_rejects_non_github_host`, `owner_repo_rejects_empty`.
fn owner_repo_from_origin(origin: &str) -> Option<GithubPath> {
    let trimmed = origin.trim();
    if trimmed.is_empty() {
        return None;
    }
    let after_scheme = trimmed
        .find("://")
        .map(|i| &trimmed[i + 3..])
        .unwrap_or(trimmed);
    let host_source = after_scheme
        .find('@')
        .map(|i| &after_scheme[i + 1..])
        .unwrap_or(after_scheme);
    let colon = host_source.find(':');
    let slash = host_source.find('/');
    let host = match (colon, slash) {
        (Some(c), s) if s.is_none_or(|s| c < s) => &host_source[..c],
        (_, Some(s)) => &host_source[..s],
        _ => host_source,
    };
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    trusty_common::github_path::parse_github_path(trimmed)
}

/// A seam for the one `gh` invocation this module makes, so tests can script
/// the outcome instead of shelling out to a live `gh`.
///
/// Why: mirrors the `CommandRunner` seam `tm ticket`/`watch` use
/// (`bin/tm/commands/ticket/runner.rs`) — that trait lives in the `tm` binary
/// crate and is not reachable from this library module, so this is a small,
/// purpose-built equivalent scoped to the one call this module needs. The
/// COMMAND LINE both seams build is shared
/// ([`policy_labels::create_label_argv`]); only the spawn differs.
/// What: `create_label` returns whether the `gh` call succeeded; callers never
/// see the raw stdout/stderr since label-create has nothing worth surfacing on
/// success and every failure is already a non-fatal skip.
/// Test: driven by `FakeGhRunner` in this module's tests.
trait GhLabelRunner {
    /// Run `gh label create …` for `label` against `repo` (with `--force` when
    /// `force`), returning whether it succeeded.
    fn create_label(&self, repo: &str, label: &PolicyLabel, force: bool) -> bool;
}

/// Production [`GhLabelRunner`] — spawns real `gh`, bounded by
/// [`GH_LABEL_TIMEOUT`].
struct RealGhRunner;

impl GhLabelRunner for RealGhRunner {
    fn create_label(&self, repo: &str, label: &PolicyLabel, force: bool) -> bool {
        // #6914: the argv comes from the crate's single `gh label create`
        // builder, not from a second copy spelled out here.
        let argv = policy_labels::create_label_argv(label, Some(repo), force);
        run_bounded(GH_LABEL_TIMEOUT, move || {
            // #5475: single `gh` entry point.
            let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
            trusty_common::gh::GhCommand::new(borrowed)
                .output_blocking()
                .ok()
                .map(|out| out.success)
        })
        .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // ── owner/repo host-scoped derivation ───────────────────────────────

    #[test]
    fn owner_repo_from_ssh_origin() {
        let gp = owner_repo_from_origin("git@github.com:bobmatnyc/trusty-tools.git")
            .expect("github ssh origin parses");
        assert_eq!(gp.owner, "bobmatnyc");
        assert_eq!(gp.repo, "trusty-tools");
    }

    #[test]
    fn owner_repo_from_https_origin() {
        let gp = owner_repo_from_origin("https://github.com/bobmatnyc/trusty-tools.git")
            .expect("github https origin parses");
        assert_eq!(gp.owner, "bobmatnyc");
        assert_eq!(gp.repo, "trusty-tools");
    }

    #[test]
    fn owner_repo_from_https_origin_without_dot_git() {
        let gp = owner_repo_from_origin("https://github.com/bobmatnyc/trusty-tools")
            .expect("github https origin without .git parses");
        assert_eq!(gp.owner, "bobmatnyc");
        assert_eq!(gp.repo, "trusty-tools");
    }

    #[test]
    fn owner_repo_rejects_non_github_host() {
        assert!(owner_repo_from_origin("git@gitlab.com:acme/widget.git").is_none());
        assert!(owner_repo_from_origin("https://bitbucket.org/acme/widget.git").is_none());
    }

    #[test]
    fn owner_repo_rejects_empty() {
        assert!(owner_repo_from_origin("").is_none());
        assert!(owner_repo_from_origin("   ").is_none());
    }

    // ── ensure_workstream_label skip-cleanly paths ──────────────────────

    /// One recorded `create_label` call: `(repo, name, color, description,
    /// force)`.
    type RecordedCall = (String, String, String, String, bool);

    struct FakeGhRunner {
        succeed: bool,
        /// Label names this runner fails for regardless of `succeed`.
        fail_for: Vec<&'static str>,
        calls: RefCell<Vec<RecordedCall>>,
    }

    impl FakeGhRunner {
        fn new(succeed: bool) -> Self {
            Self {
                succeed,
                fail_for: Vec::new(),
                calls: RefCell::new(Vec::new()),
            }
        }

        /// A runner that succeeds except for the named labels.
        fn failing_for(names: &[&'static str]) -> Self {
            Self {
                succeed: true,
                fail_for: names.to_vec(),
                calls: RefCell::new(Vec::new()),
            }
        }

        /// The label names passed to `create_label`, in call order.
        fn label_names(&self) -> Vec<String> {
            self.calls
                .borrow()
                .iter()
                .map(|(_, name, ..)| name.clone())
                .collect()
        }
    }

    impl GhLabelRunner for FakeGhRunner {
        fn create_label(&self, repo: &str, label: &PolicyLabel, force: bool) -> bool {
            self.calls.borrow_mut().push((
                repo.to_string(),
                label.name.clone(),
                label.color.clone(),
                label.description.clone(),
                force,
            ));
            self.succeed && !self.fail_for.contains(&label.name.as_str())
        }
    }

    #[test]
    fn ensure_skips_when_name_blank() {
        let runner = FakeGhRunner::new(true);
        let outcome = ensure_workstream_label_with(
            &runner,
            Some("https://github.com/bobmatnyc/trusty-tools.git"),
            Path::new("/nonexistent"),
            "   ",
        );
        assert_eq!(outcome, LabelOutcome::SkippedBlankName);
        assert!(runner.calls.borrow().is_empty(), "gh must not be invoked");
    }

    #[test]
    fn ensure_skips_when_no_origin() {
        let runner = FakeGhRunner::new(true);
        // A path that is (almost certainly) not inside any git repo and no
        // repo_url supplied.
        let outcome = ensure_workstream_label_with(&runner, None, Path::new("/"), "tm-tcode-01");
        assert_eq!(outcome, LabelOutcome::SkippedNoOrigin);
        assert!(runner.calls.borrow().is_empty(), "gh must not be invoked");
    }

    #[test]
    fn ensure_skips_when_non_github() {
        let runner = FakeGhRunner::new(true);
        let outcome = ensure_workstream_label_with(
            &runner,
            Some("git@gitlab.com:acme/widget.git"),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        assert_eq!(outcome, LabelOutcome::SkippedNonGithub);
        assert!(runner.calls.borrow().is_empty(), "gh must not be invoked");
    }

    #[test]
    fn ensure_creates_label_via_runner() {
        let runner = FakeGhRunner::new(true);
        let outcome = ensure_workstream_label_with(
            &runner,
            Some("https://github.com/bobmatnyc/trusty-tools.git"),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        assert_eq!(outcome, LabelOutcome::Ensured);
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 1);
        let (repo, name, color, description, force) = &calls[0];
        assert_eq!(repo, "bobmatnyc/trusty-tools");
        assert_eq!(name, "ws/tm-tcode-01");
        assert_eq!(color.len(), 6);
        assert_eq!(description, "trusty-mpm workstream tm-tcode-01");
        assert!(
            force,
            "the ws/ label is trusty-mpm's own — force is correct"
        );
    }

    #[test]
    fn ensure_reports_runner_failure() {
        let runner = FakeGhRunner::new(false);
        let outcome = ensure_workstream_label_with(
            &runner,
            Some("https://github.com/bobmatnyc/trusty-tools.git"),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        assert_eq!(outcome, LabelOutcome::GhFailed);
    }

    // ── launch-time composition (the shared policy set) ─────────────────

    const REPO_URL: &str = "https://github.com/bobmatnyc/trusty-tools.git";

    #[test]
    fn launch_labels_ensure_the_policy_set() {
        let runner = FakeGhRunner::new(true);
        ensure_launch_labels_with(
            &runner,
            &ResolvedTicketing::default(),
            Some(REPO_URL),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        assert_eq!(runner.label_names(), ["ws/tm-tcode-01", "trusty-mpm"]);
    }

    #[test]
    fn launch_labels_never_create_the_retired_lifecycle_pair() {
        // #6914: `in-progress` / `blocked` predate the `status:*` lifecycle.
        // Launch used to seed them on every session; it must not any more.
        let runner = FakeGhRunner::new(true);
        ensure_launch_labels_with(
            &runner,
            &ResolvedTicketing::default(),
            Some(REPO_URL),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        let names = runner.label_names();
        assert!(
            !names.iter().any(|n| n == "in-progress" || n == "blocked"),
            "launch must not create the retired pair; got {names:?}"
        );
    }

    #[test]
    fn convention_label_is_created_without_force() {
        let runner = FakeGhRunner::new(true);
        ensure_launch_labels_with(
            &runner,
            &ResolvedTicketing::default(),
            Some(REPO_URL),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        let calls = runner.calls.borrow();
        let (repo, name, color, description, force) = calls
            .iter()
            .find(|(_, name, ..)| name == "trusty-mpm")
            .expect("the convention label is ensured at launch");
        assert_eq!(repo, "bobmatnyc/trusty-tools");
        assert_eq!(name, "trusty-mpm");
        assert_eq!(color, &policy_labels::convention_label().color);
        assert_eq!(
            description,
            &policy_labels::convention_label().description,
            "description must come from the shared policy table"
        );
        assert!(
            !force,
            "an ordinary repo label — --force would rewrite a project's own \
             color/description on every launch"
        );
    }

    #[test]
    fn convention_label_runs_even_when_workstream_label_fails() {
        // The ws/ label's `gh` call fails; the rest of the policy set is an
        // independent concern and must still be attempted.
        let runner = FakeGhRunner::failing_for(&["ws/tm-tcode-01"]);
        ensure_launch_labels_with(
            &runner,
            &ResolvedTicketing::default(),
            Some(REPO_URL),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        assert_eq!(runner.label_names(), ["ws/tm-tcode-01", "trusty-mpm"]);
    }

    #[test]
    fn workstream_label_runs_even_when_convention_label_fails() {
        let runner = FakeGhRunner::failing_for(&["trusty-mpm"]);
        ensure_launch_labels_with(
            &runner,
            &ResolvedTicketing::default(),
            Some(REPO_URL),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        assert_eq!(
            runner.label_names(),
            ["ws/tm-tcode-01", "trusty-mpm"],
            "a policy-label failure must not suppress the ws/ label"
        );
    }

    #[test]
    fn launch_labels_survive_total_gh_failure() {
        // The launch-path contract: every `gh` call failing is a logged,
        // non-fatal skip. The call returns normally — no panic, no error to
        // propagate into `spawn_workstream_label_ensure`'s blocking task.
        let runner = FakeGhRunner::new(false);
        ensure_launch_labels_with(
            &runner,
            &ResolvedTicketing::default(),
            Some(REPO_URL),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        assert_eq!(runner.label_names(), ["ws/tm-tcode-01", "trusty-mpm"]);
    }

    #[test]
    fn launch_labels_skip_cleanly_on_non_github_remote() {
        let runner = FakeGhRunner::new(true);
        ensure_launch_labels_with(
            &runner,
            &ResolvedTicketing::default(),
            Some("git@gitlab.com:acme/widget.git"),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        assert!(
            runner.calls.borrow().is_empty(),
            "no gh call belongs on a non-github remote"
        );
    }

    #[test]
    fn launch_labels_ensure_convention_label_when_session_name_is_blank() {
        // A blank session name skips the ws/ label only — the rest of the
        // policy set does not depend on it.
        let runner = FakeGhRunner::new(true);
        ensure_launch_labels_with(
            &runner,
            &ResolvedTicketing::default(),
            Some(REPO_URL),
            Path::new("/nonexistent"),
            "  ",
        );
        assert_eq!(runner.label_names(), ["trusty-mpm"]);
    }

    // ── the config block at launch (#6918) ──────────────────────────────

    #[test]
    fn launch_labels_match_builtin_when_the_block_is_absent() {
        // #6918: a default `ResolvedTicketing` is what an absent
        // `agents.ticketing` resolves to, and it must seed exactly the #6914
        // set — same names, same order.
        let runner = FakeGhRunner::new(true);
        ensure_launch_labels_with(
            &runner,
            &ResolvedTicketing::default(),
            Some(REPO_URL),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        assert_eq!(runner.label_names(), ["ws/tm-tcode-01", "trusty-mpm"]);
    }

    #[test]
    fn launch_labels_include_configured_extra_labels() {
        let ticketing = ResolvedTicketing::default().with_extra_labels(vec![PolicyLabel::new(
            "area/cli",
            "0E8A16",
            "CLI surface",
        )]);
        let runner = FakeGhRunner::new(true);
        ensure_launch_labels_with(
            &runner,
            &ticketing,
            Some(REPO_URL),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        assert_eq!(
            runner.label_names(),
            ["ws/tm-tcode-01", "trusty-mpm", "area/cli"]
        );
    }

    #[test]
    fn launch_labels_skip_entirely_when_ensure_labels_is_false() {
        let ticketing = ResolvedTicketing::default().with_ensure_labels(false);
        let runner = FakeGhRunner::new(true);
        ensure_launch_labels_with(
            &runner,
            &ticketing,
            Some(REPO_URL),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        assert!(
            runner.calls.borrow().is_empty(),
            "ensure_labels: false turns the launch-time ensure off"
        );
    }
}
