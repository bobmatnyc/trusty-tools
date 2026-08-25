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
//! What: [`spawn_tga`], [`join_pumps`], and the environment-variable names the
//! child reads. [`ENV_INFERENCE_CREDENTIAL`] is re-exported from `crate::run`,
//! which is where four other modules already name it.
//!
//! Test: `crate::run::run_tests`, which drives this through `sweep_with_env`
//! rather than calling it directly — the wiring between the sweep's resolution
//! and this spawn is the part worth proving.

use std::path::Path;
use std::process::Stdio;

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
    budget: std::time::Duration,
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

    let spawned = command.spawn();

    let mut child = match spawned {
        Ok(child) => child,
        Err(source) => {
            return Ok(RepoResult::Failed {
                reason: format!("`tga audit` could not be started: {source}"),
            });
        }
    };

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
        Ok(Err(source)) => RepoResult::Failed {
            reason: format!("`tga audit` could not be waited on: {source}"),
        },
        Err(_elapsed) => {
            // Kill before returning: `kill_on_drop` would do it, but only once
            // the handle drops, and the reason must name a child that is gone.
            let killed = child.kill().await;
            RepoResult::Failed {
                reason: format!(
                    "`tga audit` timed out after {}s and was killed{}; see {}",
                    budget.as_secs(),
                    match killed {
                        Ok(()) => String::new(),
                        Err(e) => format!(" (kill failed: {e})"),
                    },
                    log.display()
                ),
            }
        }
    };

    // The child has exited or been killed, so both pipes are at EOF and the
    // pumps end on their own. Awaiting them is what guarantees the log holds
    // everything the child said before this function reports on it.
    Ok(join_pumps(pumps, log, verdict).await)
}

/// Wait for the output pumps, downgrading a success whose log is incomplete.
///
/// Why: the log is the only record a failed sweep is diagnosed from, and this
/// module's posture is that a run whose result cannot be recorded must not
/// return as a success (#5655). A pump that failed means the log is missing
/// bytes the child wrote, so a `Succeeded` verdict resting on it is downgraded
/// rather than reported. A verdict that was already a failure keeps its own
/// reason — the pump error is the less useful of the two.
/// What: awaits each pump; on the first error, replaces a `Succeeded` verdict.
/// A pump task that panicked is treated the same way.
/// Test: `crate::run::run_tests::a_childs_stage_events_reach_the_progress_sink`
/// covers the whole-log obligation this protects.
async fn join_pumps(
    pumps: Vec<tokio::task::JoinHandle<std::io::Result<()>>>,
    log: &Path,
    verdict: RepoResult,
) -> RepoResult {
    let mut broken: Option<String> = None;
    for pump in pumps {
        let failure = match pump.await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e.to_string()),
            Err(e) => Some(e.to_string()),
        };
        broken = broken.or(failure);
    }
    match (broken, &verdict) {
        (Some(reason), RepoResult::Succeeded) => RepoResult::Failed {
            reason: format!(
                "`tga audit` finished but its output could not be written to {}: {reason}",
                log.display()
            ),
        },
        _ => verdict,
    }
}
