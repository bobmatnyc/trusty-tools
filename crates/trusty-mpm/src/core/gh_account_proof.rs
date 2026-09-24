//! Prove a candidate `gh` token belongs to a pinned account (#8510, #5851).
//!
//! Why: a token is proven only by asking GitHub who it is (owner ruling
//! 2026-09-24). A `hosts.yml` line, a `-u` lookup, or two lookups that agree
//! prove nothing: gh falls back to another account's keyring slot, and a
//! migrated slot can hold the wrong account's token.
//! What: [`prove_account_token`] asks each candidate dir from
//! [`AccountDirSources`] for `gh auth token --hostname <host> -u <login>`, then
//! sends a bounded `GET /user` with that token to the API base URL of the
//! repository's host. The first token whose `login` equals the pin
//! (case-insensitively) is returned as a [`ProvenToken`]. A network failure,
//! timeout, non-200 answer or unparsable body means not proven. Callers inject
//! the token itself, never a config dir: gh re-reads a config dir against the
//! keyring on every call, so a dir would be proven only at the moment it was
//! checked. No reason, log line or `Debug` output ever contains a token.
//! Test: `gh_account_dir_tests`.

use std::path::Path;
use std::time::Duration;

use crate::core::gh_account_dir::{AccountDirSources, refuse_unmigrated_config};

/// Asks `gh` for the token it holds for one login under one config dir.
///
/// Why: the lookup reads the keyring, which no test may touch. The trait is
/// the seam: production runs `gh`, tests answer from a table.
/// What: the token `gh auth token --hostname <host> -u <login>` prints with
/// `GH_CONFIG_DIR=<dir>` and every token variable removed. An `Err` never
/// contains a token.
/// Test: `a_token_for_another_account_is_refused`.
pub(crate) trait GhTokenProbe {
    /// The token `gh` holds under `dir` for `login` on `host`.
    fn token(&self, dir: &Path, host: &str, login: &str) -> Result<String, String>;
}

/// Asks GitHub which account a token authenticates as.
///
/// Why: the proof is a network call, which no test may make.
/// What: `login` returns the `login` field of a `200` answer to
/// `GET <api_base>/user` sent with `token`. Every other outcome is an `Err`
/// that never contains the token.
/// Test: `a_failed_user_check_is_not_proof`.
pub(crate) trait GhUserCheck {
    /// The login `token` authenticates as on the API at `api_base`.
    fn login(&self, api_base: &str, token: &str) -> Result<String, String>;
}

/// The production [`GhTokenProbe`]: runs the real `gh auth token`.
///
/// Why: `gh auth token` reads the config dir and the keyring. It is bounded,
/// because a wedged `securityd` hangs it (#6867).
/// What: refuses first via [`refuse_unmigrated_config`] — callers check too;
/// this is the last line before `gh` runs. Then
/// [`crate::session_manager::worktree_reclaim_gh::gh_command`] rooted at `dir`
/// with `GH_CONFIG_DIR=dir` and every inherited identity variable removed,
/// bounded by [`crate::core::gh_account::GH_ENFORCE_TIMEOUT`]. A unit-test
/// build never runs `gh` (#8510 incident). The failure reason carries `gh`'s
/// exit code and stderr, never its stdout.
/// Test: none directly (a real `gh` and keyring); the decisions it feeds are
/// covered through table probes in `gh_account_dir_tests`.
pub(crate) struct CliTokenProbe;

impl GhTokenProbe for CliTokenProbe {
    fn token(&self, dir: &Path, host: &str, login: &str) -> Result<String, String> {
        use crate::session_manager::worktree_reclaim_gh::{gh_command, run_with_timeout};
        if cfg!(test) {
            return Err("a unit-test build never runs gh (#8510)".to_string());
        }
        refuse_unmigrated_config(dir)?;
        let cfg = crate::core::trusty_tools_config::GithubConfig {
            config_dir: Some(dir.to_path_buf()),
            ..Default::default()
        };
        let env =
            crate::core::gh_identity::resolve_gh_env(Some(&cfg)).map_err(|e| e.to_string())?;
        let mut cmd = gh_command(dir, &env);
        cmd.args(["auth", "token", "--hostname", host, "-u", login]);
        let token = run_with_timeout(cmd, crate::core::gh_account::GH_ENFORCE_TIMEOUT)
            .map_err(|failure| failure.to_string())?;
        let token = token.trim();
        if token.is_empty() {
            return Err("printed no token".to_string());
        }
        Ok(token.to_string())
    }
}

/// How long one `GET /user` may take before it counts as not proven.
pub(crate) const GH_USER_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// The production [`GhUserCheck`]: a bounded `GET /user` over HTTPS.
///
/// Why: the owner ruling's proof. Redirects are not followed, so the token is
/// sent to the one URL derived from the repository's host and nowhere else.
/// What: runs on its own thread (a blocking client must not run on an async
/// executor), with [`GH_USER_CHECK_TIMEOUT`] on the request and a slightly
/// longer bound on the thread. A unit-test build never sends it (#8510).
/// Test: the answer parsing is `parse_user_login`'s; the request itself is
/// never sent from a test.
pub(crate) struct HttpUserCheck;

impl GhUserCheck for HttpUserCheck {
    fn login(&self, api_base: &str, token: &str) -> Result<String, String> {
        if cfg!(test) {
            return Err("a unit-test build never calls the GitHub API (#8510)".to_string());
        }
        let url = format!("{api_base}/user");
        let token = token.to_string();
        let shown = url.clone();
        crate::core::gh_account::run_bounded(GH_USER_CHECK_TIMEOUT + Duration::from_secs(1), {
            move || Some(fetch_user_login(&url, &token))
        })
        .unwrap_or_else(|| Err(format!("GET {shown} did not answer in time")))
    }
}

/// Send `GET url` with `token`; the body's `login` on a `200`.
fn fetch_user_login(url: &str, token: &str) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(GH_USER_CHECK_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("no HTTP client for GET {url} ({e})"))?;
    let response = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, format!("token {token}"))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(reqwest::header::USER_AGENT, "trusty-mpm")
        .send()
        .map_err(|e| format!("GET {url} failed ({})", e.without_url()))?;
    let status = response.status().as_u16();
    let body = response
        .text()
        .map_err(|e| format!("GET {url} body unreadable ({})", e.without_url()))?;
    parse_user_login(url, status, &body)
}

/// The `login` of a `GET /user` answer, or why it proves nothing.
///
/// What: `Ok` only for status `200` with a JSON body carrying a non-empty
/// string `login`. The body is never echoed.
/// Test: `parse_user_login_accepts_only_a_200_with_a_login`.
pub(crate) fn parse_user_login(url: &str, status: u16, body: &str) -> Result<String, String> {
    if status != 200 {
        return Err(format!("GET {url} answered HTTP {status}"));
    }
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| format!("GET {url} answered a body that is not JSON"))?;
    value
        .get("login")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("GET {url} answered no `login`"))
}

/// The gh host `origin` lives on, normalized the way gh does, or why not.
///
/// What: lowercased; `github.com` and any `*.github.com` are `github.com`.
/// Test: `api_base_url_and_token_var_follow_the_host_class`.
pub(crate) fn origin_host(origin: &str) -> Result<String, String> {
    let host = trusty_common::github_path::parse_remote_url(origin)
        .map(|remote| remote.host.to_ascii_lowercase())
        .map_err(|e| format!("cannot tell which gh host serves it ({e})"))?;
    Ok(if host == "github.com" || host.ends_with(".github.com") {
        "github.com".to_string()
    } else {
        host
    })
}

/// `true` for a host gh authenticates with `GH_TOKEN`: `github.com` and the
/// `*.ghe.com` data-residency hosts. Every other host is an Enterprise Server,
/// where gh reads `GH_ENTERPRISE_TOKEN` and ignores `GH_TOKEN`.
fn uses_gh_token(host: &str) -> bool {
    host == "github.com" || host.ends_with(".ghe.com")
}

/// The REST API base URL gh uses for `host`, with no trailing slash.
///
/// What: `https://api.github.com`; `https://api.<host>` for `*.ghe.com`;
/// `https://<host>/api/v3` for an Enterprise Server.
/// Test: `api_base_url_and_token_var_follow_the_host_class`.
pub(crate) fn api_base_url(host: &str) -> String {
    if host == "github.com" {
        "https://api.github.com".to_string()
    } else if host.ends_with(".ghe.com") {
        format!("https://api.{host}")
    } else {
        format!("https://{host}/api/v3")
    }
}

/// The env var gh reads a token from for `host` (#8510 HIGH).
/// Test: `api_base_url_and_token_var_follow_the_host_class`.
pub(crate) fn token_var_for(host: &str) -> &'static str {
    if uses_gh_token(host) {
        crate::core::gh_account::GH_TOKEN_ENV_VAR
    } else {
        GH_ENTERPRISE_TOKEN_ENV_VAR
    }
}

/// gh's token variable for an Enterprise Server host.
pub(crate) const GH_ENTERPRISE_TOKEN_ENV_VAR: &str = "GH_ENTERPRISE_TOKEN";

/// Both token variables, each set to `token` for its own host class and to
/// the nobody-token otherwise (#8510 HIGH).
///
/// Why: gh reads `GH_TOKEN` for github.com and `*.ghe.com`, and
/// `GH_ENTERPRISE_TOKEN` for an Enterprise Server. Setting only one leaves the
/// other class on the keyring's active account.
/// What: `(GH_TOKEN, _)` then `(GH_ENTERPRISE_TOKEN, _)`. `verified` is
/// `Some((host, token))` for a proven token; `None` puts the nobody-token in
/// both.
/// Test: `a_ghes_spawn_pin_puts_the_token_in_gh_enterprise_token`,
/// `a_ghes_spawn_pin_refusal_blanks_both_token_vars`.
pub(crate) fn identity_token_vars(verified: Option<(&str, &str)>) -> Vec<(String, String)> {
    let refused = crate::core::gh_account::REFUSED_GH_TOKEN;
    [
        crate::core::gh_account::GH_TOKEN_ENV_VAR,
        GH_ENTERPRISE_TOKEN_ENV_VAR,
    ]
    .into_iter()
    .map(|var| {
        let value = match verified {
            Some((host, token)) if token_var_for(host) == var => token,
            _ => refused,
        };
        (var.to_string(), value.to_string())
    })
    .collect()
}

/// A token `GET /user` proved to be the pinned login's, and its host.
///
/// Why: the token is a secret; `Debug` must never print it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProvenToken {
    /// The normalized gh host the token was proven on.
    pub(crate) host: String,
    token: String,
}

impl std::fmt::Debug for ProvenToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProvenToken")
            .field("host", &self.host)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl ProvenToken {
    /// The token variables that make the session's gh use this token only.
    /// Test: `a_ghes_spawn_pin_puts_the_token_in_gh_enterprise_token`.
    pub(crate) fn identity_vars(&self) -> Vec<(String, String)> {
        identity_token_vars(Some((&self.host, &self.token)))
    }

    /// A proven token built by a test fake.
    #[cfg(test)]
    pub(crate) fn for_test(host: &str, token: &str) -> Self {
        Self {
            host: host.to_string(),
            token: token.to_string(),
        }
    }
}

/// The first candidate token `GET /user` proves is `login`'s, else every
/// candidate's reason for refusal (#8510).
///
/// What: the host comes from `origin`; no host refuses. Each candidate from
/// [`AccountDirSources::candidates`] must exist and pass
/// [`refuse_unmigrated_config`] BEFORE `probe` runs; then its `-u <login>`
/// token must answer `check` with `login`. A failed candidate never stops the
/// loop.
/// Test: `the_second_candidate_is_used_when_the_first_fails`,
/// `a_token_for_another_account_is_refused`,
/// `a_failed_user_check_is_not_proof`,
/// `a_candidate_without_a_config_version_is_refused_before_gh_runs`,
/// `the_user_check_is_sent_to_the_hosts_api`.
pub(crate) fn prove_account_token(
    sources: &AccountDirSources,
    login: &str,
    origin: &str,
    probe: &dyn GhTokenProbe,
    check: &dyn GhUserCheck,
) -> Result<ProvenToken, Vec<String>> {
    let host = origin_host(origin).map_err(|e| vec![e])?;
    let api_base = api_base_url(&host);
    let mut reasons = Vec::new();
    for candidate in sources.candidates(login) {
        match candidate.and_then(|dir| prove_one(&dir, login, &host, &api_base, probe, check)) {
            Ok(token) => return Ok(ProvenToken { host, token }),
            Err(reason) => reasons.push(reason),
        }
    }
    Err(reasons)
}

/// Where an account-only pin looks for candidate tokens, and how it proves
/// them (#8510): the three seams [`prove_account_token`] takes, bundled.
pub(crate) struct AccountProver<'a> {
    /// The candidate config dirs.
    pub(crate) sources: &'a AccountDirSources,
    /// Asks `gh` for a candidate's token.
    pub(crate) probe: &'a dyn GhTokenProbe,
    /// Asks GitHub whose token it is.
    pub(crate) check: &'a dyn GhUserCheck,
}

impl AccountProver<'_> {
    /// [`prove_account_token`] over this prover's seams.
    /// Test: `the_second_candidate_is_used_when_the_first_fails`.
    pub(crate) fn prove(&self, login: &str, origin: &str) -> Result<ProvenToken, Vec<String>> {
        prove_account_token(self.sources, login, origin, self.probe, self.check)
    }
}

/// One candidate: exists, is safe to run gh in, and yields `login`'s token.
fn prove_one(
    dir: &Path,
    login: &str,
    host: &str,
    api_base: &str,
    probe: &dyn GhTokenProbe,
    check: &dyn GhUserCheck,
) -> Result<String, String> {
    let shown = dir.display();
    if !dir.is_dir() {
        return Err(format!("{shown} does not exist"));
    }
    refuse_unmigrated_config(dir)?;
    let token = probe.token(dir, host, login).map_err(|e| {
        format!("{shown}: `gh auth token --hostname {host} -u {login}` failed ({e})")
    })?;
    let actual = check
        .login(api_base, &token)
        .map_err(|e| format!("{shown}: the token's account could not be proven ({e})"))?;
    if !actual.eq_ignore_ascii_case(login) {
        return Err(format!(
            "{shown}: the token gh returned for '{login}' authenticates as '{actual}'"
        ));
    }
    Ok(token)
}
