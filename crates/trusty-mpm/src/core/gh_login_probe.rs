//! Where the daemon asks `gh` whether an account is logged in (#9091).
//!
//! Why: the daemon runs under launchd, which passes no `GH_CONFIG_DIR`. An
//! operator whose shell exports one (a per-context dir such as
//! `~/.config/gh-<org>`) may have no `~/.config/gh` at all, so the daemon's
//! own `gh auth status` answers "You are not logged into any GitHub hosts"
//! while the operator's `gh` lists every account. The `--account` clone path
//! never hit this: it runs `gh` in tm's per-account dir
//! (`<state_root>/gh-accounts/<login>`, from
//! [`crate::core::gh_account_dir::tm_account_dir`]).
//!
//! What: [`probe_gh_login`] asks `gh auth status` in the daemon's own
//! environment and, with `GH_CONFIG_DIR` set, in each dir
//! [`AccountDirSources::candidates`] names for the login: the project's
//! pinned config dir and tm's per-account dir, which is the dir the clone path
//! uses. A dir that does not exist is not asked; one whose `config.yml`
//! would make gh migrate it is refused before gh runs there (#8510). The
//! answers are merged by [`merge_probes`]. [`classify_auth_status`] turns one
//! run into a [`GhAuthProbe`] and keeps "could not run gh" apart from "gh
//! reports no logged-in account".
//!
//! Test: `gh_login_probe_tests.rs`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use trusty_common::gh::{GhCommand, GhOutput};

use crate::core::gh_account::{
    GH_CONFIG_DIR_ENV, GhAccountStatus, GhAuthProbe, parse_gh_account_status_from_auth_status,
};
use crate::core::gh_account_dir::{AccountDirSources, refuse_unmigrated_config};

/// Runs `gh auth status`, with `GH_CONFIG_DIR` set to the dir when one is given.
///
/// `Err` carries why `gh` could not be spawned. The seam lets tests stand in a
/// fake `gh` per config dir with no `PATH` change and no subprocess.
pub(crate) type AuthStatusRunner =
    Arc<dyn Fn(Option<&Path>) -> Result<GhOutput, String> + Send + Sync>;

/// gh's own words for a host with no account at all.
const NO_HOSTS: &str = "not logged into any GitHub hosts";

/// gh's own words for an account whose stored token it rejected.
const FAILED_LOGIN: &str = "Failed to log in to";

/// Classify one `gh auth status` run (#9091).
///
/// Why: "gh could not be run" and "gh ran and reports no account" are
/// different facts, and only the second is the caller's to fix. A non-zero
/// exit carrying neither an account line nor gh's own "not logged in" words
/// (a broken config, a crash) is neither fact, so it is not a "none".
/// What: a spawn failure is `Inconclusive("could not run gh: <reason>")`.
/// Output naming a logged-in account, gh's "not logged into any GitHub hosts",
/// or a rejected-token line is `Answered`. Any other output is `Inconclusive`
/// with the exit status and gh's first line.
/// Test: `a_spawn_failure_is_could_not_run_gh`,
/// `gh_reporting_no_hosts_is_a_definite_none`,
/// `an_unrecognised_failure_is_not_a_none`.
pub(crate) fn classify_auth_status(run: Result<GhOutput, String>) -> GhAuthProbe {
    let out = match run {
        Ok(out) => out,
        Err(why) => return GhAuthProbe::Inconclusive(format!("could not run gh: {why}")),
    };
    let text = out.combined();
    let status = parse_gh_account_status_from_auth_status(&text);
    if !status.logged_in.is_empty() || text.contains(NO_HOSTS) || text.contains(FAILED_LOGIN) {
        return GhAuthProbe::Answered(status);
    }
    let first = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no output");
    let exit = out
        .code
        .map_or_else(|| "a signal".to_string(), |c| format!("exit {c}"));
    GhAuthProbe::Inconclusive(format!(
        "could not read gh's answer: `gh auth status` ended with {exit} and named no account \
         ({first})"
    ))
}

/// The real runner: `gh auth status` through the workspace's one `gh` entry point.
fn run_auth_status(dir: Option<&Path>) -> Result<GhOutput, String> {
    let mut cmd = GhCommand::new(["auth", "status"]);
    if let Some(dir) = dir {
        cmd = cmd.env(GH_CONFIG_DIR_ENV, dir);
    }
    cmd.output_blocking().map_err(|e| e.to_string())
}

/// Ask `gh` whether `login` is logged in, in every place tm keeps gh state for it.
///
/// Why: see the module docs — the daemon's own environment can hold no gh
/// config while the operator's accounts live in a pinned dir or in tm's
/// per-account dir.
/// What: one `gh auth status` in the daemon's environment, plus one per
/// existing candidate dir from `sources` with `GH_CONFIG_DIR` set. A candidate
/// that cannot be named safely, or that [`refuse_unmigrated_config`] refuses,
/// is `Inconclusive` and gh never runs there. All runs start together and
/// share one `timeout`; a run still going at the deadline is `Inconclusive`.
/// The answers are merged by [`merge_probes`].
/// Test: `a_pinned_config_dir_with_two_accounts_is_seen_under_launchd`,
/// `the_clone_paths_account_dir_is_seen_under_launchd`,
/// `gh_missing_everywhere_fails_closed_with_its_reason`,
/// `a_dir_gh_would_migrate_is_never_asked`.
pub(crate) fn probe_gh_login_with(
    login: &str,
    sources: &AccountDirSources,
    timeout: Duration,
    run: AuthStatusRunner,
) -> GhAuthProbe {
    let mut asked: Vec<Option<PathBuf>> = vec![None];
    let mut probes: Vec<GhAuthProbe> = Vec::new();
    for candidate in sources.candidates(login) {
        let dir = match candidate {
            Ok(dir) => dir,
            Err(why) => {
                probes.push(GhAuthProbe::Inconclusive(why));
                continue;
            }
        };
        if !dir.exists() {
            continue;
        }
        match refuse_unmigrated_config(&dir) {
            Ok(()) => asked.push(Some(dir)),
            Err(why) => probes.push(GhAuthProbe::Inconclusive(why)),
        }
    }

    let deadline = Instant::now() + timeout;
    let pending: Vec<(Option<PathBuf>, mpsc::Receiver<GhAuthProbe>)> = asked
        .into_iter()
        .map(|dir| {
            let (tx, rx) = mpsc::channel();
            let (run, for_thread) = (Arc::clone(&run), dir.clone());
            std::thread::spawn(move || {
                let _ = tx.send(classify_auth_status(run(for_thread.as_deref())));
            });
            (dir, rx)
        })
        .collect();
    for (dir, rx) in pending {
        let left = deadline.saturating_duration_since(Instant::now());
        probes.push(rx.recv_timeout(left).unwrap_or_else(|_| {
            let place = dir.map_or_else(
                || "the daemon's environment".to_string(),
                |d| d.display().to_string(),
            );
            GhAuthProbe::Inconclusive(format!(
                "`gh auth status` in {place} did not answer within {timeout:?}"
            ))
        }));
    }
    let merged = merge_probes(login, probes);
    tracing::debug!(login, ?merged, "#9091: gh login probe");
    merged
}

/// Fold several `gh auth status` answers into one for `login`.
///
/// Why: one place that can answer is enough to accept a login, but a "none"
/// is only definite when every place answered.
/// What: the union of every answered `logged_in` (case-insensitive, first
/// spelling kept) and the first answered `active`. `Answered(union)` when the
/// union holds `login` or nothing was inconclusive; otherwise
/// `Inconclusive` with every reason, joined by `; `.
/// Test: `an_answer_elsewhere_outranks_an_unknown`,
/// `gh_missing_everywhere_fails_closed_with_its_reason`.
fn merge_probes(login: &str, probes: Vec<GhAuthProbe>) -> GhAuthProbe {
    let mut status = GhAccountStatus::default();
    let mut unknown: Vec<String> = Vec::new();
    for probe in probes {
        match probe {
            GhAuthProbe::Answered(answer) => {
                for name in answer.logged_in {
                    if !status
                        .logged_in
                        .iter()
                        .any(|known| known.eq_ignore_ascii_case(&name))
                    {
                        status.logged_in.push(name);
                    }
                }
                status.active = status.active.or(answer.active);
            }
            GhAuthProbe::Inconclusive(why) => unknown.push(why),
        }
    }
    if unknown.is_empty() || status.canonical_logged_in_login(login).is_some() {
        GhAuthProbe::Answered(status)
    } else {
        GhAuthProbe::Inconclusive(unknown.join("; "))
    }
}

/// Production entry point for the daemon's `gh_user` check (#9091).
///
/// Why: the daemon must see the accounts the operator's own `gh` sees, which
/// its launchd environment alone does not show it.
/// What: candidates come from [`AccountDirSources::for_origin`] for `origin`
/// (the static config's binding for it), with `pinned_config_dir` (the
/// registry record's own `github.config_dir`) taking that slot when set, and
/// tm's per-account dir under the state root. The daemon's own gh config dir
/// is not a candidate: the environment run already asks it. Blocking; run it
/// off the async executor.
/// Test: none directly (real `gh`); the decisions are
/// [`probe_gh_login_with`]'s.
pub(crate) fn probe_gh_login(
    login: &str,
    origin: Option<&str>,
    pinned_config_dir: Option<PathBuf>,
    timeout: Duration,
) -> GhAuthProbe {
    let state_root = crate::core::paths::FrameworkPaths::default().root;
    let mut sources = match origin {
        Some(origin) => AccountDirSources::for_origin(
            &crate::core::trusty_tools_config::TrustyToolsConfig::load(),
            origin,
            state_root,
            None,
        ),
        None => AccountDirSources {
            state_root: Some(state_root),
            ..AccountDirSources::default()
        },
    };
    if pinned_config_dir.is_some() {
        sources.static_config_dir = pinned_config_dir;
    }
    probe_gh_login_with(login, &sources, timeout, Arc::new(run_auth_status))
}

#[cfg(test)]
#[path = "gh_login_probe_tests.rs"]
mod tests;
