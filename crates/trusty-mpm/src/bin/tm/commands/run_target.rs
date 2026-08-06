//! `tm run <target>` routing: DOC-24 alias vs. daemon-managed cold start (#4990).
//!
//! Why: `tm run` used to accept exactly one thing — a DOC-24 registry alias —
//! and launch a blocking foreground `claude` for it. Starting work on a repo
//! that is not already registered and not already on disk had no entry point
//! at all: the daemon-managed system (ADR-0030's tm checkout at
//! `~/trusty-mpm-projects/<owner>/<repo>`, a real `SessionRecord`, a tmux pane)
//! can only be entered from a checkout that already exists, because
//! `try_inproject_spawn` gates on `.git` and derives the GitHub identity from
//! `remote.origin.url`. `tm run <owner>/<repo>` is the cold start: clone or
//! verify the managed checkout, then hand off to the daemon-managed launch.
//!
//! What: [`classify_run_target`] sorts the positional using the SAME predicate
//! `tm register` uses ([`super::register_args::looks_like_repo`]) so the two
//! commands cannot disagree about what a string means; [`run`] dispatches an
//! alias to the unchanged standalone driver and a repo to [`run_managed`],
//! which drives [`trusty_mpm::daemon::managed_routes::inproject_cold_start`]
//! and then the existing [`super::launch::launch`] path.
//! Test: `run_target_tests.rs`.

use trusty_common::github_path::{parse_github_path, parse_owner_repo};

/// What a `tm run` positional names.
///
/// Why: the positional now carries two disjoint meanings, and the disjointness
/// is real rather than assumed — a DOC-24 alias matches
/// `^[a-z0-9][a-z0-9._-]*$` (`core::standalone::registry::validate_alias`), so
/// it can never contain the `/` or `:` every repo form requires.
/// What: `Alias` is the pre-existing standalone target; `Repo` carries the
/// resolved GitHub identity and the URL to clone.
/// Test: `classify_sorts_alias_from_repo`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunTarget {
    /// A DOC-24 registry alias — the unchanged standalone `tm run <alias>`.
    Alias(String),
    /// A GitHub repo to cold-start under daemon management.
    Repo {
        /// Slugified repository owner.
        owner: String,
        /// Slugified repository name.
        repo: String,
        /// Fully-resolved clone URL.
        clone_url: String,
    },
}

/// Sort a `tm run` positional into the case that decides its outcome.
///
/// Why: routing has to happen before any I/O, so an unparseable repo string is
/// rejected with a usable message rather than becoming a `git clone` that
/// fails minutes later — or, worse, a registry lookup for an alias that was
/// never meant to be one.
/// What: delegates the repo-or-not question to
/// [`super::register_args::looks_like_repo`] and the URL resolution to
/// [`super::register_args::resolved_url`], so `tm run` inherits every rejection
/// `tm register` makes (browser pastes into a repo's web UI, relative paths,
/// host-only URLs). Identity comes from
/// [`trusty_common::github_path::parse_owner_repo`] for the `owner/repo`
/// shorthand — the shared, tested primitive for a canonical identity path —
/// falling back to [`trusty_common::github_path::parse_github_path`] for a full
/// URL, which `parse_owner_repo` refuses by design.
/// Test: `classify_sorts_alias_from_repo`, `classify_resolves_shorthand`,
/// `classify_resolves_full_urls`, `classify_rejects_browser_pastes`,
/// `shorthand_identity_agrees_with_url_identity`.
pub(crate) fn classify_run_target(spec: &str) -> anyhow::Result<RunTarget> {
    let spec = spec.trim();
    if spec.is_empty() {
        anyhow::bail!(
            "tm run needs a target. Pass <owner>/<repo> to start a managed session for a \
             GitHub repo, or a registered alias (see `tm ls --projects`)."
        );
    }

    // A relative path is neither a repo nor an alias, and `looks_like_repo`
    // answers `false` for it — so without this arm `tm run ./some/dir` would
    // become a registry lookup and report "alias not found" instead of the
    // path advice. An alias matches `^[a-z0-9][a-z0-9._-]*$` and can never
    // start with a dot.
    if super::register_args::is_relative_path(spec) {
        return Err(super::register_args::rejection(spec));
    }

    if !super::register_args::looks_like_repo(spec) {
        return Ok(RunTarget::Alias(spec.to_string()));
    }

    let clone_url = super::register_args::resolved_url(spec)?;
    let gh = parse_owner_repo(spec)
        .or_else(|| parse_github_path(&clone_url))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "cannot derive an owner/repo identity from '{spec}'. \
                 Pass <owner>/<repo>, or a full repository URL."
            )
        })?;

    Ok(RunTarget::Repo {
        owner: gh.owner,
        repo: gh.repo,
        clone_url,
    })
}

/// `tm run` handler.
///
/// Why: one command, two subsystems. Keeping the branch here rather than in
/// `main.rs` keeps the dispatch table flat and gives the routing decision a
/// testable home.
/// What: warns about the unimplemented `--task`, classifies the target, and
/// dispatches — alias to `core::standalone`'s driver (unchanged), repo to
/// [`run_managed`].
/// Test: `classify_sorts_alias_from_repo` covers the routing decision; the
/// managed arm's checkout half is covered by
/// `daemon::managed_routes::inproject_cold_start` tests.
pub(crate) async fn run(
    client: &reqwest::Client,
    url: &str,
    target: &str,
    task: Option<String>,
    root: Option<String>,
) -> anyhow::Result<()> {
    // `--task` is not implemented on either arm. Per DOC-24 autonomous/task
    // dispatch is the session-manager layer's concern; warn rather than drop
    // the flag silently.
    if let Some(ref t) = task {
        eprintln!(
            "warning: --task '{t}' is not yet implemented and will be ignored. \
             Task dispatch is handled by the session-manager layer."
        );
    }

    match classify_run_target(target)? {
        RunTarget::Alias(alias) => {
            let paths = super::managed_root::resolve_managed_paths(root.as_deref())?;
            super::standalone::run_cmd(&paths, &alias)
        }
        RunTarget::Repo {
            owner,
            repo,
            clone_url,
        } => run_managed(client, url, &owner, &repo, &clone_url).await,
    }
}

/// Cold-start a daemon-managed session for a GitHub `owner/repo`.
///
/// Why: this is the whole point of #4990 — a bare string ends in a real
/// `SessionRecord` with a tmux pane, visible in `tm ls` and `tm sessions`, not
/// a blocking foreground `claude`.
/// What: (1) ensures the managed base clone at
/// `<repos_root>/<owner>/<repo>` via
/// [`trusty_mpm::daemon::managed_routes::inproject_cold_start::ensure_managed_checkout`],
/// which clones when absent and fails loud on a mismatched remote or a dirty
/// tree; (2) hands that directory to [`super::launch::launch`], the existing
/// daemon-managed launch path — which registers the project alias, provisions
/// the per-session worktree, prepares the session, registers the session with
/// the daemon, creates the tmux host, and attaches.
///
/// Step 2 is a HANDOFF, not a reimplementation: everything after the checkout
/// exists already and is shared with `tm launch`, so a managed session started
/// cold is indistinguishable from one started from a local checkout.
/// Test: the checkout half is covered by the `inproject_cold_start` tests and
/// `tests/inproject_cold_start.rs`; the launch half is `tm launch`'s existing
/// coverage.
async fn run_managed(
    client: &reqwest::Client,
    url: &str,
    owner: &str,
    repo: &str,
    clone_url: &str,
) -> anyhow::Result<()> {
    let checkout =
        trusty_mpm::daemon::managed_routes::inproject_cold_start::ensure_managed_checkout(
            owner, repo, clone_url,
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if checkout.reused {
        eprintln!(
            "tm: reusing managed checkout {}",
            checkout.base_path.display()
        );
    } else {
        eprintln!("tm: cloned {clone_url} → {}", checkout.base_path.display());
    }

    super::launch::launch(
        client,
        url,
        Some(checkout.base_path.to_string_lossy().into_owned()),
        None,
    )
    .await
}

#[cfg(test)]
#[path = "run_target_tests.rs"]
mod tests;
