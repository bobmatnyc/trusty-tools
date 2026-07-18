//! Active-`gh`-account awareness for every `tm` surface that shells to GitHub.
//!
//! Why: `tm` makes many `gh` calls (PR list/view/merge, issue edits, managed
//! spawn), but historically had NO awareness of WHICH github.com identity was
//! active. When a host has several accounts logged in (the common
//! personal + work + bot setup), `gh` uses whichever one is marked active in its
//! `hosts.yml` — and that is easy to get wrong. This directly caused a silent
//! failure: the active account was a non-admin identity (`bob-duetto`) instead
//! of the repo owner (`bobmatnyc`), so `gh pr merge --admin` kept failing with
//! "1 approving review required" until the account was switched. This module
//! makes the active account (and the multi-account ambiguity that hides the bug)
//! a first-class, always-inspectable fact for the statusline and `tm doctor`.
//!
//! What: the CHEAP, subprocess-free path reads `gh`'s own `hosts.yml`
//! (honouring `GH_CONFIG_DIR` / `XDG_CONFIG_HOME`, else `~/.config/gh/`) and
//! parses `github.com.user` (the active login) plus the `github.com.users` map
//! (every logged-in login). [`gh_account_status_local`] returns both from one
//! file read with no subprocess — safe for the statusline's hot render path.
//! [`active_gh_account`] falls back to a bounded `gh auth status --active`
//! subprocess when the local file is absent, and [`logged_in_gh_accounts`]
//! parses `gh auth status` for the full multi-account list. Every entry point is
//! fail-soft: a missing `gh`, absent config, or parse error yields `None` /
//! an empty vec, never an error or panic.
//!
//! Test: the pure parsers (`parse_active_account_from_hosts_yml`,
//! `parse_logged_in_accounts`, `parse_gh_account_status_from_hosts_yml`) are
//! unit-tested against sample `hosts.yml` and `gh auth status` strings in the
//! inline `tests` module; the subprocess wrappers are thin bounded glue.
//!
//! ## Per-project account enforcement (#2081)
//!
//! [`ensure_gh_account_for_project`] is the #2081 mechanism for defaulting
//! `gh` operations on a project to its configured [`crate::project::record::Project::gh_user`].
//! It deliberately does NOT run a bare, unscoped `gh auth switch` — `tm`'s
//! daemon manages many projects concurrently, and `gh auth switch` rewrites the
//! single SHARED `~/.config/gh/hosts.yml` "active" pointer, so switching for
//! project A would silently redirect project B's concurrent `gh` calls to the
//! wrong identity. Instead it requires an already-isolated `GH_CONFIG_DIR`
//! (the convention this codebase's `gh_identity` module and the operator's own
//! shell tooling already use to scope `gh` state per project/context) and only
//! ever mutates the "active" pointer INSIDE that one directory, verifying the
//! result with a fresh `gh auth status` before returning success. No
//! `GH_CONFIG_DIR` in the environment → refusal, never a fall-through to the
//! shared global store.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;

/// Bounded wall-clock timeout for any `gh` subprocess this module spawns.
///
/// Why: both the statusline (hot render path) and `tm doctor` must never block
/// on a wedged `gh` (stuck credential helper, slow keyring, network). A short
/// bound turns "hung" into a clean "omit the segment / warn".
/// What: 250 ms — generous enough for a local `gh auth status` yet short enough
/// to never stall a render cycle.
/// Test: covered indirectly (the parsers are the tested surface).
pub const GH_PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// A snapshot of the local `gh` github.com account state.
///
/// Why: callers (statusline, doctor) need BOTH the active login and the count of
/// logged-in accounts in one value, so a single cheap `hosts.yml` read answers
/// "who is active?" and "is there dangerous ambiguity?" together.
/// What: `active` is `github.com.user` (the login `gh` will actually use);
/// `logged_in` is every login under `github.com.users` (or just `active` when no
/// `users` map is present). `logged_in.len() > 1` flags the multi-account
/// ambiguity that hid the #admin-merge bug.
/// Test: `parse_gh_account_status_*` construct and assert this.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GhAccountStatus {
    /// The active github.com login (`hosts.yml` → `github.com.user`), if any.
    pub active: Option<String>,
    /// Every github.com login logged in on this host.
    pub logged_in: Vec<String>,
}

impl GhAccountStatus {
    /// Whether more than one github.com account is logged in (the ambiguity).
    ///
    /// Why: a single logged-in account is unambiguous; two or more means `gh`
    /// silently picks the active one and the operator can easily be on the wrong
    /// identity — the exact condition worth flagging everywhere.
    /// What: true when `logged_in` holds more than one login.
    /// Test: `parse_gh_account_status_multi_from_users_map`.
    pub fn is_ambiguous(&self) -> bool {
        self.logged_in.len() > 1
    }
}

/// Resolve `gh`'s config directory, honouring the same env vars `gh` itself does.
///
/// Why: `tm` (and its managed sessions) may set `GH_CONFIG_DIR` to isolate a
/// per-project identity; reading the DEFAULT path in that case would report the
/// wrong account. Mirroring `gh`'s own precedence keeps our view identical to
/// what `gh` will actually use.
/// What: returns `GH_CONFIG_DIR` when set and non-empty, else
/// `XDG_CONFIG_HOME/gh`, else `~/.config/gh`.
/// Test: exercised via `gh_hosts_yml_path` behaviour; env-dependent so not
/// asserted directly.
fn gh_config_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("GH_CONFIG_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let xdg = xdg.trim();
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("gh"));
        }
    }
    dirs::home_dir().map(|h| h.join(".config").join("gh"))
}

/// Path to `gh`'s `hosts.yml`, honouring the resolved config dir.
///
/// Why: the cheap, subprocess-free account read needs the exact file `gh` writes
/// its host/auth state to.
/// What: `<gh-config-dir>/hosts.yml`.
/// Test: env-dependent; covered indirectly by `gh_account_status_local`.
fn gh_hosts_yml_path() -> Option<PathBuf> {
    Some(gh_config_dir()?.join("hosts.yml"))
}

/// Parse the active github.com login from a `gh` `hosts.yml` document.
///
/// Why: the single-value hot-path answer ("who will `gh` act as?") without
/// spawning a subprocess.
/// What: reads `github.com.user`, trimmed; returns `None` for a missing key,
/// empty value, or unparseable YAML.
/// Test: `parse_active_account_present`, `parse_active_account_absent`.
pub fn parse_active_account_from_hosts_yml(yaml: &str) -> Option<String> {
    let doc: serde_yaml::Value = serde_yaml::from_str(yaml).ok()?;
    let user = doc.get("github.com")?.get("user")?.as_str()?.trim();
    if user.is_empty() {
        None
    } else {
        Some(user.to_string())
    }
}

/// Parse the full account status (active + all logged-in) from a `hosts.yml`.
///
/// Why: the statusline and doctor both need to know not just who is active but
/// whether MULTIPLE accounts are logged in (the ambiguity that hid the bug), and
/// `hosts.yml` carries both — so one cheap read answers everything.
/// What: reads `github.com.user` as `active` and every key under
/// `github.com.users` as `logged_in`; when no `users` map exists it falls back
/// to `[active]`. Returns `None` only when neither an active user nor any
/// logged-in account can be found (or the YAML is unparseable).
/// Test: `parse_gh_account_status_single`,
/// `parse_gh_account_status_multi_from_users_map`, `parse_gh_account_status_empty`.
pub fn parse_gh_account_status_from_hosts_yml(yaml: &str) -> Option<GhAccountStatus> {
    let doc: serde_yaml::Value = serde_yaml::from_str(yaml).ok()?;
    let host = doc.get("github.com")?;

    let active = host
        .get("user")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let mut logged_in: Vec<String> = Vec::new();
    if let Some(users) = host.get("users").and_then(|v| v.as_mapping()) {
        for key in users.keys() {
            if let Some(name) = key.as_str() {
                let name = name.trim();
                if !name.is_empty() && !logged_in.iter().any(|e| e == name) {
                    logged_in.push(name.to_string());
                }
            }
        }
    }
    // No `users:` map (older/single-account layout) — the active user is the
    // only known account.
    if logged_in.is_empty()
        && let Some(a) = &active
    {
        logged_in.push(a.clone());
    }

    if active.is_none() && logged_in.is_empty() {
        return None;
    }
    Some(GhAccountStatus { active, logged_in })
}

/// Parse every logged-in github.com login from `gh auth status` output.
///
/// Why: the subprocess path (used when `hosts.yml` is absent, or by the doctor
/// for a definitive list) needs to enumerate accounts from `gh`'s human-readable
/// status text, which is the authoritative multi-account source.
/// What: scans each line for `gh`'s "Logged in to github.com account <login>"
/// (and the older "as <login>") phrasing and collects the `<login>` tokens,
/// de-duplicated and order-preserving. Tolerant of leading check-marks and
/// trailing `(keyring)` / `(...)` annotations.
/// Test: `parse_logged_in_single`, `parse_logged_in_multiple`,
/// `parse_logged_in_older_as_phrasing`, `parse_logged_in_empty`.
pub fn parse_logged_in_accounts(auth_status: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in auth_status.lines() {
        let Some((_, rest)) = line.split_once("Logged in to") else {
            continue;
        };
        if let Some(login) = extract_login_token(rest)
            && !out.iter().any(|e| e == &login)
        {
            out.push(login);
        }
    }
    out
}

/// Extract the `<login>` following the `account`/`as` keyword in a status line.
///
/// Why: isolates the token-scan so [`parse_logged_in_accounts`] stays readable
/// and the trimming rules are tested in one place.
/// What: given the remainder after "Logged in to" (e.g.
/// `" github.com account bobmatnyc (keyring)"`), returns the token after the
/// `account` (preferred) or `as` keyword, stripped of surrounding punctuation.
/// Test: covered via `parse_logged_in_*`.
fn extract_login_token(rest: &str) -> Option<String> {
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    for keyword in ["account", "as"] {
        if let Some(pos) = tokens.iter().position(|t| *t == keyword)
            && let Some(raw) = tokens.get(pos + 1)
        {
            let login = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
            if !login.is_empty() {
                return Some(login.to_string());
            }
        }
    }
    None
}

/// Cheap, subprocess-free local account status from `gh`'s `hosts.yml`.
///
/// Why: the statusline renders on Claude Code's hot path; a subprocess per
/// render is unacceptable. A single small-file read answers both "who is active"
/// and "are multiple accounts logged in" for the common case where `gh` has ever
/// been configured on this host.
/// What: reads the resolved `hosts.yml` and parses it via
/// [`parse_gh_account_status_from_hosts_yml`]; returns `None` when the file is
/// absent/unreadable or carries no github.com account.
/// Test: env/filesystem-dependent; the parse is covered by
/// `parse_gh_account_status_*`.
pub fn gh_account_status_local() -> Option<GhAccountStatus> {
    let path = gh_hosts_yml_path()?;
    let contents = std::fs::read_to_string(&path).ok()?;
    parse_gh_account_status_from_hosts_yml(&contents)
}

/// Run `f` on a detached thread and return its result within `timeout`.
///
/// Why: `gh` subprocesses (or even a slow network-filesystem read) must never
/// block the statusline or doctor; a bounded thread turns "hung" into "omitted".
/// The pattern mirrors the statusline's `git_branch` bounded-thread probe.
/// What: spawns `f` on a new thread, waits up to `timeout` on an mpsc channel,
/// and returns `None` on timeout (the thread is abandoned, not joined).
/// Test: side-effect timing; covered by the callers' fail-soft behaviour.
fn run_bounded<T, F>(timeout: Duration, f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> Option<T> + Send + 'static,
{
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(timeout).ok().flatten()
}

/// Resolve the active github.com login, preferring the cheap local path.
///
/// Why: the one call every surface uses to answer "which account is active?".
/// Reads `hosts.yml` first (no subprocess) and only falls back to a bounded
/// `gh auth status --active` when the local file cannot answer, so it is fast in
/// the common case yet still correct when config lives only in the keyring.
/// What: returns the active login from `hosts.yml`, else the first account
/// parsed from a bounded `gh auth status --active`; `None` when `gh` is
/// unconfigured/unauthenticated/absent.
/// Test: the local parse is covered by `parse_active_account_*`; the subprocess
/// fallback is thin bounded glue.
pub fn active_gh_account() -> Option<String> {
    if let Some(status) = gh_account_status_local()
        && let Some(active) = status.active
    {
        return Some(active);
    }
    run_bounded(GH_PROBE_TIMEOUT, || {
        let out = std::process::Command::new("gh")
            .args(["auth", "status", "--active"])
            .output()
            .ok()?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        parse_logged_in_accounts(&text).into_iter().next()
    })
}

/// List every logged-in github.com account via a bounded `gh auth status`.
///
/// Why: detecting the MULTIPLE-accounts ambiguity (the condition that caused the
/// admin-merge bug) is the authoritative signal `tm doctor` warns on. `gh auth
/// status` is the definitive source across all storage backends (keyring or
/// file), so the doctor uses it rather than trusting only `hosts.yml`.
/// What: runs `gh auth status` (bounded by [`GH_PROBE_TIMEOUT`]) and parses its
/// stdout+stderr via [`parse_logged_in_accounts`]; returns an empty vec on
/// timeout, a missing `gh`, or when not authenticated.
/// Test: the parse is covered by `parse_logged_in_*`; this is thin bounded glue.
pub fn logged_in_gh_accounts() -> Vec<String> {
    run_bounded(GH_PROBE_TIMEOUT, || {
        let out = std::process::Command::new("gh")
            .args(["auth", "status"])
            .output()
            .ok()?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Some(parse_logged_in_accounts(&text))
    })
    .unwrap_or_default()
}

/// Environment variable that scopes every `gh` invocation to a private config
/// home — the project-isolation primitive [`ensure_gh_account_for_project`]
/// requires rather than reinvents (already used by `gh_identity::GithubConfig`
/// and, on at least one operator's host, by a shell `chpwd` hook that sets it
/// per working directory).
pub const GH_CONFIG_DIR_ENV: &str = "GH_CONFIG_DIR";

/// Env vars that can override `gh`'s config-dir-resolved identity; must be
/// neutralised before enforcing/verifying the active account so a stray token
/// cannot silently outrank the isolated config dir.
const GH_TOKEN_ENV_VARS: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];

/// Bound for the `gh auth status` / `gh auth switch` calls
/// [`ensure_gh_account_for_project`] makes.
///
/// Why: unlike [`GH_PROBE_TIMEOUT`] (tuned for the statusline's hot render
/// path), this call is an explicit, occasional pre-flight — generous enough
/// for a keyring-backed `gh` to respond without hanging the caller indefinitely.
const GH_ENFORCE_TIMEOUT: Duration = Duration::from_secs(5);

/// Ensure `gh` is authenticated as `expected_user` for `config_dir`, WITHOUT
/// mutating any other gh config store — the #2081 per-project mechanism.
///
/// Why: `tm`'s daemon runs many projects concurrently; a bare `gh auth switch`
/// rewrites the single shared `~/.config/gh/hosts.yml` "active" pointer, so
/// switching for one project would silently redirect a concurrently-running
/// project's `gh` calls to the wrong identity. Scoping every read AND the
/// switch itself to an already-isolated `config_dir` (via `GH_CONFIG_DIR`)
/// confines the mutation to that one project's store — two projects with
/// distinct config dirs can run this concurrently without interfering. The
/// motivating incident (#2081) was NOT fixed by `GH_CONFIG_DIR` isolation
/// alone: the isolated store itself still had the wrong account marked
/// active, so this also corrects (and then re-verifies) the active-user
/// pointer INSIDE that store rather than assuming a config-dir switch is
/// sufficient.
/// What: runs `gh auth status` scoped to `config_dir` (with `GH_TOKEN` /
/// `GITHUB_TOKEN` stripped from the child env so a stray token cannot outrank
/// the config-dir identity); if `expected_user` is already active, returns
/// immediately with no mutation. Otherwise runs `gh auth switch --user
/// <expected_user>` under the same scoped, token-stripped env, then re-runs
/// `gh auth status` and fails loudly (never silently proceeds) unless the
/// re-check confirms `expected_user` is now active.
/// Test: `verify_active_account_*`, `parse_active_account_from_status_*`
/// cover the pure decision logic; `ensure_gh_account_for_project_*` cover the
/// public entry point's refusal path without a live `gh`.
fn ensure_gh_account_in_dir(
    expected_user: &str,
    config_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let dir = config_dir.to_string_lossy().to_string();
    let host = crate::core::trusty_tools_config::DEFAULT_GITHUB_HOST;

    let status_text = run_gh_scoped(&dir, ["auth", "status"])
        .context("failed to run `gh auth status` scoped to the project's GH_CONFIG_DIR")?;
    if verify_active_account(&status_text, expected_user).is_ok() {
        return Ok(());
    }

    let switch = std::process::Command::new("gh")
        .args([
            "auth",
            "switch",
            "--hostname",
            host,
            "--user",
            expected_user,
        ])
        .env(GH_CONFIG_DIR_ENV, &dir)
        .env_remove(GH_TOKEN_ENV_VARS[0])
        .env_remove(GH_TOKEN_ENV_VARS[1])
        .output()
        .with_context(|| format!("failed to spawn `gh auth switch --user {expected_user}`"))?;
    if !switch.status.success() {
        let stderr = String::from_utf8_lossy(&switch.stderr);
        anyhow::bail!(
            "`gh auth switch --user {expected_user}` failed inside {dir}: {} \
             (is '{expected_user}' logged in under this GH_CONFIG_DIR? run \
             `GH_CONFIG_DIR={dir} gh auth login` first)",
            stderr.trim()
        );
    }

    let recheck = run_gh_scoped(&dir, ["auth", "status"])
        .context("failed to re-verify `gh auth status` after switching")?;
    verify_active_account(&recheck, expected_user).map_err(|msg| {
        anyhow::anyhow!(
            "gh account switch to '{expected_user}' did not take effect inside {dir}: {msg}"
        )
    })
}

/// Run a bounded `gh` subprocess scoped to `config_dir`, with any env-token
/// override stripped, returning the combined stdout+stderr text.
///
/// Why: both the pre-check and the post-switch re-verification in
/// [`ensure_gh_account_in_dir`] need the identical scoped-subprocess
/// invocation; a shared helper keeps the env-stripping and timeout bound in
/// one place.
/// What: bounded by [`GH_ENFORCE_TIMEOUT`]; returns an error on a spawn
/// failure or timeout.
/// Test: exercised indirectly via `ensure_gh_account_for_project_*` (no live
/// `gh` in CI, so only the refusal path — which never reaches this
/// function — is asserted there).
fn run_gh_scoped(config_dir: &str, args: [&'static str; 2]) -> anyhow::Result<String> {
    let config_dir = config_dir.to_string();
    run_bounded(GH_ENFORCE_TIMEOUT, move || {
        let out = std::process::Command::new("gh")
            .args(args)
            .env(GH_CONFIG_DIR_ENV, &config_dir)
            .env_remove(GH_TOKEN_ENV_VARS[0])
            .env_remove(GH_TOKEN_ENV_VARS[1])
            .output()
            .ok()?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Some(text)
    })
    .ok_or_else(|| anyhow::anyhow!("`gh` did not respond within {GH_ENFORCE_TIMEOUT:?}"))
}

/// Parse the login marked `Active account: true` from a `gh auth status` transcript.
///
/// Why: [`verify_active_account`] needs the SPECIFIC active login (not just
/// "who is logged in", which [`parse_logged_in_accounts`] already answers) to
/// confirm a switch actually took effect.
/// What: scans line-by-line, tracking the login introduced by the most recent
/// "Logged in to ..." line, and returns it the first time a following
/// "Active account: true" line is seen. Returns `None` when no login is ever
/// marked active.
/// Test: `parse_active_account_from_status_multi`,
/// `parse_active_account_from_status_single`,
/// `parse_active_account_from_status_none_active`.
pub(crate) fn parse_active_account_from_status(auth_status: &str) -> Option<String> {
    let mut current: Option<String> = None;
    for line in auth_status.lines() {
        if let Some((_, rest)) = line.split_once("Logged in to") {
            current = extract_login_token(rest);
            continue;
        }
        if let Some((_, value)) = line.split_once("Active account:")
            && value.trim().eq_ignore_ascii_case("true")
        {
            return current.clone();
        }
    }
    None
}

/// Decide whether a `gh auth status` transcript confirms `expected` is active.
///
/// Why: isolates the verification decision — including the defensive check
/// for an env-token override that would make the config-dir identity
/// unverifiable — so it is unit-testable without a live `gh` and so
/// [`ensure_gh_account_in_dir`] never has to guess at the meaning of raw
/// status text.
/// What: `Err` when the transcript mentions an environment-variable token
/// override (defence in depth: `GH_TOKEN`/`GITHUB_TOKEN` should already be
/// stripped from the child env, but this catches the case where `gh` reports
/// one anyway); `Err` when no account or a DIFFERENT account is active;
/// `Ok(())` only when [`parse_active_account_from_status`] returns exactly
/// `expected`.
/// Test: `verify_active_account_matches`, `verify_active_account_mismatch`,
/// `verify_active_account_rejects_env_token_override`.
pub(crate) fn verify_active_account(status_text: &str, expected: &str) -> Result<(), String> {
    let lower = status_text.to_lowercase();
    if lower.contains("environment variable")
        && (lower.contains("gh_token") || lower.contains("github_token"))
    {
        return Err(format!(
            "gh is authenticating via a GH_TOKEN/GITHUB_TOKEN environment variable override, \
             so the active account inside this GH_CONFIG_DIR cannot be verified as '{expected}'"
        ));
    }
    match parse_active_account_from_status(status_text) {
        Some(active) if active == expected => Ok(()),
        Some(active) => Err(format!(
            "active account is '{active}', expected '{expected}'"
        )),
        None => Err(format!(
            "could not determine an active gh account (expected '{expected}')"
        )),
    }
}

/// Ensure `gh` operations for a project default to `gh_user` — the #2081
/// public entry point.
///
/// Why: `Project::gh_user` (#2081) declares a project's preferred `gh`
/// account, but honouring it safely requires an already-isolated
/// `GH_CONFIG_DIR` (see the module-level "Per-project account enforcement"
/// doc for why a global `gh auth switch` is refused). Requiring
/// [`GH_CONFIG_DIR_ENV`] to already be set — rather than inventing a
/// project-specific directory here — aligns with the isolation convention
/// this codebase (`gh_identity::GithubConfig::config_dir`) and at least one
/// operator's own shell tooling already use.
/// What: reads [`GH_CONFIG_DIR_ENV`] from the process environment; `None`/empty
/// → a loud, actionable error (never falls through to mutating the shared
/// default gh config). When present, delegates to
/// [`ensure_gh_account_in_dir`], which is itself a no-op when `gh_user` is
/// already the active account inside that directory.
/// Test: `ensure_gh_account_for_project_refuses_without_config_dir` proves the
/// refusal path never attempts any `gh` call (so it can never perform a
/// global switch) when no isolated config dir is available.
pub fn ensure_gh_account_for_project(gh_user: &str) -> anyhow::Result<()> {
    let gh_user = gh_user.trim();
    anyhow::ensure!(!gh_user.is_empty(), "gh_user must not be empty");

    let config_dir = std::env::var_os(GH_CONFIG_DIR_ENV)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no isolated {GH_CONFIG_DIR_ENV} is set in this environment; refusing to run \
                 `gh auth switch` against the shared ~/.config/gh store (that would leak \
                 identity across concurrently-running projects). Export {GH_CONFIG_DIR_ENV} \
                 to a per-project gh config directory (see `github.config_dir` in \
                 TrustyToolsConfig, or the equivalent per-directory shell convention) before \
                 retrying."
            )
        })?;

    ensure_gh_account_in_dir(gh_user, &config_dir)
}

// ── #3025: spawn-time GH_TOKEN minting for a pinned `gh_account` ───────────
//
// Distinct from the #2081 mechanism above: `ensure_gh_account_for_project`
// requires an already-isolated `GH_CONFIG_DIR` and only ever corrects the
// "active" pointer INSIDE it. The functions below need no such isolation —
// `gh auth token -u <account>` reads a specific already-logged-in account's
// credential directly, with no "active account" mutation at all, so it is
// safe to call unconditionally at every session spawn/relaunch and inject
// the result as `GH_TOKEN`/`GH_USER` into that ONE session's environment,
// leaving every other concurrently-running session's `gh` identity
// untouched. This also fulfils the `resolve_gh_account_env` reference in
// [`crate::core::trusty_tools_config::ProjectConfig::gh_user`]'s doc
// comment, left dangling since #2081 landed.

/// Env var this module injects for the resolved `GH_TOKEN` (#3025).
pub const GH_TOKEN_ENV_VAR: &str = "GH_TOKEN";

/// Env var this module injects to record which account minted the token
/// (#3025) — informational only; `gh` itself never reads it.
pub const GH_USER_ENV_VAR: &str = "GH_USER";

/// Mint a `GH_TOKEN` for `account` via `gh auth token -u <account>` — the
/// production resolver [`resolve_gh_account_env`] uses (#3025).
///
/// Why: `gh auth token -u <login>` reads an already-logged-in account's
/// credential straight from the keyring/`hosts.yml` with NO "active
/// account" side effect, unlike `gh auth switch` — safe to call from
/// concurrently-spawning sessions pinned to different accounts.
/// What: bounded by [`GH_ENFORCE_TIMEOUT`]; returns the trimmed stdout token
/// on a zero exit with non-empty output, else an `Err` describing why (`gh`
/// missing, account not logged in, empty output, or a timeout) — never
/// panics, never retries.
/// Test: exercised indirectly via [`resolve_gh_account_env_with`]'s
/// fake-resolver tests (no live `gh` needed); this thin subprocess wrapper
/// has no pure branch of its own left to unit test.
pub fn gh_token_via_cli(account: &str) -> Result<String, String> {
    let owned = account.to_string();
    run_bounded(GH_ENFORCE_TIMEOUT, move || {
        let out = std::process::Command::new("gh")
            .args(["auth", "token", "-u", &owned])
            .output()
            .ok()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Some(Err(format!("`gh auth token -u {owned}` failed: {stderr}")));
        }
        let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if token.is_empty() {
            return Some(Err(format!(
                "`gh auth token -u {owned}` returned an empty token"
            )));
        }
        Some(Ok(token))
    })
    .unwrap_or_else(|| {
        Err(format!(
            "`gh auth token -u {account}` did not respond within {GH_ENFORCE_TIMEOUT:?} \
             (is `gh` installed and on PATH?)"
        ))
    })
}

/// Resolve `GH_TOKEN`/`GH_USER` spawn-env overrides for a pinned
/// `gh_account`, given an injectable token resolver (#3025) — the pure,
/// hermetically testable core every call site shares.
///
/// Why: separating "which account, if any" from "how a token is minted for
/// it" lets tests exercise every outcome (unset, success, failure) with a
/// fake `resolve_token` closure, matching this codebase's established
/// trait-seam convention (`GitBackend`, `ManagedTmuxDriver`) for I/O that
/// cannot run hermetically in CI.
/// What: `gh_account` unset or blank → `None` (nothing to inject, no
/// regression). `Some(account)` → `Some(Ok(vars))` with `GH_TOKEN` then
/// `GH_USER` (in that order) when `resolve_token(account)` succeeds;
/// `Some(Err(msg))` when it fails — the caller logs `msg` as a warning and
/// proceeds WITHOUT injecting anything (issue #3025's documented failure
/// mode: spawn must never be blocked by a resolution failure).
/// Test: `resolve_gh_account_env_with_unset_is_none`,
/// `resolve_gh_account_env_with_blank_is_none`,
/// `resolve_gh_account_env_with_success_returns_token_and_user`,
/// `resolve_gh_account_env_with_failure_returns_err`.
pub fn resolve_gh_account_env_with(
    gh_account: Option<&str>,
    resolve_token: impl FnOnce(&str) -> Result<String, String>,
) -> Option<Result<Vec<(String, String)>, String>> {
    let account = gh_account.map(str::trim).filter(|s| !s.is_empty())?;
    Some(resolve_token(account).map(|token| {
        vec![
            (GH_TOKEN_ENV_VAR.to_string(), token),
            (GH_USER_ENV_VAR.to_string(), account.to_string()),
        ]
    }))
}

/// Production entry point: resolve `GH_TOKEN`/`GH_USER` for `gh_account` via
/// the real `gh auth token -u <account>` CLI (#3025).
///
/// Why: the one call every spawn/relaunch site uses, so none of them has to
/// name [`gh_token_via_cli`] as a resolver argument itself.
/// What: delegates to [`resolve_gh_account_env_with`] with [`gh_token_via_cli`].
/// Test: covered via `resolve_gh_account_env_with`'s fake-resolver tests.
pub fn resolve_gh_account_env(
    gh_account: Option<&str>,
) -> Option<Result<Vec<(String, String)>, String>> {
    resolve_gh_account_env_with(gh_account, gh_token_via_cli)
}

/// Resolve `GH_TOKEN`/`GH_USER` for the project owning workspace `cwd`
/// (#3025) — the one call `ClaudeCodeAdapter::spawn`/`spawn_resume` uses at
/// every session spawn/relaunch.
///
/// Why: the runtime adapter only has a `cwd`, not a `Project`; detecting the
/// owning project from `cwd`'s git origin mirrors the exact convention
/// `bin/tm/gh_identity.rs::load_gh_env` already uses for CLI `gh` calls, so
/// this and the CLI never disagree about which project a workspace belongs
/// to.
/// What: reads `cwd`'s `remote.origin.url`, matches it against the loaded
/// [`crate::core::trusty_tools_config::TrustyToolsConfig`]'s `projects:` list,
/// and resolves any pinned `gh_account` via [`resolve_gh_account_env`].
/// Fail-open throughout: no git origin, no project match, no `gh_account`
/// pinned, or a resolution failure all yield an EMPTY vec — never blocks the
/// spawn. A resolution failure is logged as a `tracing::warn!` here so every
/// call site gets the warning for free.
/// Test: `resolve_gh_account_env_for_workspace_no_origin_is_empty`
/// (`gh_account_spawn_env_tests.rs`); the match/resolve logic itself is
/// covered by `resolve_gh_account_env_with`'s fake-resolver tests.
pub fn resolve_gh_account_env_for_workspace(cwd: &std::path::Path) -> Vec<(String, String)> {
    let origin = crate::daemon::managed_routes::inproject::get_origin_url(cwd);
    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    let gh_account = origin
        .as_deref()
        .and_then(|url| {
            config
                .projects
                .iter()
                .find(|p| crate::project::record::repo_url_matches(&p.repo_url, url))
        })
        .and_then(|p| p.gh_account.as_deref());
    match resolve_gh_account_env(gh_account) {
        None => Vec::new(),
        Some(Ok(vars)) => vars,
        Some(Err(msg)) => {
            tracing::warn!(
                cwd = %cwd.display(),
                "gh_account token resolution failed; spawning without GH_TOKEN: {msg}"
            );
            Vec::new()
        }
    }
}

#[cfg(test)]
#[path = "gh_account_spawn_env_tests.rs"]
mod spawn_env_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// A `hosts.yml` with two accounts logged in and `bob-duetto` active — the
    /// exact shape that produced the admin-merge bug on the dev host.
    const MULTI_HOSTS_YML: &str = "\
github.com:
    git_protocol: https
    users:
        bobmatnyc:
        bob-duetto:
    user: bob-duetto
";

    /// A single-account `hosts.yml` (only the active user, no `users` map).
    const SINGLE_HOSTS_YML: &str = "\
github.com:
    git_protocol: https
    user: bobmatnyc
";

    /// Real `gh auth status` output with two github.com accounts logged in.
    const MULTI_AUTH_STATUS: &str = "\
github.com
  ✓ Logged in to github.com account bob-duetto (keyring)
  - Active account: true
  - Git operations protocol: https
  - Token: gho_************************************
  - Token scopes: 'admin:org', 'gist', 'project', 'repo', 'workflow'

  ✓ Logged in to github.com account bobmatnyc (keyring)
  - Active account: false
  - Git operations protocol: https
  - Token: gho_************************************
  - Token scopes: 'gist', 'read:org', 'repo', 'workflow'
";

    /// Single-account `gh auth status` output.
    const SINGLE_AUTH_STATUS: &str = "\
github.com
  ✓ Logged in to github.com account bobmatnyc (keyring)
  - Active account: true
  - Git operations protocol: https
  - Token: gho_************************************
";

    /// Why: the active login must be read from `github.com.user`.
    /// Test: itself.
    #[test]
    fn parse_active_account_present() {
        assert_eq!(
            parse_active_account_from_hosts_yml(MULTI_HOSTS_YML).as_deref(),
            Some("bob-duetto")
        );
        assert_eq!(
            parse_active_account_from_hosts_yml(SINGLE_HOSTS_YML).as_deref(),
            Some("bobmatnyc")
        );
    }

    /// Why: absent/blank config must yield `None`, never panic.
    /// Test: itself.
    #[test]
    fn parse_active_account_absent() {
        assert_eq!(parse_active_account_from_hosts_yml(""), None);
        assert_eq!(
            parse_active_account_from_hosts_yml("other.host:\n  user: x"),
            None
        );
        assert_eq!(
            parse_active_account_from_hosts_yml("github.com:\n  git_protocol: https"),
            None
        );
        // Not YAML at all → None, not a panic.
        assert_eq!(parse_active_account_from_hosts_yml(": : ["), None);
    }

    /// Why: a single-account host is unambiguous — active set, one logged-in
    /// account, `is_ambiguous()` false.
    /// Test: itself.
    #[test]
    fn parse_gh_account_status_single() {
        let status = parse_gh_account_status_from_hosts_yml(SINGLE_HOSTS_YML).expect("some");
        assert_eq!(status.active.as_deref(), Some("bobmatnyc"));
        assert_eq!(status.logged_in, vec!["bobmatnyc".to_string()]);
        assert!(!status.is_ambiguous());
    }

    /// Why: multiple accounts under `users:` must populate `logged_in` and flag
    /// `is_ambiguous()` — the signal that hid the admin-merge bug.
    /// Test: itself.
    #[test]
    fn parse_gh_account_status_multi_from_users_map() {
        let status = parse_gh_account_status_from_hosts_yml(MULTI_HOSTS_YML).expect("some");
        assert_eq!(status.active.as_deref(), Some("bob-duetto"));
        assert!(status.logged_in.contains(&"bobmatnyc".to_string()));
        assert!(status.logged_in.contains(&"bob-duetto".to_string()));
        assert_eq!(status.logged_in.len(), 2);
        assert!(status.is_ambiguous());
    }

    /// Why: a doc with no github.com host must yield `None`.
    /// Test: itself.
    #[test]
    fn parse_gh_account_status_empty() {
        assert_eq!(parse_gh_account_status_from_hosts_yml(""), None);
        assert_eq!(
            parse_gh_account_status_from_hosts_yml("enterprise.example.com:\n  user: x"),
            None
        );
    }

    /// Why: a single logged-in account parses to exactly one login.
    /// Test: itself.
    #[test]
    fn parse_logged_in_single() {
        assert_eq!(
            parse_logged_in_accounts(SINGLE_AUTH_STATUS),
            vec!["bobmatnyc".to_string()]
        );
    }

    /// Why: multiple logged-in accounts must all be parsed, in first-seen order.
    /// Test: itself.
    #[test]
    fn parse_logged_in_multiple() {
        assert_eq!(
            parse_logged_in_accounts(MULTI_AUTH_STATUS),
            vec!["bob-duetto".to_string(), "bobmatnyc".to_string()]
        );
    }

    /// Why: older `gh` phrased it "Logged in to github.com as <login>"; the
    /// parser must still find the login.
    /// Test: itself.
    #[test]
    fn parse_logged_in_older_as_phrasing() {
        let text = "✓ Logged in to github.com as octocat (oauth_token)";
        assert_eq!(parse_logged_in_accounts(text), vec!["octocat".to_string()]);
    }

    /// Why: "not authenticated" output must yield an empty list, never panic.
    /// Test: itself.
    #[test]
    fn parse_logged_in_empty() {
        assert!(parse_logged_in_accounts("").is_empty());
        assert!(parse_logged_in_accounts("You are not logged into any GitHub hosts.").is_empty());
    }

    /// Why: `is_ambiguous` is the load-bearing predicate for the warnings; assert
    /// its boundary directly.
    /// Test: itself.
    #[test]
    fn is_ambiguous_boundary() {
        let none = GhAccountStatus::default();
        assert!(!none.is_ambiguous());
        let one = GhAccountStatus {
            active: Some("a".into()),
            logged_in: vec!["a".into()],
        };
        assert!(!one.is_ambiguous());
        let two = GhAccountStatus {
            active: Some("a".into()),
            logged_in: vec!["a".into(), "b".into()],
        };
        assert!(two.is_ambiguous());
    }

    // ── #2081: per-project account enforcement (pure decision logic) ────────

    /// Why: the active-account parser must pick the login marked
    /// `Active account: true`, not just the first login seen.
    /// Test: itself.
    #[test]
    fn parse_active_account_from_status_multi() {
        assert_eq!(
            parse_active_account_from_status(MULTI_AUTH_STATUS).as_deref(),
            Some("bob-duetto")
        );
    }

    /// Why: a single-account transcript must resolve to that one login.
    /// Test: itself.
    #[test]
    fn parse_active_account_from_status_single() {
        assert_eq!(
            parse_active_account_from_status(SINGLE_AUTH_STATUS).as_deref(),
            Some("bobmatnyc")
        );
    }

    /// Why: a transcript with no `Active account: true` line (or no logins at
    /// all) must yield `None`, never panic.
    /// Test: itself.
    #[test]
    fn parse_active_account_from_status_none_active() {
        assert_eq!(parse_active_account_from_status(""), None);
        assert_eq!(
            parse_active_account_from_status(
                "github.com\n  ✓ Logged in to github.com account x\n  - Active account: false\n"
            ),
            None
        );
    }

    /// Why: [`verify_active_account`] must succeed only when the expected
    /// login is the one marked active.
    /// Test: itself.
    #[test]
    fn verify_active_account_matches() {
        assert!(verify_active_account(SINGLE_AUTH_STATUS, "bobmatnyc").is_ok());
    }

    /// Why: a mismatched active account (the exact #2081 incident shape —
    /// `bob-duetto` active when `bobmatnyc` was expected) must be a loud `Err`,
    /// never a silent pass.
    /// Test: itself.
    #[test]
    fn verify_active_account_mismatch() {
        let err = verify_active_account(MULTI_AUTH_STATUS, "bobmatnyc").unwrap_err();
        assert!(err.contains("bob-duetto"), "err: {err}");
        assert!(err.contains("bobmatnyc"), "err: {err}");
    }

    /// Why: if `gh` reports it is authenticating via an environment-variable
    /// token override, the config-dir identity cannot be trusted even if some
    /// login happens to also be marked active — this must be caught and
    /// refused explicitly (defence in depth alongside stripping `GH_TOKEN` /
    /// `GITHUB_TOKEN` from the child env before the call).
    /// Test: itself.
    #[test]
    fn verify_active_account_rejects_env_token_override() {
        let text = "The value of the GH_TOKEN environment variable is being used for authentication.\ngithub.com\n  ✓ Logged in to github.com account bobmatnyc\n  - Active account: true\n";
        let err = verify_active_account(text, "bobmatnyc").unwrap_err();
        assert!(err.contains("environment variable"), "err: {err}");
    }

    /// Why (#2081): the public entry point must REFUSE (never fall through to
    /// mutating the shared default `~/.config/gh` store) when no isolated
    /// `GH_CONFIG_DIR` is present in the environment. Because this returns
    /// before spawning any `gh` subprocess, it structurally proves the
    /// refusal path can never perform a global `gh auth switch`.
    /// Test: itself.
    #[test]
    fn ensure_gh_account_for_project_refuses_without_config_dir() {
        let _g = crate::core::trusty_tools_config::env_test_lock();
        // SAFETY: guarded by the crate-wide env test lock; restored below.
        let prior = std::env::var_os(GH_CONFIG_DIR_ENV);
        unsafe { std::env::remove_var(GH_CONFIG_DIR_ENV) };
        let err = ensure_gh_account_for_project("bobmatnyc").unwrap_err();
        // SAFETY: restoring whatever was present before the test ran.
        unsafe {
            match prior {
                Some(v) => std::env::set_var(GH_CONFIG_DIR_ENV, v),
                None => std::env::remove_var(GH_CONFIG_DIR_ENV),
            }
        }
        let msg = err.to_string();
        assert!(msg.contains(GH_CONFIG_DIR_ENV), "msg: {msg}");
        assert!(
            msg.contains("shared"),
            "expected the shared-store refusal rationale: {msg}"
        );
    }

    /// Why (#2081): an empty `gh_user` is a caller bug, not a valid
    /// "no preference" signal (that is `Option::None` at the `Project` layer)
    /// — must be rejected before even checking `GH_CONFIG_DIR`.
    /// Test: itself.
    #[test]
    fn ensure_gh_account_for_project_rejects_empty_login() {
        let err = ensure_gh_account_for_project("   ").unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }
}
