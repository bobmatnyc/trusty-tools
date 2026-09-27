//! Spawning one `tga audit` child, and turning its exit into a verdict.
//!
//! Why: split out of `crate::run` when adding the run index (#6080) crossed the
//! 500-SLOC production cap — the eighth split for that reason, after
//! `selection`, `boards`, `github_issues`, `checkpoint`, `verify`, `pins` and
//! `report`. It separates on the same line those did: `run.rs` decides WHICH
//! repositories are audited and what the sweep records, and this file owns the
//! one process it starts per repository — the argument vector, the environment
//! it hands over, the timeout, and the pumps that keep the log whole.
//!
//! What: [`spawn_tga`], [`supervise`], [`join_pumps`], and the environment-variable names the
//! child reads. [`ENV_INFERENCE_CREDENTIAL`] is re-exported from `crate::run`,
//! which is where four other modules already name it.
//!
//! Test: `crate::run::run_tests`, which drives this through `sweep_with_env`
//! rather than calling it directly — the wiring between the sweep's resolution
//! and this spawn is the part worth proving.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Child;

use super::boards;
use super::github_issues;
use super::pins::PinnedBinaries;
use super::report::RepoResult;
use crate::config::EngagementConfig;
use crate::error::AuditError;
use crate::progress::Progress;
use crate::relay::{Scrubber, tee_and_relay};

/// The variable `tga audit` reads the inference credential from.
pub const ENV_INFERENCE_CREDENTIAL: &str = "OPENROUTER_API_KEY";

/// The variable that asks a child to relay its progress events (#5823).
///
/// Why: named through `trusty-progress` rather than spelled here, because the
/// producer reads the same constant — a literal in this file would be a second
/// copy of the contract, free to drift.
const ENV_PROGRESS_RELAY: &str = trusty_progress::relay::ENV_RELAY;

/// The variable `tga audit` reads its trusty-search binary from (#5670).
const ENV_SEARCH_BIN: &str = "TRUSTY_SEARCH_BIN";

/// The variable `trusty-review` reads its analyze binary from.
const ENV_ANALYZE_BIN: &str = "TRUSTY_ANALYZE_BIN";

/// The variable `tga audit` reads its report renderer from.
const ENV_REVIEW_BIN: &str = "TRUSTY_REVIEW_BIN";

/// Spawn the pinned `tga audit` and turn its exit into a per-repo verdict.
///
/// The child inherits nothing it does not need: the four binaries are named by
/// absolute path or by the variables tga and trusty-review read
/// (`TRUSTY_SEARCH_BIN`, `TRUSTY_ANALYZE_BIN`, `TRUSTY_REVIEW_BIN`), so nothing
/// on the operator's `PATH` can be reached instead. The credential goes in the
/// environment and only there — see `crate::run`'s module docs for what that
/// costs.
///
/// Alongside the credential the child gets the provider and per-role model ids
/// from [`crate::inference`]: naming the key never routed anything to
/// OpenRouter on its own, because `trusty-review` defaults to Bedrock (#5671).
///
/// A child that outlives `budget` is killed and recorded as a failure, so one
/// hung repository costs that repository rather than the whole run.
///
/// #8783: the child leads its own process group, and the kill reaches every
/// member. That includes any daemon `tga` auto-starts on this run —
/// trusty-analyze through `trusty_common::daemon_guard::spawn_detached`,
/// trusty-search through `daemon_guard::spawn_current_exe` — because
/// `daemon_guard::detached_command` nulls their stdio but never calls `setsid`,
/// so both inherit the group. A timeout therefore `SIGKILL`s them, and
/// a Ctrl-C forwarded by `crate::clone::stop_clones_on_interrupt` sends them
/// `SIGINT` and then `SIGKILL` 250 ms later. The next run starts them again.
///
/// #5823: the child's streams are PIPED rather than pointed straight at the log
/// file, and this function tees them — every byte still reaches the log, and the
/// progress lines the child writes on stderr additionally reach `progress`. The
/// log is unchanged as a record; what changed is that it is no longer the only
/// place the output goes.
///
/// #5869: `scrubber` filters both streams on the way to the log, because the
/// credential this function puts in the child's environment can come back out
/// of it — a provider's 401 body, a `git` remote URL in a clone failure. See
/// [`crate::relay`] for what that filtering can and cannot promise.
#[allow(clippy::too_many_arguments)]
pub(super) async fn spawn_tga(
    binaries: &PinnedBinaries,
    config: &EngagementConfig,
    inference: &[(&'static str, String)],
    boards: &boards::Boards,
    github_access: &github_issues::GithubAccess,
    config_path: &Path,
    output: &Path,
    log: &Path,
    cwd: &Path,
    budget: Duration,
    investigation: crate::grounding::priority::Budget,
    progress: &Progress,
    target: &str,
    scrubber: &Scrubber,
) -> Result<RepoResult, AuditError> {
    let file = std::fs::File::create(log).map_err(|source| AuditError::WorkDir {
        path: log.to_path_buf(),
        source,
    })?;
    let errors = file.try_clone().map_err(|source| AuditError::WorkDir {
        path: log.to_path_buf(),
        source,
    })?;

    let mut command = tokio::process::Command::new(&binaries.tga);
    command
        .arg("--config")
        .arg(config_path)
        .arg("audit")
        .arg("--output")
        .arg(output)
        .current_dir(cwd)
        .env(ENV_INFERENCE_CREDENTIAL, config.openrouter_key.expose())
        // #5823: ask the child to write its per-stage events where this process
        // can read them. A child too old to know the variable ignores it, and
        // the sweep shows the coarse per-repository progress it derives itself.
        .env(ENV_PROGRESS_RELAY, "1")
        // #5670: `tga audit` starts trusty-search and indexes each repository
        // through it. On a recipient's clean machine the pinned copy in
        // `work/tools/` is the only one there is, so without this the guard falls
        // through to a PATH lookup and refuses the run.
        .env(ENV_SEARCH_BIN, &binaries.search)
        .env(ENV_ANALYZE_BIN, &binaries.analyze)
        .env(ENV_REVIEW_BIN, &binaries.review)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // #8783: a group of its own, so the timeout kill reaches every process
        // the child forked — a grandchild holding the piped streams open kept
        // the pumps below from ever seeing EOF, and a 200 ms budget took 600 s.
        .process_group(0)
        .kill_on_drop(true);
    // #5671: the credential alone never reached OpenRouter — trusty-review
    // defaults to Bedrock, so the provider and the three role models must be
    // named too. Resolved by `sweep_with_env`: either all four or none, never a
    // subset that could pair one provider with another's model ids.
    for (name, value) in inference {
        command.env(name, value);
    }
    // #5857: the board credential the generated config only references. Exposed
    // here rather than held on `Boards`, so it lives no longer than this
    // `Command` — the same shape as the inference credential above.
    for (name, value) in boards.env(&config.boards) {
        command.env(name, value);
    }
    // #5980: the `gh`-derived credential the generated config's `github:`
    // section only references, when one was read — see `github_issues`'s
    // module docs for why a missing one does not stop the child from running.
    if let Some((name, value)) = github_access.env() {
        command.env(name, value);
    }
    // #6244: the same credential again, under the name tga's GIT TRANSPORT
    // reads. The line above serves the REST client that reads issues; the fetch
    // that runs before every collection resolves `GITHUB_TOKEN` instead, so
    // without this a recipient logged in only with `gh` had every fetch fail and
    // got a header-only `pr-metrics.csv` per repository. Set only when the sweep
    // resolved that the `gh` login is the only source — see
    // `github_issues::GithubAccess::git_transport_env`.
    if let Some((name, value)) = github_access.git_transport_env() {
        command.env(name, value);
    }
    // #6082: the investigation budget, down the one channel that reaches the
    // grandchild in time. `tga audit` writes the manifest and runs
    // `trusty-review report` against it in the same process, and this crate's
    // grounding pass edits that manifest only after the child exits — so the
    // budget it records there reaches a re-render and never this run's report.
    // See `grounding::priority::Budget::child_env`.
    // #6247: the sweep resolves it ONCE and hands it down, rather than this
    // spawn re-reading the environment. The same value is what the grounding
    // pass writes into the manifest, so the file cannot name a budget the
    // investigation did not run under.
    for (name, value) in investigation.child_env() {
        command.env(name, value);
    }
    // #6669: which report the engagement asked for, down the same channel and
    // for the same reason — the manifest arrives too late on this path, and an
    // argument would break an older pinned renderer. See
    // `EngagementConfig::report`'s `child_env`.
    for (name, value) in config.report.child_env() {
        command.env(name, value);
    }

    let child = match command.spawn() {
        Ok(child) => child,
        Err(source) => {
            return Ok(RepoResult::Failed {
                reason: format!("`tga audit` could not be started: {source}"),
            });
        }
    };
    Ok(supervise(
        child,
        (file, errors),
        log,
        budget,
        progress,
        target,
        scrubber,
    )
    .await)
}

/// Pump, bound and settle one spawned `tga audit` child.
///
/// Why: split from [`spawn_tga`] so the budget can start on a child whose
/// grandchild a test has already watched fork — against the spawn, a budget
/// could expire before the fork and leave nothing to prove the kill on (#8783).
/// What: registers the child's process group for Ctrl-C forwarding, tees both
/// streams into `logs`, waits under `budget`, and kills the whole group on
/// expiry or on a failed wait. The group is deregistered once the verdict is
/// computed — the leader is reaped by then, and a pgid left in the list could
/// be reused before a Ctrl-C reads it — and the pumps are joined last, under
/// [`DRAIN_GRACE`].
/// Test: `crate::run::run_tests::a_timeout_kill_reaches_the_childs_grandchild`,
/// `crate::run::run_tests::a_running_tga_group_is_registered_for_ctrl_c`.
pub(super) async fn supervise(
    mut child: Child,
    (file, errors): (std::fs::File, std::fs::File),
    log: &Path,
    budget: Duration,
    progress: &Progress,
    target: &str,
    scrubber: &Scrubber,
) -> RepoResult {
    // #8783: `process_group(0)` took this tree out of the terminal's foreground
    // group, so record it for `stop_clones_on_interrupt` to forward a Ctrl-C to.
    let detached = child.id().map(crate::clone::Detached::register);

    // #5823: both streams are pumped concurrently with the wait. Reading them
    // is not optional now that they are pipes — a child that fills a pipe
    // buffer nobody drains blocks forever, which would turn every sizeable
    // sweep into the four-hour timeout.
    let mut pumps = Vec::with_capacity(2);
    if let Some(stream) = child.stdout.take() {
        pumps.push(tokio::spawn(tee_and_relay(
            stream,
            tokio::fs::File::from_std(file),
            progress.clone(),
            target.to_owned(),
            scrubber.clone(),
        )));
    }
    if let Some(stream) = child.stderr.take() {
        pumps.push(tokio::spawn(tee_and_relay(
            stream,
            tokio::fs::File::from_std(errors),
            progress.clone(),
            target.to_owned(),
            scrubber.clone(),
        )));
    }

    let verdict = match tokio::time::timeout(budget, child.wait()).await {
        Ok(Ok(status)) if status.success() => RepoResult::Succeeded,
        Ok(Ok(status)) => RepoResult::Failed {
            reason: format!(
                "`tga audit` exited with {}; see {}",
                status
                    .code()
                    .map_or_else(|| "a signal".to_string(), |c| format!("code {c}")),
                log.display()
            ),
        },
        Ok(Err(source)) => {
            // #8783: a wait that failed says nothing about whether the tree is
            // still running, so it is ended the same way as a timeout.
            let killed = crate::clone::kill_tree(&mut child).await;
            RepoResult::Failed {
                reason: format!(
                    "`tga audit` could not be waited on: {source}{}",
                    kill_failure(killed)
                ),
            }
        }
        Err(_elapsed) => {
            // Kill before returning: `kill_on_drop` would do it, but only once
            // the handle drops, and the reason must name a child that is gone.
            // #8783: the whole group, not the direct child alone.
            let killed = crate::clone::kill_tree(&mut child).await;
            RepoResult::Failed {
                reason: format!(
                    "`tga audit` timed out after {}s and was killed{}; see {}",
                    budget.as_secs(),
                    kill_failure(killed),
                    log.display()
                ),
            }
        }
    };
    // #8783: deregister before the drain below, which can run for the grace.
    drop(detached);

    // The child has exited or been killed, so both pipes are at EOF and the
    // pumps end on their own. Awaiting them is what guarantees the log holds
    // everything the child said before this function reports on it.
    // #8783: under a deadline — a descendant that left the group (`setsid`)
    // still holds the pipes, and its EOF would come only when it exits.
    join_pumps(pumps, log, verdict, DRAIN_GRACE).await
}

/// The suffix a failure reason carries when the group kill itself failed.
fn kill_failure(killed: std::io::Result<()>) -> String {
    match killed {
        Ok(()) => String::new(),
        Err(e) => format!(" (kill failed: {e})"),
    }
}

/// How long the output pumps may run on after the child has exited or been
/// killed (#8783).
///
/// Their EOF normally follows the exit in milliseconds; only a process that
/// escaped the group kill can hold it back, and waiting for that process would
/// make the budget meaningless.
const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// Wait for the output pumps, downgrading a success whose log is incomplete.
///
/// Why: the log is the only record a failed sweep is diagnosed from, and this
/// module's posture is that a run whose result cannot be recorded must not
/// return as a success (#5655). A pump that failed means the log is missing
/// bytes the child wrote, so a `Succeeded` verdict resting on it is downgraded
/// rather than reported. A verdict that was already a failure keeps its own
/// reason — the pump error is the less useful of the two.
/// What: awaits each pump; on the first error, replaces a `Succeeded` verdict.
/// A pump task that panicked is treated the same way, and so is one still
/// running once `grace` has elapsed — it is aborted rather than awaited, and a
/// `Failed` verdict keeps its reason with that note appended (#8783).
/// Test: `crate::run::run_tests::a_childs_stage_events_reach_the_progress_sink`
/// covers the whole-log obligation this protects;
/// `crate::run::run_tests::a_pump_held_open_past_the_grace_downgrades_a_success`
/// and `crate::run::run_tests::a_pump_held_open_past_the_grace_extends_a_failure`
/// cover the deadline.
pub(super) async fn join_pumps(
    pumps: Vec<tokio::task::JoinHandle<std::io::Result<()>>>,
    log: &Path,
    verdict: RepoResult,
    grace: Duration,
) -> RepoResult {
    let deadline = tokio::time::Instant::now() + grace;
    // #8783: `{:?}`, because `as_secs` reads a sub-second grace as "0s".
    let held_open = format!(
        "a process the child left behind still held its output open {grace:?} after it ended"
    );
    let mut broken: Option<String> = None;
    let mut escaped = false;
    for mut pump in pumps {
        let failure = match tokio::time::timeout_at(deadline, &mut pump).await {
            Ok(Ok(Ok(()))) => None,
            Ok(Ok(Err(e))) => Some(e.to_string()),
            Ok(Err(e)) => Some(e.to_string()),
            Err(_elapsed) => {
                pump.abort();
                escaped = true;
                Some(held_open.clone())
            }
        };
        broken = broken.or(failure);
    }
    match (broken, verdict) {
        (Some(reason), RepoResult::Succeeded) => RepoResult::Failed {
            reason: format!(
                "`tga audit` finished but its output could not be written to {}: {reason}",
                log.display()
            ),
        },
        // #8783: an escaped holder is worth knowing on a failure too — it is
        // why the log may end mid-line.
        (_, RepoResult::Failed { reason }) if escaped => RepoResult::Failed {
            reason: format!("{reason}; {held_open}"),
        },
        (_, verdict) => verdict,
    }
}
