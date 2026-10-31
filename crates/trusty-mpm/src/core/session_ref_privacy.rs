//! Keep a pause's session ref off public repositories (See #TBD-pause-ref).
//!
//! Why: a pause lease-pushed its snapshot to `refs/tm/sessions/<user>/<key>`
//! on `origin` whatever `origin` was. On a public repository that made the
//! summary, scratch paths, local home paths and the writer's name
//! world-readable, and the `<user>` segment could be a full personal name
//! taken from `git config user.name`.
//! What: [`origin_visibility`] asks GitHub whether the push URL's repository
//! is private, and [`RepoVisibility::refusal`] turns anything but a confirmed
//! `private` into a skip — fail closed. For the private path that still
//! publishes, [`redact_home`] rewrites the home directory to `~` and
//! [`anonymous_user_id`] replaces the personal ref segment with a stable hash.
//! Test: `session_ref_privacy_tests.rs`, plus the daemon wiring tests in
//! `daemon::mcp_context_session_refs_tests`.

use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

/// How long the `gh api` visibility lookup may take before it counts as unknown.
const VISIBILITY_TIMEOUT: Duration = Duration::from_secs(10);

/// What GitHub says about the repository a pause would push its ref to.
///
/// Why: three answers need two caller actions — publish, or skip — and the
/// skip must say which case it was, so the operator can tell "public" from
/// "could not tell".
/// What: `Unknown` carries the reason the lookup failed; it is treated exactly
/// like `Public`.
/// Test: `gh_output_maps_to_visibility`, `only_a_private_repo_is_allowed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoVisibility {
    /// GitHub answered `private: true`.
    Private,
    /// GitHub answered `private: false`; `0` is `<owner>/<repo>`.
    Public(String),
    /// The lookup did not produce a clear answer; `0` says why.
    Unknown(String),
}

impl RepoVisibility {
    /// The skip reason for every visibility except `Private`.
    pub fn refusal(&self) -> Option<String> {
        match self {
            Self::Private => None,
            Self::Public(repo) => Some(format!(
                "session ref not published: origin {repo} is a public repository; \
                 the snapshot stays in the local cache only"
            )),
            Self::Unknown(why) => Some(format!(
                "session ref not published: origin's visibility could not be \
                 confirmed as private ({why}); the snapshot stays in the local cache only"
            )),
        }
    }
}

/// Map the `gh api repos/<o>/<r> --jq .private` result to a visibility.
///
/// What: exactly `true` is `Private`, exactly `false` is `Public`; any other
/// output, and any failure, is `Unknown`.
/// Test: `gh_output_maps_to_visibility`.
pub fn visibility_from_gh(owner_repo: &str, result: Result<String, String>) -> RepoVisibility {
    match result.as_deref().map(str::trim) {
        Ok("true") => RepoVisibility::Private,
        Ok("false") => RepoVisibility::Public(owner_repo.to_string()),
        Ok(other) => RepoVisibility::Unknown(format!("`gh api` answered {other:?}")),
        Err(e) => RepoVisibility::Unknown(e.to_string()),
    }
}

/// The visibility of the repository `git push origin` would reach.
///
/// Why: the push URL, not the fetch URL, is where the snapshot lands, and
/// `pushInsteadOf` or a credential pin's `url.*.insteadOf` can redirect it —
/// so the URL is resolved under the same `env` the push runs with.
/// What: `git remote get-url --push origin` under `env`; a host other than
/// `github.com` is `Unknown` with no network call; otherwise
/// `gh api repos/<owner>/<repo> --jq .private` under `env`, bounded by
/// [`VISIBILITY_TIMEOUT`].
/// Test: `an_unknown_visibility_skips_the_publish` (daemon wiring) covers the
/// non-GitHub arm; the GitHub arm needs the network and is covered through
/// [`visibility_from_gh`].
pub fn origin_visibility(repo: &Path, env: &[(String, String)]) -> RepoVisibility {
    let mut cmd = trusty_common::git::command_in(repo);
    cmd.args(["remote", "get-url", "--push", "origin"]);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let url = match cmd.output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Ok(_) => return RepoVisibility::Unknown("origin has no push URL".to_string()),
        Err(e) => return RepoVisibility::Unknown(format!("git could not run: {e}")),
    };
    let remote = match trusty_common::github_path::parse_remote_url(&url) {
        Ok(remote) => remote,
        Err(e) => return RepoVisibility::Unknown(e.to_string()),
    };
    if !remote.host.eq_ignore_ascii_case("github.com") {
        return RepoVisibility::Unknown(format!("origin host {} is not github.com", remote.host));
    }
    let owner_repo = remote.owner_repo();
    let endpoint = format!("repos/{owner_repo}");
    let mut gh = trusty_common::gh::GhCommand::new(["api", &endpoint, "--jq", ".private"]);
    for (k, v) in env {
        gh = gh.env(k, v);
    }
    let result = crate::core::gh_account::run_bounded(VISIBILITY_TIMEOUT, move || {
        Some(gh.nonempty_stdout_blocking().map_err(|e| e.to_string()))
    })
    .unwrap_or_else(|| {
        Err(format!(
            "`gh api` did not answer within {VISIBILITY_TIMEOUT:?}"
        ))
    });
    visibility_from_gh(&owner_repo, result)
}

/// `content` with every occurrence of the home directory rewritten to `~`.
///
/// What: `None`, an empty path, or the bare root leaves `content` unchanged.
/// Test: `home_paths_are_redacted`.
pub fn redact_home(content: &str, home: Option<&Path>) -> String {
    let Some(home) = home.map(|h| h.to_string_lossy().trim_end_matches('/').to_string()) else {
        return content.to_string();
    };
    if home.is_empty() {
        return content.to_string();
    }
    content.replace(&home, "~")
}

/// A stable, non-personal ref segment for a writer id.
///
/// Why: the `<user>` segment came from the gh login or `git config user.name`,
/// which put a full personal name into a ref anyone with read access sees. A
/// hash keeps the segment stable per writer, across hosts, without the name.
/// What: `u` plus the first 12 hex digits of SHA-256 over `raw`.
/// Test: `anonymous_user_id_is_stable_and_non_personal`.
pub fn anonymous_user_id(raw: &str) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    format!("u{hex}")
}

#[cfg(test)]
#[path = "session_ref_privacy_tests.rs"]
mod tests;
