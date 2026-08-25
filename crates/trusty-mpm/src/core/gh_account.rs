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
//! `tm doctor` instead runs one bounded `gh auth status` via [`probe_gh_auth`]:
//! that is the only source that sees env-token auth (`GH_TOKEN` /
//! `GITHUB_TOKEN`), which writes no config file at all (#5032). Every entry
//! point is fail-soft: a missing `gh`, absent config, or parse error yields
//! `None` / [`GhAuthProbe::Inconclusive`], never an error or panic.
//!
//! Test: the pure parsers (`parse_active_account_from_hosts_yml`,
//! `parse_logged_in_accounts`, `parse_gh_account_status_from_hosts_yml`,
//! `parse_gh_account_status_from_auth_status`) are
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

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::trusty_tools_config::GithubConfig;

/// Bound for the `gh auth status` probe `tm doctor` runs (#5032).
///
/// Why: the bound this replaced was 250 ms, inherited from the statusline's hot
/// render path. `gh auth status` validates an env token (`GH_TOKEN` /
/// `GITHUB_TOKEN`) over the network, which measured 281–389 ms across eight runs
/// on the reporter's machine and 366–373 ms here — so every probe timed out and
/// the doctor reported a fully authenticated `gh` as unauthenticated. The
/// statusline never used this path (it reads `hosts.yml` directly, see
/// [`gh_account_status_local`]), so nothing latency-sensitive is left to protect.
/// What: 5 s, matching [`GH_ENFORCE_TIMEOUT`] — an order of magnitude above the
/// measured token round trip, with headroom for a slow network, while still
/// bounding a wedged `gh`. A probe that exceeds it reports UNKNOWN, never
/// "unauthenticated" (see [`GhAuthProbe`]).
/// Test: `doctor_probe_tolerates_token_auth_latency`,
/// `probe_beyond_bound_is_inconclusive`.
pub const GH_DOCTOR_TIMEOUT: Duration = Duration::from_secs(5);

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

    /// `gh`'s own spelling of `login` when it is one of the logged-in accounts.
    ///
    /// Why (#2121): a caller that wants to STORE a login must first prove `gh`
    /// knows it, and must store the spelling `gh` reports. GitHub logins are
    /// case-insensitive, but [`enforce::ensure_gh_account_in_dir`]'s
    /// `verify_active_account` compares them byte-for-byte — so a `gh_user`
    /// persisted as `Acme-Bot` against a `gh` that reports `acme-bot` would
    /// pass a naive membership test here and then fail that later exact match.
    /// Returning the canonical form makes the two agree by construction. Since
    /// #5849 that later match is against `gh api user --jq .login`, GitHub's
    /// own spelling, so the canonical form is what it will be compared to.
    /// What: case-insensitive lookup over `logged_in`, returning `gh`'s
    /// spelling. `active` is deliberately not consulted — a project may name
    /// any logged-in account, not only the currently active one.
    /// Test: `canonical_logged_in_login_is_case_insensitive`,
    /// `canonical_logged_in_login_rejects_unknown`.
    pub fn canonical_logged_in_login(&self, login: &str) -> Option<String> {
        let needle = login.trim();
        self.logged_in
            .iter()
            .find(|known| known.eq_ignore_ascii_case(needle))
            .cloned()
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
/// `pub(crate)` (rather than private) so other bounded-`gh`-subprocess callers
/// in the crate — e.g. `session_launch::workstream_label`'s launch-time label
/// ensure — reuse this one implementation instead of re-deriving the same
/// thread+mpsc-timeout pattern.
/// What: spawns `f` on a new thread, waits up to `timeout` on an mpsc channel,
/// and returns `None` on timeout (the thread is abandoned, not joined).
/// Test: side-effect timing; covered by the callers' fail-soft behaviour.
pub(crate) fn run_bounded<T, F>(timeout: Duration, f: F) -> Option<T>
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

/// Parse both the active login and every logged-in login from `gh auth status`.
///
/// Why: `gh auth status` is the only source that sees ALL auth modes — keyring,
/// `hosts.yml`, and an env token (`GH_TOKEN` / `GITHUB_TOKEN`), which writes no
/// config file at all (#5032). Reading active + logged-in from ONE invocation
/// also means one network round trip instead of two.
/// What: `logged_in` comes from [`parse_logged_in_accounts`]; `active` is the
/// login of the block whose `- Active account: true` line follows it. A single
/// logged-in account with no such marker (older `gh`) is taken as active.
/// Test: `parse_auth_status_active_from_marker`,
/// `parse_auth_status_token_auth`, `parse_auth_status_unauthenticated`.
pub fn parse_gh_account_status_from_auth_status(auth_status: &str) -> GhAccountStatus {
    let logged_in = parse_logged_in_accounts(auth_status);
    let mut active: Option<String> = None;
    let mut current: Option<String> = None;
    for line in auth_status.lines() {
        if let Some((_, rest)) = line.split_once("Logged in to") {
            current = extract_login_token(rest);
        } else if active.is_none() && line.contains("Active account: true") {
            active = current.clone();
        }
    }
    if active.is_none() && logged_in.len() == 1 {
        active = logged_in.first().cloned();
    }
    GhAccountStatus { active, logged_in }
}

/// Outcome of a bounded `gh auth status` probe (#5032).
///
/// Why: "`gh` says no account" and "I could not tell within the bound" are
/// different facts, and collapsing them is the defect this type exists to
/// prevent — the doctor reported a working `GH_TOKEN` login as "not
/// authenticated" because a timed-out probe was indistinguishable from an empty
/// answer.
/// What: `Answered` carries what `gh` reported (an empty `logged_in` IS a
/// definitive "not authenticated"); `Inconclusive` carries why the state is
/// unknown — the bound elapsed, or `gh` could not be run.
/// Test: `probe_answered_parses_accounts`, `probe_beyond_bound_is_inconclusive`,
/// `probe_missing_gh_is_inconclusive`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhAuthProbe {
    /// `gh` answered within the bound; an empty `logged_in` means unauthenticated.
    Answered(GhAccountStatus),
    /// Auth state is UNKNOWN, for the carried reason — not a negative answer.
    Inconclusive(String),
}

/// Probe `gh`'s github.com auth state with an injectable runner (#5032).
///
/// Why: the seam lets the timeout arm — the one that produced the false
/// "not authenticated" — be tested hermetically, with no live `gh` and no
/// network.
/// What: runs `run` on a bounded thread; `Some(text)` is parsed via
/// [`parse_gh_account_status_from_auth_status`], `None` means `gh` could not be
/// executed, and an elapsed bound yields [`GhAuthProbe::Inconclusive`].
/// Test: `doctor_probe_tolerates_token_auth_latency`,
/// `probe_beyond_bound_is_inconclusive`, `probe_missing_gh_is_inconclusive`.
pub fn probe_gh_auth_with<F>(timeout: Duration, run: F) -> GhAuthProbe
where
    F: FnOnce() -> Option<String> + Send + 'static,
{
    run_bounded(timeout, move || {
        Some(match run() {
            Some(text) => GhAuthProbe::Answered(parse_gh_account_status_from_auth_status(&text)),
            None => GhAuthProbe::Inconclusive("`gh` could not be run (is it on PATH?)".to_string()),
        })
    })
    .unwrap_or_else(|| {
        GhAuthProbe::Inconclusive(format!(
            "`gh auth status` did not answer within {timeout:?}"
        ))
    })
}

/// Probe `gh`'s github.com auth state by running the real `gh auth status`.
///
/// Why: the single entry point every caller that needs an authoritative,
/// all-auth-modes answer uses — `hosts.yml` alone cannot see env-token auth.
/// What: delegates to [`probe_gh_auth_with`] with a runner that returns
/// `gh auth status`'s stdout+stderr, or `None` when the spawn itself fails.
/// Test: covered via [`probe_gh_auth_with`]'s fake-runner tests.
pub fn probe_gh_auth(timeout: Duration) -> GhAuthProbe {
    probe_gh_auth_with(timeout, || {
        // #5475: single `gh` entry point.
        let out = trusty_common::gh::GhCommand::new(["auth", "status"])
            .output_blocking()
            .ok()?;
        Some(out.combined())
    })
}

/// Bound for the `gh auth status` / `gh auth switch` calls the [`enforce`]
/// module's `ensure_gh_account_for_project` makes, and that
/// [`gh_token_via_cli`] below also uses for its `gh auth token` call.
///
/// Why: like [`GH_DOCTOR_TIMEOUT`], these calls are explicit, occasional
/// pre-flights — generous enough for a keyring-backed or network-validated `gh`
/// to respond without hanging the caller indefinitely.
const GH_ENFORCE_TIMEOUT: Duration = Duration::from_secs(5);

#[path = "gh_account_enforce.rs"]
mod enforce;
pub use enforce::{
    GH_CONFIG_DIR_ENV, configured_account_pair, ensure_gh_account_for_project,
    ensure_gh_account_in_dir,
};

// ── Spawn-time gh identity selection for a project (#3025, #5851) ──────────
//
// Distinct from the #2081 mechanism above: `ensure_gh_account_for_project`
// requires an already-isolated `GH_CONFIG_DIR` and only ever corrects the
// "active" pointer INSIDE it. The functions below run at every session
// spawn/relaunch and inject overrides into that ONE session's environment,
// leaving every other concurrently-running session's `gh` identity
// untouched. This also fulfils the `resolve_gh_account_env` reference in
// [`crate::core::trusty_tools_config::ProjectConfig::gh_user`]'s doc
// comment, left dangling since #2081 landed.
//
// #5851 changed WHICH mechanism selects the account. `gh auth token -u
// <account>` was the original selector, but on a keyring-backed host that
// flag does not discriminate: `-u bobmatnyc` and `-u bob-duetto` return the
// identical value, so the credential a session got followed the global
// active account rather than the project's. A project that configures
// `github.config_dir` is now pinned with `GH_CONFIG_DIR` instead, which does
// discriminate; `gh_token_via_cli` remains only for a project that has no
// `config_dir` to pin to.

/// Env var this module injects for the resolved `GH_TOKEN` (#3025).
pub const GH_TOKEN_ENV_VAR: &str = "GH_TOKEN";

/// Env var this module injects to record which account minted the token
/// (#3025) — informational only; `gh` itself never reads it.
pub const GH_USER_ENV_VAR: &str = "GH_USER";

/// Mint a `GH_TOKEN` for `account` via `gh auth token -u <account>` — the
/// FALLBACK selector, used only for a project with no `github.config_dir`
/// (#3025, demoted by #5851).
///
/// Why: `gh auth token -u <login>` has no "active account" side effect,
/// unlike `gh auth switch`, so it is safe to call from concurrently-spawning
/// sessions. It is nonetheless a WEAK selector: measured on gh 2.89.0 against
/// a keyring-backed credential store, `-u bobmatnyc` and `-u bob-duetto`
/// return the identical value, so the token follows the global active
/// account rather than the one named (#5851). A project that pins
/// `github.config_dir` never reaches this function; one that does not still
/// gets the pre-#5851 behaviour rather than nothing.
/// What: bounded by [`GH_ENFORCE_TIMEOUT`]; returns the trimmed stdout token
/// on a zero exit with non-empty output, else an `Err` describing why (`gh`
/// missing, account not logged in, empty output, or a timeout) — never
/// panics, never retries.
/// Test: exercised indirectly via [`resolve_gh_account_env_with`]'s
/// fake-resolver tests (no live `gh` needed); this thin subprocess wrapper
/// has no pure branch of its own left to unit test.
pub fn gh_token_via_cli(account: &str) -> Result<String, String> {
    // #5851: `-u` does not discriminate on a keyring-backed machine — this is
    // the fallback for a project with no `config_dir`, not the selector.
    let owned = account.to_string();
    run_bounded(GH_ENFORCE_TIMEOUT, move || {
        // #5475: the non-zero and empty-output arms are the entry point's
        // `nonempty_stdout_blocking`; the failure message stays local because
        // it names the pinned account.
        Some(
            trusty_common::gh::GhCommand::new(["auth", "token", "-u", &owned])
                .nonempty_stdout_blocking()
                .map_err(|e| format!("`gh auth token -u {owned}` failed: {e}")),
        )
    })
    .unwrap_or_else(|| {
        Err(format!(
            "`gh auth token -u {account}` did not respond within {GH_ENFORCE_TIMEOUT:?} \
             (is `gh` installed and on PATH?)"
        ))
    })
}

/// The spawn-env overrides resolved for one project, plus any diagnostic the
/// caller must surface (#5851).
///
/// Why: a project can pin a `GH_CONFIG_DIR` that holds no credential. That is
/// a real misconfiguration, but suppressing the override because of it would
/// hand the session back to the machine-global `gh` account — the exact
/// wrong-identity outcome #5851 fixes. Carrying the vars and the diagnostic in
/// one value lets the caller warn loudly while the session still fails CLOSED
/// (`gh` exits 4 inside an empty config dir) rather than open.
/// What: `vars` is the ordered `(name, value)` list to inject; `warning` is
/// `Some(msg)` only when the resolved selection is usable but suspect. An
/// `Err` from [`resolve_gh_account_env_with`] is the separate "nothing could
/// be resolved" outcome — it carries no vars at all.
/// Test: `config_dir_without_credential_still_pins_and_warns`,
/// `config_dir_with_credential_has_no_warning`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GhSpawnEnv {
    /// Ordered `(name, value)` overrides to inject into the spawned session.
    pub vars: Vec<(String, String)>,
    /// A non-fatal diagnostic the caller logs at warn level; `vars` still applies.
    pub warning: Option<String>,
}

/// Whether `dir` carries a github.com credential of its own (#5851).
///
/// Why: `GH_CONFIG_DIR` pointed at a directory with no `hosts.yml` makes every
/// authenticated `gh` call exit 4 ("To get started with GitHub CLI, please run:
/// gh auth login"), measured against an empty scratch dir on gh 2.89.0. Detecting
/// that at spawn time turns a mid-session auth failure into one actionable
/// warning naming the directory.
/// What: reads `<dir>/hosts.yml` and reuses
/// [`parse_gh_account_status_from_hosts_yml`]; false for a missing/unreadable/
/// unparseable file or one naming no github.com account. This proves the dir
/// NAMES an account, not that the credential resolves to it — that stronger
/// assertion is #5849's.
/// Test: `config_dir_without_credential_still_pins_and_warns`,
/// `config_dir_with_credential_has_no_warning`.
fn config_dir_has_credential(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("hosts.yml"))
        .ok()
        .and_then(|text| parse_gh_account_status_from_hosts_yml(&text))
        .is_some_and(|status| !status.logged_in.is_empty())
}

/// Build the scoped-`GH_CONFIG_DIR` overrides for a project pinned to `dir`.
///
/// Why: `GH_CONFIG_DIR` is the one selector that actually discriminates
/// between logged-in accounts (#5851), and the `config_dir > token_env >
/// account` precedence that turns a binding into env vars is already written
/// once in [`crate::core::gh_identity::resolve_gh_env`] — this routes through
/// it rather than restating it.
/// What: returns `GH_CONFIG_DIR=<dir>` (plus `GH_USER=<account>`, informational
/// only, when an account is pinned) and NO `GH_TOKEN`: an env token outranks
/// the scoped config in `gh`'s own resolution order, so emitting both would
/// leave the config dir decorative. `warning` is set when `dir` holds no
/// credential.
/// Test: `config_dir_is_selected_over_a_minted_token`,
/// `config_dir_without_credential_still_pins_and_warns`.
fn scoped_config_dir_env(dir: &Path, account: Option<&str>) -> GhSpawnEnv {
    // #5851: reuse gh_identity's precedence chain — one implementation.
    let cfg = GithubConfig {
        config_dir: Some(dir.to_path_buf()),
        account: account.map(str::to_string),
        ..GithubConfig::default()
    };
    match crate::core::gh_identity::resolve_gh_env(Some(&cfg)) {
        Ok(env) => {
            let mut vars = env.vars().to_vec();
            if let Some(account) = account {
                vars.push((GH_USER_ENV_VAR.to_string(), account.to_string()));
            }
            let warning =
                (!config_dir_has_credential(dir)).then(|| no_credential_warning(dir, account));
            GhSpawnEnv { vars, warning }
        }
        // Unreachable in practice: `resolve_gh_env` only errors on the
        // account-ONLY case and `config_dir` is always `Some` here. Reported
        // rather than swallowed, so it can never become a silent fail-open.
        Err(e) => GhSpawnEnv {
            vars: Vec::new(),
            warning: Some(format!(
                "cannot pin this session to gh config dir {}: {e}",
                dir.display()
            )),
        },
    }
}

/// The warning for a pinned config dir that holds no credential (#5851).
///
/// Why: `gh`'s own exit-4 message tells the operator to run `gh auth login`,
/// which writes to the SHARED store and fixes nothing here. The message has to
/// name the directory and scope the login command to it.
/// What: one sentence of cause, one of remedy, and an explicit statement that
/// `tm` does not fall back to the machine-global account.
/// Test: `config_dir_without_credential_still_pins_and_warns`.
fn no_credential_warning(dir: &Path, account: Option<&str>) -> String {
    let dir = dir.display();
    let who = account.unwrap_or("the pinned account");
    format!(
        "gh config dir {dir} holds no github.com credential ({dir}/hosts.yml is missing or \
         names no account), so every `gh` call in this session will fail with exit 4. Run \
         `GH_CONFIG_DIR={dir} gh auth login` to authenticate as {who} inside it. tm does not \
         fall back to the machine-global gh account here — that fallback is the wrong-identity \
         defect #5851 fixes."
    )
}

/// Resolve the spawn-env overrides for a project's pinned `gh` identity, given
/// an injectable token resolver (#3025, reworked by #5851) — the pure,
/// hermetically testable core every call site shares.
///
/// Why: separating "which identity, if any" from "how a token is minted" lets
/// tests exercise every outcome with a fake `resolve_token` closure, matching
/// this codebase's trait-seam convention (`GitBackend`, `ManagedTmuxDriver`)
/// for I/O that cannot run hermetically in CI. #5851 added the `config_dir`
/// arm because `gh auth token -u <account>` does not select an account on a
/// keyring-backed host, so the token arm alone let a correctly-pinned project
/// run as whoever was globally active.
/// What: both inputs unset or blank → `None` (nothing to inject, no
/// regression). `config_dir` set → `Some(Ok(env))` carrying `GH_CONFIG_DIR`
/// and NEVER `GH_TOKEN`, with `resolve_token` not called at all; an env token
/// outranks the scoped config in `gh`'s resolution order, so emitting both
/// would leave the config dir decorative. `config_dir` unset but `gh_account`
/// set → the pre-#5851 path: `Some(Ok(env))` with `GH_TOKEN` then `GH_USER`
/// on success, `Some(Err(msg))` on failure — the caller logs `msg` and
/// proceeds WITHOUT injecting anything (#3025's documented failure mode:
/// spawn must never be blocked by a resolution failure).
/// Test: `resolve_gh_account_env_with_unset_is_none`,
/// `resolve_gh_account_env_with_blank_is_none`,
/// `resolve_gh_account_env_with_success_returns_token_and_user`,
/// `resolve_gh_account_env_with_failure_returns_err`,
/// `config_dir_is_selected_over_a_minted_token`,
/// `config_dir_without_credential_still_pins_and_warns`.
pub fn resolve_gh_account_env_with(
    gh_account: Option<&str>,
    config_dir: Option<&Path>,
    resolve_token: impl FnOnce(&str) -> Result<String, String>,
) -> Option<Result<GhSpawnEnv, String>> {
    let account = gh_account.map(str::trim).filter(|s| !s.is_empty());
    // Trimmed the same way `gh_identity::resolve_gh_env` trims it, so a
    // whitespace-only `config_dir` is "unset" here and there alike.
    let dir = config_dir
        .map(|d| d.to_string_lossy().trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);

    // #5851: a scoped config dir SELECTS the account and `gh auth token -u`
    // does not, so the token path is skipped entirely rather than layered on.
    if let Some(dir) = dir {
        return Some(Ok(scoped_config_dir_env(&dir, account)));
    }

    let account = account?;
    Some(resolve_token(account).map(|token| GhSpawnEnv {
        vars: vec![
            (GH_TOKEN_ENV_VAR.to_string(), token),
            (GH_USER_ENV_VAR.to_string(), account.to_string()),
        ],
        warning: None,
    }))
}

/// Production entry point: resolve a project's spawn-env `gh` overrides,
/// minting a token via the real `gh auth token -u <account>` CLI only when no
/// `config_dir` is pinned (#3025, #5851).
///
/// Why: the one call every spawn/relaunch site uses, so none of them has to
/// name [`gh_token_via_cli`] as a resolver argument itself.
/// What: delegates to [`resolve_gh_account_env_with`] with [`gh_token_via_cli`].
/// Test: covered via `resolve_gh_account_env_with`'s fake-resolver tests.
pub fn resolve_gh_account_env(
    gh_account: Option<&str>,
    config_dir: Option<&Path>,
) -> Option<Result<GhSpawnEnv, String>> {
    resolve_gh_account_env_with(gh_account, config_dir, gh_token_via_cli)
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
/// separately, directly tested via `find_pinned_gh_identity` below.
pub async fn resolve_gh_account_env_for_registry(
    registry: &crate::project::ProjectRegistry,
    cwd: &std::path::Path,
) -> Vec<(String, String)> {
    let cwd_for_origin = cwd.to_path_buf();
    let probe = tokio::task::spawn_blocking(move || {
        crate::daemon::managed_routes::inproject::get_origin_url(&cwd_for_origin)
    })
    .await;
    // #4734: still fail-open (see the doc above), but a git failure is now
    // reported rather than being indistinguishable from "no origin remote".
    let origin = match probe {
        Ok(Ok(origin)) => origin,
        Ok(Err(e)) => {
            tracing::warn!(
                cwd = %cwd.display(),
                "cannot read git origin remote; spawning without a pinned gh_account: {e}"
            );
            None
        }
        Err(_) => None,
    };
    let Some(origin) = origin else {
        return Vec::new();
    };

    let Some(pinned) = find_pinned_gh_identity(registry, &origin).await else {
        return Vec::new();
    };

    let cwd_for_log = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || {
        match resolve_gh_account_env(pinned.account.as_deref(), pinned.config_dir.as_deref()) {
            None => Vec::new(),
            Some(Ok(env)) => {
                // #5851: the vars still apply — a pinned-but-empty config dir
                // must fail closed, never fall back to the global account.
                if let Some(warning) = env.warning {
                    tracing::warn!(cwd = %cwd_for_log.display(), "{warning}");
                }
                env.vars
            }
            Some(Err(msg)) => {
                tracing::warn!(
                    cwd = %cwd_for_log.display(),
                    "gh_account token resolution failed; spawning without GH_TOKEN: {msg}"
                );
                Vec::new()
            }
        }
    })
    .await
    .unwrap_or_default()
}

/// A project's pinned `gh` identity as persisted on its registry record
/// (#5851).
///
/// Why: selection needs BOTH keys. `config_dir` is the one that actually
/// discriminates between logged-in accounts; `account` names who is expected
/// inside it (and is the only input the pre-#5851 token fallback has). Reading
/// them in one registry pass keeps the two from drifting apart at the call
/// site.
/// What: `account` is `Project::gh_account`; `config_dir` is
/// `Project::github.config_dir`. Either may be `None` independently.
/// Test: `find_pinned_gh_identity_reads_config_dir`,
/// `resolve_gh_account_env_for_registry_picks_up_registered_gh_account`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PinnedGhIdentity {
    /// `Project::gh_account` — the login this project's sessions run as.
    pub account: Option<String>,
    /// `Project::github.config_dir` — the scoped `gh` config home to pin to.
    pub config_dir: Option<PathBuf>,
}

/// Look up the pinned `gh` identity for the first registered project whose
/// `repo_url` matches `origin` — the pure(ish), registry-backed matching
/// step [`resolve_gh_account_env_for_registry`] delegates to, isolated so it
/// is directly testable against a real (temp-dir-backed) `ProjectRegistry`
/// fixture without needing a real git repository or a live `gh` subprocess
/// (#3025 review follow-up item 1: this is the exact step that proves the
/// registry — not the static config — is consulted).
///
/// #5851 widened it from `gh_account` alone to the `(account, config_dir)`
/// pair: returning only the account is what forced the caller down the
/// non-discriminating `gh auth token -u` path.
/// What: `None` when no project matches, or when the matched project pins
/// NEITHER key (nothing to inject, no regression).
/// Test: `resolve_gh_account_env_for_registry_picks_up_registered_gh_account`,
/// `resolve_gh_account_env_for_registry_no_match_is_none`,
/// `resolve_gh_account_env_for_registry_registered_without_gh_account_is_none`,
/// `find_pinned_gh_identity_reads_config_dir`.
async fn find_pinned_gh_identity(
    registry: &crate::project::ProjectRegistry,
    origin: &str,
) -> Option<PinnedGhIdentity> {
    let projects = registry.list().await.ok()?;
    let project = projects
        .iter()
        .find(|p| crate::project::record::repo_url_matches(&p.repo_url, origin))?;
    // #5851: `github.config_dir` already exists on the record and is persisted;
    // it was simply never read here.
    let pinned = PinnedGhIdentity {
        account: project.gh_account.clone(),
        config_dir: project
            .github
            .as_ref()
            .and_then(|cfg| cfg.config_dir.clone()),
    };
    (pinned != PinnedGhIdentity::default()).then_some(pinned)
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

    /// `gh auth status` for an env-token login — the #5032 reporter's exact
    /// output shape, including the `(GH_TOKEN)` annotation.
    const TOKEN_AUTH_STATUS: &str = "\
github.com
  ✓ Logged in to github.com account mac-duetto (GH_TOKEN)
  - Active account: true
  - Git operations protocol: https
  - Token scopes: 'repo', 'workflow'
";

    /// Why: the doctor reads the active login out of `gh auth status`, so the
    /// `- Active account: true` marker must bind to the login above it.
    /// Test: itself.
    #[test]
    fn parse_auth_status_active_from_marker() {
        let status = parse_gh_account_status_from_auth_status(MULTI_AUTH_STATUS);
        assert_eq!(status.active.as_deref(), Some("bob-duetto"));
        assert_eq!(
            status.logged_in,
            vec!["bob-duetto".to_string(), "bobmatnyc".to_string()]
        );
        assert!(status.is_ambiguous());
    }

    /// Why (#2121): a stored `gh_user` is later compared byte-for-byte by
    /// `verify_active_account`, so the lookup must return `gh`'s spelling
    /// rather than whatever case the caller typed.
    /// Test: itself.
    #[test]
    fn canonical_logged_in_login_is_case_insensitive() {
        let status = parse_gh_account_status_from_auth_status(MULTI_AUTH_STATUS);
        assert_eq!(
            status.canonical_logged_in_login("BobMatNyc").as_deref(),
            Some("bobmatnyc")
        );
        // A non-active account is still a valid choice for a project.
        assert_eq!(
            status
                .canonical_logged_in_login("  bob-duetto  ")
                .as_deref(),
            Some("bob-duetto")
        );
    }

    /// Why (#2121): the whole point of the lookup is to say "no" to a login
    /// `gh` has never seen, including on a host with no accounts at all.
    /// Test: itself.
    #[test]
    fn canonical_logged_in_login_rejects_unknown() {
        let status = parse_gh_account_status_from_auth_status(MULTI_AUTH_STATUS);
        assert_eq!(status.canonical_logged_in_login("typo-bot"), None);
        assert_eq!(
            GhAccountStatus::default().canonical_logged_in_login("bobmatnyc"),
            None
        );
    }

    /// Why: env-token auth writes no `hosts.yml`, so `gh auth status` is the
    /// ONLY place this login is visible (#5032).
    /// Test: itself.
    #[test]
    fn parse_auth_status_token_auth() {
        let status = parse_gh_account_status_from_auth_status(TOKEN_AUTH_STATUS);
        assert_eq!(status.active.as_deref(), Some("mac-duetto"));
        assert_eq!(status.logged_in, vec!["mac-duetto".to_string()]);
        assert!(!status.is_ambiguous());
    }

    /// Why: a genuinely unauthenticated `gh` must parse to an empty status —
    /// that is the one state the doctor may report as "not authenticated".
    /// Test: itself.
    #[test]
    fn parse_auth_status_unauthenticated() {
        let status =
            parse_gh_account_status_from_auth_status("You are not logged into any GitHub hosts.");
        assert_eq!(status, GhAccountStatus::default());
    }

    /// #5032 regression: `gh auth status` validates an env token over the
    /// network — measured 281–389 ms by the reporter and 366–373 ms locally.
    /// Under the old 250 ms bound every such probe timed out and the doctor
    /// reported a working login as "not authenticated". This pins the doctor's
    /// bound above realistic token-auth latency; it FAILS if
    /// [`GH_DOCTOR_TIMEOUT`] is ever lowered below the simulated 400 ms probe.
    #[test]
    fn doctor_probe_tolerates_token_auth_latency() {
        let probe = probe_gh_auth_with(GH_DOCTOR_TIMEOUT, || {
            std::thread::sleep(Duration::from_millis(400));
            Some(TOKEN_AUTH_STATUS.to_string())
        });
        let GhAuthProbe::Answered(status) = probe else {
            panic!("token-auth probe must answer under GH_DOCTOR_TIMEOUT, got {probe:?}");
        };
        assert_eq!(status.active.as_deref(), Some("mac-duetto"));
    }

    /// Why: a probe that outruns its bound must report UNKNOWN, never an empty
    /// (i.e. "unauthenticated") answer (#5032).
    /// Test: itself.
    #[test]
    fn probe_beyond_bound_is_inconclusive() {
        let probe = probe_gh_auth_with(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_millis(500));
            Some(TOKEN_AUTH_STATUS.to_string())
        });
        match probe {
            GhAuthProbe::Inconclusive(reason) => assert!(reason.contains("did not answer")),
            other => panic!("expected Inconclusive, got {other:?}"),
        }
    }

    /// Why: a `gh` that cannot be executed is also an unknown auth state, not a
    /// negative one.
    /// Test: itself.
    #[test]
    fn probe_missing_gh_is_inconclusive() {
        match probe_gh_auth_with(GH_DOCTOR_TIMEOUT, || None) {
            GhAuthProbe::Inconclusive(reason) => assert!(reason.contains("could not be run")),
            other => panic!("expected Inconclusive, got {other:?}"),
        }
    }

    /// Why: the happy path must parse straight through the seam.
    /// Test: itself.
    #[test]
    fn probe_answered_parses_accounts() {
        let probe = probe_gh_auth_with(GH_DOCTOR_TIMEOUT, || Some(MULTI_AUTH_STATUS.to_string()));
        assert_eq!(
            probe,
            GhAuthProbe::Answered(parse_gh_account_status_from_auth_status(MULTI_AUTH_STATUS))
        );
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
