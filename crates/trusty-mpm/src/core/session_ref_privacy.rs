//! Keep a pause's session ref off public repositories (See #10614).
//!
//! Why: a pause lease-pushed its snapshot to `refs/tm/sessions/<user>/<key>`
//! on `origin` whatever `origin` was. On a public repository that made the
//! summary, scratch paths, local home paths and the writer's name
//! world-readable, and the `<user>` segment could be a full personal name
//! taken from `git config user.name`.
//! What: [`origin_visibility`] asks GitHub whether every push URL's
//! repository is private, and [`RepoVisibility::refusal`] turns anything but a confirmed
//! `private` into a skip — fail closed. For the private path that still
//! publishes, [`redact_home`] rewrites the home directory to `~` and
//! [`anonymous_user_id`] replaces the personal ref segment with a stable hash.
//! Test: `session_ref_privacy_tests.rs`, plus the daemon wiring tests in
//! `daemon::mcp_context_session_refs_tests`.

use std::path::Path;
use std::process::Command;
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

/// The visibility of every repository `git push origin` would reach.
///
/// Why: the push URLs, not the fetch URL, are where the snapshot lands, and
/// `pushInsteadOf` or a credential pin's `url.*.insteadOf` can redirect them —
/// so they are resolved under the same `env` the push runs with.
/// What: [`origin_visibility_with`] with the real `gh api` lookup, each bounded
/// by [`VISIBILITY_TIMEOUT`] and killed and reaped on expiry.
/// Test: `an_unknown_visibility_skips_the_publish` (daemon wiring) covers the
/// non-GitHub arm; the per-URL rule is `every_push_url_must_be_private`.
pub fn origin_visibility(repo: &Path, env: &[(String, String)]) -> RepoVisibility {
    origin_visibility_with(repo, env, |owner_repo| {
        run_lookup(gh_visibility_command(owner_repo, env), VISIBILITY_TIMEOUT)
    })
}

/// [`origin_visibility`] with the per-repository `.private` lookup given.
///
/// What: `git remote get-url --push --all origin` under `env`, then
/// [`visibility_of_urls`] over every URL it lists.
/// Test: `every_push_url_must_be_private`.
pub fn origin_visibility_with(
    repo: &Path,
    env: &[(String, String)],
    lookup: impl FnMut(&str) -> Result<String, String>,
) -> RepoVisibility {
    let mut cmd = trusty_common::git::command_in(repo);
    // #10614: `--all` — `git push origin` pushes to EVERY push URL.
    cmd.args(["remote", "get-url", "--push", "--all", "origin"]);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let urls: Vec<String> = match cmd.output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        Ok(_) => return RepoVisibility::Unknown("origin has no push URL".to_string()),
        Err(e) => return RepoVisibility::Unknown(format!("git could not run: {e}")),
    };
    visibility_of_urls(&urls, lookup)
}

/// `Private` only when EVERY URL is a github.com repository GitHub confirms
/// private (#10614).
///
/// What: no URL, an unparsable URL or a host other than `github.com` is
/// `Unknown` with no lookup; otherwise the first URL whose lookup is not
/// `true` decides the answer.
/// Test: `every_push_url_must_be_private`.
pub fn visibility_of_urls(
    urls: &[String],
    mut lookup: impl FnMut(&str) -> Result<String, String>,
) -> RepoVisibility {
    if urls.is_empty() {
        return RepoVisibility::Unknown("origin has no push URL".to_string());
    }
    for url in urls {
        let remote = match trusty_common::github_path::parse_remote_url(url) {
            Ok(remote) => remote,
            Err(e) => return RepoVisibility::Unknown(e.to_string()),
        };
        if !remote.host.eq_ignore_ascii_case("github.com") {
            return RepoVisibility::Unknown(format!(
                "origin push host {} is not github.com",
                remote.host
            ));
        }
        let owner_repo = remote.owner_repo();
        match visibility_from_gh(&owner_repo, lookup(&owner_repo)) {
            RepoVisibility::Private => {}
            other => return other,
        }
    }
    RepoVisibility::Private
}

/// The `gh api repos/<owner>/<repo> --jq .private` command, pinned to github.com.
///
/// Why (#10614): an ambient `GH_HOST` would send the lookup to an enterprise
/// host, which could answer for a different repository of the same name.
/// What: `--hostname github.com`, the push `env` overlaid, then `GH_HOST`,
/// `GH_ENTERPRISE_TOKEN` and `GITHUB_ENTERPRISE_TOKEN` removed.
/// Test: `the_lookup_targets_github_com`.
pub fn gh_visibility_command(owner_repo: &str, env: &[(String, String)]) -> Command {
    let endpoint = format!("repos/{owner_repo}");
    let mut gh = trusty_common::gh::GhCommand::new([
        "api",
        "--hostname",
        "github.com",
        &endpoint,
        "--jq",
        ".private",
    ]);
    for (k, v) in env {
        gh = gh.env(k, v);
    }
    gh.env_remove("GH_HOST")
        .env_remove("GH_ENTERPRISE_TOKEN")
        .env_remove("GITHUB_ENTERPRISE_TOKEN")
        .to_std_command()
}

/// Run one lookup, killing and reaping it at `timeout` (#10614).
///
/// What: [`crate::core::bounded_proc::run_bounded`]; a zero exit with
/// non-blank stdout is `Ok(stdout)`, anything else — a timeout included — is
/// `Err`, which [`visibility_from_gh`] makes `Unknown`.
/// Test: `a_hung_lookup_is_killed_and_reaped`.
fn run_lookup(cmd: Command, timeout: Duration) -> Result<String, String> {
    use crate::core::bounded_proc::{BoundedError, run_bounded};
    match run_bounded(cmd, timeout) {
        Ok(out) if out.status.success() && !out.stdout.trim().is_empty() => {
            Ok(out.stdout.trim().to_string())
        }
        Ok(out) => Err(format!(
            "`gh api` failed ({}): {}",
            out.status,
            out.stderr.trim()
        )),
        Err(BoundedError::TimedOut) => Err(format!("`gh api` did not answer within {timeout:?}")),
        Err(e) => Err(format!("`gh api` could not run: {e}")),
    }
}

/// `content` with every occurrence of the home directory rewritten to `~`.
///
/// What: an occurrence is rewritten only when it ends at `/`, the end of the
/// text, or a character that cannot continue a path segment (#10614:
/// `/home/ada` must not rewrite `/home/adam`). A `.` continues the segment only
/// when a segment character follows it. `None`, an empty path, or the bare
/// root leaves `content` unchanged.
/// Test: `home_paths_are_redacted`.
pub fn redact_home(content: &str, home: Option<&Path>) -> String {
    let Some(home) = home.map(|h| h.to_string_lossy().trim_end_matches('/').to_string()) else {
        return content.to_string();
    };
    if home.is_empty() {
        return content.to_string();
    }
    let segment = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    let mut out = String::with_capacity(content.len());
    let mut last = 0;
    for (at, _) in content.match_indices(&home) {
        let mut rest = content[at + home.len()..].chars();
        let continues = match rest.next() {
            Some('.') => rest.next().is_some_and(segment),
            Some(c) => segment(c),
            None => false,
        };
        if continues {
            continue;
        }
        out.push_str(&content[last..at]);
        out.push('~');
        last = at + home.len();
    }
    out.push_str(&content[last..]);
    out
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
