//! The `gh` subprocess runner the merged-PR reclaim survey calls (#2919, #6561).
//!
//! Why: split out of `worktree_reclaim` so that file stays under the 500-SLOC
//! production cap, and because this half answers a different question. That
//! module decides what a pull-request state MEANS; this one is responsible for
//! obtaining it from `gh` and — since #6561 — for saying WHY it could not.
//!
//! #6561: every failure used to collapse into one `None`. The daemon runs with
//! neither `GH_TOKEN` nor `GH_CONFIG_DIR` in its environment, so on a host that
//! keeps its credentials in a scoped config directory `gh pr list` exits 4 with
//! `To get started with GitHub CLI, please run:  gh auth login`. That exit was
//! discarded, every branch read `Unknown`, and `--merged-prs` reported
//! `0 worktree(s) reclaimable` — indistinguishable from a healthy sweep over a
//! workspace with nothing to reclaim. The runner now returns the exit code and
//! the first line of `gh`'s own stderr so the caller can disclose it.
//!
//! Test: `run_with_timeout_captures_output`, `run_with_timeout_kills_a_hung_child`,
//! `run_with_timeout_reports_the_exit_code_and_stderr` in
//! `worktree_reclaim::worktree_reclaim_tests`.
//!
//! #6623: the daemon runs under launchd, which carries neither `GH_TOKEN` nor
//! `GH_CONFIG_DIR` — every `gh` call made from this module used to inherit
//! that bare environment and exit 4 ("gh auth login"). [`resolve_daemon_gh_env`]
//! resolves the same per-project/global `github:` binding an interactive `tm`
//! invocation would (via `core::gh_identity`), and [`gh_command`] applies it.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::core::gh_identity::{self, GhEnv};
use crate::core::trusty_tools_config::TrustyToolsConfig;

/// Environment variables that would point `gh`/`git` at a DIFFERENT repository.
///
/// Why: the same hazard `worktree_safety::GIT_REDIRECTING_ENV` documents —
/// `gh` resolves the repository through git, and an inherited `GIT_DIR` names
/// a repository the worktree under inspection has nothing to do with. A
/// work-destroying gate must not be steerable by ambient environment.
/// What: removed from the child environment so the repository is resolved from
/// the working directory alone.
/// Test: `gh_command_strips_repository_redirecting_env`.
const GH_STRIPPED_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GH_REPO",
];

/// The `--json` field set every `gh pr list` call in this module requests.
///
/// Why: named once so the bulk index and the per-branch fallback cannot drift
/// into asking for different facts. `isCrossRepository` is not optional — see
/// `PrRow::is_cross_repository`; a `gh` that does not understand the field
/// makes the whole call fail, which yields an unavailable index and blocks,
/// rather than silently returning rows with fork PRs indistinguishable from
/// local ones.
///
/// `headRefName` is what resolves a SQUASH-MERGED pull request whose head
/// branch was deleted at merge (#6561): the branch is gone from the remote, but
/// the pull request still records the name it was opened from, so
/// `gh pr list --head <branch> --state all` still answers `MERGED`.
/// Test: `pr_index_skips_fork_pull_requests`,
/// `pr_index_resolves_a_squash_merged_pr_whose_head_branch_was_deleted`.
pub(crate) const PR_JSON_FIELDS: &str = "number,headRefName,state,isCrossRepository";

/// Wall-clock ceiling for any single `gh` invocation (#2919).
///
/// Why: `gh pr list` is a NETWORK call. `std::process::Command::output()` has
/// no timeout, so a wedged or throttled request hangs the calling thread
/// forever — and because this runs inside `spawn_blocking`, which cannot be
/// cancelled, that hang propagates to runtime shutdown. This is the same
/// failure shape already fixed for the byte walk; a subprocess needs its own
/// bound for the same reason.
/// What: 10 seconds, after which the child is killed and the call reports a
/// timeout — which resolves to a blocked candidate either way.
/// Test: `run_with_timeout_kills_a_hung_child`.
pub(crate) const GH_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait for a drained pipe's contents after the child has exited.
const PIPE_DRAIN_WAIT: Duration = Duration::from_secs(2);

/// A `gh` invocation rooted at `dir` with a sanitised environment and the
/// resolved `gh` identity applied (#2919, #6623).
///
/// Why: `gh` resolves the repository from its WORKING DIRECTORY. It has no
/// global `-C` flag — that is a `git` habit, and carrying it across made every
/// call in this module fail at flag parsing with
/// `unknown shorthand flag: 'C' in -C` (verified against gh 2.96.0). The
/// failure was silent by design: a failed call made every branch read
/// `BranchPrState::Unknown`, `classify` gate 5 blocked every candidate, and
/// `--merged-prs` reclaimed zero bytes ALWAYS. Hence `current_dir`, and hence
/// `gh_command_passes_no_dash_c_flag`.
///
/// The pager and prompt suppression are not cosmetic either — an interactive
/// prompt in a daemon-run probe hangs it forever. See [`GH_STRIPPED_ENV`] for
/// the environment scrub.
///
/// #6623: `gh_env` is applied BEFORE the pager/prompt overrides so a `gh`
/// spawned under launchd — which inherits neither `GH_TOKEN` nor
/// `GH_CONFIG_DIR` — authenticates the same way an interactive `tm`
/// invocation would. It is injected rather than resolved internally so the
/// existing hermetic tests (a bare `/tmp` directory, no real config on disk)
/// stay pure; [`resolve_daemon_gh_env`] is the production resolver every real
/// call site uses.
/// What: `gh` with its working directory set to `dir`, the repository-
/// redirecting variables removed, `gh_env`'s overrides applied, and
/// pager/prompt/colour disabled.
/// Test: `gh_command_passes_no_dash_c_flag`,
/// `gh_command_runs_in_the_requested_directory`,
/// `gh_command_strips_repository_redirecting_env`,
/// `gh_command_applies_the_resolved_gh_env`.
pub(crate) fn gh_command(dir: &Path, gh_env: &GhEnv) -> Command {
    // #5475: argv/binary/env come from trusty-common's single `gh` entry
    // point; this module keeps its own kill-on-timeout runner, which is why it
    // takes the unspawned `std::process::Command` rather than a runner method.
    // #2919: `current_dir`, NEVER `-C` — `gh` has no such flag.
    let mut cmd = trusty_common::gh::GhCommand::bare().cwd(dir);
    for key in GH_STRIPPED_ENV {
        cmd = cmd.env_remove(key);
    }
    // #6668: clear the inherited identity BEFORE applying the binding — gh
    // reads an env token ahead of `GH_CONFIG_DIR`, so leaving the shell's
    // `GH_TOKEN` in place left the binding decorative.
    for key in gh_env.unset_vars() {
        cmd = cmd.env_remove(key);
    }
    for (key, value) in gh_env.vars() {
        cmd = cmd.env(key, value);
    }
    cmd.env("GH_PAGER", "")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1")
        .to_std_command()
}

/// Resolve the [`GhEnv`] to apply to a `gh` spawn rooted at `dir` — the
/// production wiring every real call site in this module uses (#6623).
///
/// Why: the daemon's `gh` spawn sites have a WORKING DIRECTORY only. Under
/// launchd they inherit neither `GH_TOKEN` nor `GH_CONFIG_DIR` (this issue's
/// root cause), so each call must resolve its own identity the same way an
/// interactive `tm` invocation's `resolve_project_aware` does. Kept separate
/// from [`gh_identity::select_config_for_origin`] (the pure selection) so that
/// function stays unit-testable without disk or subprocess I/O; this wrapper
/// is the impure boundary, deliberately thin and exercised only through its
/// callers — mirroring `bin/tm/gh_identity::load_gh_env`.
/// What: reads `dir`'s `remote.origin.url` (best-effort — an unreadable or
/// non-git directory falls through to the global tier, never a hard failure,
/// and is logged at `warn`), loads [`TrustyToolsConfig`] from disk, and
/// resolves via [`gh_identity::select_config_for_origin`] +
/// [`gh_identity::resolve_gh_env`]. An `account`-only binding (refused by
/// `resolve_gh_env` — see its module docs) is logged and treated as ambient:
/// a housekeeping spawn must not mutate the operator's global `gh` account,
/// and a misconfigured project must not block the whole reclaim survey.
/// Test: exercised through the production `gh_command` call sites
/// (`PrIndex::from_gh`, `pr_state_for_branch`); the pure selection is unit-
/// tested via `select_config_for_origin_*` in `core::gh_identity`.
pub(crate) fn resolve_daemon_gh_env(dir: &Path) -> GhEnv {
    let origin_url = crate::daemon::managed_routes::inproject::get_origin_url(dir)
        .inspect_err(|e| {
            tracing::warn!(
                dir = %dir.display(),
                "worktree-reclaim: cannot read git origin remote for gh identity \
                 resolution — falling back to the global github: binding (#6623): {e}"
            );
        })
        .ok()
        .flatten();
    let config = TrustyToolsConfig::load();
    let selected = gh_identity::select_config_for_origin(&config, origin_url.as_deref());
    match gh_identity::resolve_gh_env(selected) {
        Ok(env) => env,
        Err(e) => {
            tracing::warn!(
                dir = %dir.display(),
                "worktree-reclaim: {e} — spawning gh with the ambient environment (#6623)"
            );
            GhEnv::default()
        }
    }
}

/// Read `pipe` to EOF on its own thread, handing the text back over a channel.
///
/// Why: a child that fills a pipe buffer blocks forever if nobody reads it,
/// which would defeat the timeout [`run_with_timeout`] enforces — a 400-PR JSON
/// reply comfortably exceeds a 64 KiB pipe buffer. Both pipes are drained for
/// that reason, and since #6561 stderr's contents are KEPT rather than sunk.
/// Test: `run_with_timeout_reports_the_exit_code_and_stderr`.
fn drain_pipe(mut pipe: impl Read + Send + 'static) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });
    rx
}

/// One operator-readable line naming why a `gh` call failed (#6561).
///
/// Why: the reason is rendered into a CLI summary and a doctor message, so it
/// has to be one line and has to carry the fact that identifies the fix — the
/// exit code plus `gh`'s own first complaint. `gh auth login` and
/// `gh: command not found` need opposite responses and used to read alike.
/// What: the first non-blank stderr line, prefixed by the exit code, or by a
/// signal note when the child was killed rather than exiting.
/// Test: `run_with_timeout_reports_the_exit_code_and_stderr`.
fn failure_reason(code: Option<i32>, stderr: &str) -> String {
    let first = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no error output");
    match code {
        Some(c) => format!("`gh` exited {c}: {first}"),
        None => format!("`gh` was terminated by a signal: {first}"),
    }
}

/// Run `cmd`, killing it if it outlives `budget` (#2919, #6561).
///
/// Why: see [`GH_TIMEOUT`]. `Command::output()` waits indefinitely.
/// What: spawns the child with both pipes drained on their own threads (see
/// [`drain_pipe`]), polls `try_wait` until the budget expires, then kills and
/// reaps. `Ok` carries stdout; `Err` carries a one-line reason — a spawn
/// failure, a non-zero exit with `gh`'s own first stderr line, a timeout, or a
/// wait error. Every `Err` blocks reclamation; #6561 is that the caller can now
/// tell the operator which one it was.
/// Test: `run_with_timeout_captures_output`, `run_with_timeout_kills_a_hung_child`,
/// `run_with_timeout_reports_the_exit_code_and_stderr`.
pub(crate) fn run_with_timeout(mut cmd: Command, budget: Duration) -> Result<String, String> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("`gh` could not be run: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "`gh` exposed no stdout pipe".to_string())
        .map(drain_pipe)?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "`gh` exposed no stderr pipe".to_string())
        .map(drain_pipe)?;
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = stdout.recv_timeout(PIPE_DRAIN_WAIT).unwrap_or_default();
                if status.success() {
                    return Ok(out);
                }
                let err = stderr.recv_timeout(PIPE_DRAIN_WAIT).unwrap_or_default();
                return Err(failure_reason(status.code(), &err));
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`gh` did not answer within {}s and was killed",
                    budget.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(format!("`gh` could not be waited on: {e}")),
        }
    }
}
