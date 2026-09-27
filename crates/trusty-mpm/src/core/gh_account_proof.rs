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
//! Test: `gh_account_dir_tests`, `gh_account_proof_tests`.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
/// Test: the request is `send_user_request`'s, tested against a loopback
/// listener; this wrapper is never run from a test.
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

/// Send `GET url` with `token` over HTTPS only; the body's `login` on a `200`.
/// Test: `fetch_user_login_refuses_a_non_https_url`.
fn fetch_user_login(url: &str, token: &str) -> Result<String, String> {
    // #8510 r4: a token only ever leaves over TLS.
    if !url.starts_with("https://") {
        return Err(format!("refusing to send a token to non-https {url}"));
    }
    send_user_request(user_client(), url, token)
}

/// The client every `GET /user` uses: bounded, and never following a redirect.
fn user_client() -> reqwest::blocking::ClientBuilder {
    reqwest::blocking::Client::builder()
        .timeout(GH_USER_CHECK_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
}

/// Send `GET url` with `token` through `builder`'s client (#8510 r4 seam).
///
/// Why: [`fetch_user_login`] allows only https; a hermetic test reaches this
/// directly with a plain-http loopback URL.
/// Test: `a_redirect_is_refused_and_never_followed`,
/// `a_stalled_user_answer_times_out`,
/// `a_matching_login_is_read_and_the_token_is_sent`.
fn send_user_request(
    builder: reqwest::blocking::ClientBuilder,
    url: &str,
    token: &str,
) -> Result<String, String> {
    let client = builder
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
/// What: the remote's host through [`normalize_gh_host`].
/// Test: `api_base_url_and_token_var_follow_the_host_class`,
/// `origin_host_refuses_a_host_outside_the_hostname_set`.
pub(crate) fn origin_host(origin: &str) -> Result<String, String> {
    let host = trusty_common::github_path::parse_remote_url(origin)
        .map(|remote| remote.host)
        .map_err(|e| format!("cannot tell which gh host serves it ({e})"))?;
    normalize_gh_host(&host)
}

/// `host` lowercased and normalized the way gh does, or why it is no host.
///
/// Why: the host is spliced into the URL a token is sent to, so
/// `evil.com#.foo.ghe.com` must never become `https://api.evil.com#…` (#8510 r4).
/// What: refuses anything but ASCII letters, digits, `.` and `-`, optionally
/// followed by `:<digits>`; then `github.com` and any `*.github.com` become
/// `github.com`.
/// Test: `origin_host_refuses_a_host_outside_the_hostname_set`.
pub(crate) fn normalize_gh_host(host: &str) -> Result<String, String> {
    let host = host.trim().to_ascii_lowercase();
    let name = match host.split_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        Some(_) => "",
        None => host.as_str(),
    };
    let valid = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    if !valid {
        return Err(format!("'{host}' is not a valid gh host name"));
    }
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
    /// Remembers proofs across calls; `None` proves every time.
    pub(crate) cache: Option<&'a ProofCache>,
}

impl AccountProver<'_> {
    /// [`prove_account_token`] over this prover's seams.
    /// Test: `the_second_candidate_is_used_when_the_first_fails`.
    pub(crate) fn prove(&self, login: &str, origin: &str) -> Result<ProvenToken, Vec<String>> {
        self.prove_at(login, origin, Instant::now())
    }

    /// [`Self::prove`] as of `now`: a fresh remembered proof for the same
    /// login and host is returned without asking `gh` or GitHub (#8510 r4).
    /// Test: `a_remembered_proof_is_reused_within_its_ttl`,
    /// `a_remembered_proof_never_serves_another_login_or_host`,
    /// `an_expired_proof_is_proven_again`, `a_refusal_is_never_remembered`.
    pub(crate) fn prove_at(
        &self,
        login: &str,
        origin: &str,
        now: Instant,
    ) -> Result<ProvenToken, Vec<String>> {
        let Some(cache) = self.cache else {
            return prove_account_token(self.sources, login, origin, self.probe, self.check);
        };
        let key = (
            login.to_ascii_lowercase(),
            origin_host(origin).map_err(|e| vec![e])?,
        );
        if let Some(proven) = cache.fresh(&key, now) {
            return Ok(proven);
        }
        let proven = prove_account_token(self.sources, login, origin, self.probe, self.check)?;
        cache.remember(key, now, proven.clone());
        Ok(proven)
    }
}

/// How long a remembered proof is served: 5 minutes (#8510 r4).
///
/// Why: a token's account never changes, so the proof goes stale only when the
/// token is revoked or the operator logs the account out; 5 minutes bounds how
/// long either goes unnoticed while covering one hook run or reclaim sweep.
pub(crate) const PROOF_TTL: Duration = Duration::from_secs(300);

/// The proofs one process remembers, for [`PROOF_TTL`] (#8510 r4).
pub(crate) static PROCESS_PROOFS: ProofCache = ProofCache::new(PROOF_TTL);

/// A `(lowercased login, normalized host)` proof key.
type ProofKey = (String, String);

/// Proofs remembered per login and host (#8510 r4).
///
/// Why: the pm-guard removal hook proves once per repository and again for its
/// commit search inside a 3.5 s budget, and the reclaim sweep proves per
/// branch; each proof is a keyring read plus a network round trip.
/// Why refusals are not remembered: a refusal is usually transient (a dropped
/// network, a locked keyring, an account not yet logged in), and the
/// operator's fix must take effect on the next lookup. Remembering one would
/// not stop a false deny either: on the network that refused the proof, the
/// hook's own `gh` calls fail too.
/// What: an entry serves only its exact key, and only while younger than the
/// TTL; an older one is dropped on the next write. A poisoned lock is a miss.
/// Test: `a_remembered_proof_is_reused_within_its_ttl`,
/// `a_refusal_is_never_remembered`.
pub(crate) struct ProofCache {
    ttl: Duration,
    entries: Mutex<Vec<(ProofKey, Instant, ProvenToken)>>,
}

impl ProofCache {
    /// An empty cache whose entries live for `ttl`.
    pub(crate) const fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(Vec::new()),
        }
    }

    /// Is a proof proven at `at` still fresh at `now`?
    fn is_fresh(&self, at: Instant, now: Instant) -> bool {
        now.checked_duration_since(at)
            .is_some_and(|age| age < self.ttl)
    }

    /// The remembered proof for `key`, if it is still fresh at `now`.
    fn fresh(&self, key: &ProofKey, now: Instant) -> Option<ProvenToken> {
        let entries = self.entries.lock().ok()?;
        entries
            .iter()
            .find(|(k, at, _)| k == key && self.is_fresh(*at, now))
            .map(|(_, _, proven)| proven.clone())
    }

    /// Remember `proven` for `key` as of `now`, dropping stale entries.
    fn remember(&self, key: ProofKey, now: Instant, proven: ProvenToken) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|(k, at, _)| *k != key && self.is_fresh(*at, now));
            entries.push((key, now, proven));
        }
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

#[cfg(test)]
#[path = "gh_account_proof_tests.rs"]
mod gh_account_proof_tests;
