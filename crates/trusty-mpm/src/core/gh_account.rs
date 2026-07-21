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
//! Per-project account enforcement (#2081) — defaulting `gh` operations for a
//! project to its configured `gh_user` — lives in the sibling [`enforce`]
//! module (split out by #3070 to keep both files under the 500-SLOC cap);
//! [`ensure_gh_account_for_project`] and [`GH_CONFIG_DIR_ENV`] are re-exported
//! here so the public path is unchanged. [`ensure_gh_account_in_dir`] (the
//! explicit-directory core) and [`configured_account_pair`] (the shared
//! "does enforcement apply here" decision) are also re-exported — #3312 wires
//! them into the CLI's `gh_identity::resolve_project_aware` and the daemon's
//! `git_identity::resolve_for_config_enforced`, the two real production call
//! sites this enforcement previously never reached.

use std::path::PathBuf;
use std::time::Duration;

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

/// Bound for the `gh auth status` / `gh auth switch` calls the [`enforce`]
/// module's `ensure_gh_account_for_project` makes, and that
/// [`gh_token_via_cli`] below also uses for its `gh auth token` call.
///
/// Why: unlike [`GH_PROBE_TIMEOUT`] (tuned for the statusline's hot render
/// path), these calls are explicit, occasional pre-flights — generous enough
/// for a keyring-backed `gh` to respond without hanging the caller indefinitely.
const GH_ENFORCE_TIMEOUT: Duration = Duration::from_secs(5);

#[path = "gh_account_enforce.rs"]
mod enforce;
pub use enforce::{
    GH_CONFIG_DIR_ENV, configured_account_pair, ensure_gh_account_for_project,
    ensure_gh_account_in_dir,
};

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

/// Resolve `GH_TOKEN`/`GH_USER` for the project owning workspace `cwd`,
/// consulting the [`crate::project::ProjectRegistry`] — the ACTUAL
/// persistence target for a pinned `gh_account` (#3025 review follow-up:
/// `tm projects register --gh-account`, the PATCH route, and the
/// `project_register` MCP tool all write into the registry; an earlier
/// revision matched against [`crate::core::trusty_tools_config::TrustyToolsConfig`]'s
/// static `projects:` list instead, which none of those operator-facing
/// paths ever touches, so spawn-time injection silently no-op'd). A
/// config-declared `gh_account` is still covered: the daemon's
/// `seed_from_config` mirrors `TrustyToolsConfig.projects` into the registry
/// at startup, so it becomes visible here too, one hop later.
///
/// Why: the runtime adapter only has a `cwd`, not a `Project`; detecting the
/// owning project from `cwd`'s git origin mirrors the exact convention
/// `bin/tm/gh_identity.rs::load_gh_env` already uses for CLI `gh` calls.
/// What: runs the blocking `git config --get remote.origin.url` probe on the
/// blocking pool, looks the result up against every registered project's
/// `repo_url` (async — [`crate::project::ProjectRegistry::list`]), and — when
/// a match pins a `gh_account` — mints its token via a SECOND blocking-pool
/// hop (`gh auth token -u <account>` can block up to [`GH_ENFORCE_TIMEOUT`]).
/// Both blocking hops run via `tokio::task::spawn_blocking` so this can be
/// awaited directly from an async spawn/resume handler without stalling the
/// executor (review follow-up; mirrors `daemon::managed_routes::summary::
/// probe_stale_assets`'s `spawn_blocking` shape). Fail-open throughout: no
/// git origin, no project match, no `gh_account` pinned, a panicked blocking
/// task, or a resolution failure all yield an EMPTY vec — never blocks or
/// fails the spawn. A resolution failure is logged as a `tracing::warn!`
/// here so every call site gets the warning for free.
/// Test: `resolve_gh_account_env_for_registry_no_origin_is_empty`
/// (`gh_account_spawn_env_tests.rs`); the registry-matching step is
/// separately, directly tested via [`find_pinned_gh_account`] below.
pub async fn resolve_gh_account_env_for_registry(
    registry: &crate::project::ProjectRegistry,
    cwd: &std::path::Path,
) -> Vec<(String, String)> {
    let cwd_for_origin = cwd.to_path_buf();
    let origin = tokio::task::spawn_blocking(move || {
        crate::daemon::managed_routes::inproject::get_origin_url(&cwd_for_origin)
    })
    .await
    .unwrap_or(None);
    let Some(origin) = origin else {
        return Vec::new();
    };

    let Some(gh_account) = find_pinned_gh_account(registry, &origin).await else {
        return Vec::new();
    };

    let cwd_for_log = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || match resolve_gh_account_env(Some(&gh_account)) {
        None => Vec::new(),
        Some(Ok(vars)) => vars,
        Some(Err(msg)) => {
            tracing::warn!(
                cwd = %cwd_for_log.display(),
                "gh_account token resolution failed; spawning without GH_TOKEN: {msg}"
            );
            Vec::new()
        }
    })
    .await
    .unwrap_or_default()
}

/// Look up the pinned `gh_account` for the first registered project whose
/// `repo_url` matches `origin` — the pure(ish), registry-backed matching
/// step [`resolve_gh_account_env_for_registry`] delegates to, isolated so it
/// is directly testable against a real (temp-dir-backed) `ProjectRegistry`
/// fixture without needing a real git repository or a live `gh` subprocess
/// (#3025 review follow-up item 1: this is the exact step that proves the
/// registry — not the static config — is consulted).
/// Test: `resolve_gh_account_env_for_registry_picks_up_registered_gh_account`,
/// `resolve_gh_account_env_for_registry_no_match_is_none`,
/// `resolve_gh_account_env_for_registry_registered_without_gh_account_is_none`.
async fn find_pinned_gh_account(
    registry: &crate::project::ProjectRegistry,
    origin: &str,
) -> Option<String> {
    let projects = registry.list().await.ok()?;
    projects
        .iter()
        .find(|p| crate::project::record::repo_url_matches(&p.repo_url, origin))
        .and_then(|p| p.gh_account.clone())
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
}
