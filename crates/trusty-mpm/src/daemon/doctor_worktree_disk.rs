//! `tm doctor` worktree disk-consumption probe (#2919).
//!
//! Why: the sibling `worktrees` probe counts ORPHANS — worktrees no session
//! claims. It has never reported a single byte, so on 2026-07-21 it read the
//! same whether `.base/.claude/worktrees` held 4 GiB or the 1.1 TiB it actually
//! held. Disk pressure was discovered by a human running `du`, four days after
//! a manual sweep had already reclaimed the same terabyte. This probe is the
//! early-warning half of #2919: it puts the number on screen before the volume
//! fills, and names how much of it a merged pull request has already made
//! disposable.
//!
//! What: one `worktree_disk` check driven by
//! [`crate::session_manager::worktree_reclaim::survey`], which is read-only —
//! this probe opens no destructive path and never removes anything.
//! Test: `worktree_disk_check_is_ok_without_a_repos_root`,
//! `worktree_disk_check_warns_when_bytes_are_reclaimable`,
//! `worktree_disk_check_is_unknown_when_no_pr_state_resolved`.

use std::path::{Path, PathBuf};

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::session_manager::worktree_ownership::AgentDelegationState;
use crate::session_manager::worktree_reclaim::ReclaimSurvey;
use crate::session_manager::worktree_reclaim_sweep::{SurveyBudget, survey};

/// Wall-clock budget for the survey this probe runs.
///
/// Why: `tm doctor` is an interactive diagnostic, and the whole
/// `/api/v1/doctor` response has to land inside the client's 10-second
/// `DEFAULT_REQUEST_TIMEOUT`. Summing bytes over a terabyte-scale worktree
/// store takes MINUTES, so this probe cannot be given "as long as it needs" —
/// an earlier 30-second budget here failed
/// `execute_doctor_against_test_daemon` with "daemon unreachable" because the
/// client gave up first. A probe that breaks the endpoint it reports on is
/// worse than a partial one.
/// What: 3 seconds. Whatever is measured in that window is reported; the rest
/// is disclosed as an explicit UNDERCOUNT naming how many worktrees went
/// unmeasured, and the message points at `tm session prune-worktrees
/// --merged-prs`, whose survey runs with NO deadline when an operator wants
/// exact numbers. Deliberately a partial-but-honest answer rather than a slow
/// exact one.
/// Test: `worktree_disk_timeout_is_a_bounded_constant`.
const SURVEY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Extra margin the outer timeout allows beyond the survey's own deadline.
///
/// Why: the survey checks its deadline between walk entries and between
/// worktrees, but cannot interrupt a `git` or `gh` subprocess already in
/// flight. Firing the outer timeout at exactly the same instant would report
/// "exceeded" for a survey that was about to return normally.
/// What: 2 seconds — long enough for an in-flight subprocess to finish, short
/// enough that the 5-second worst case still clears the client's 10-second
/// request timeout.
/// Test: `worktree_disk_timeout_is_a_bounded_constant`.
const SURVEY_TIMEOUT_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Render a byte count the way an operator reads a disk figure.
///
/// Why: "1180591620717411303424" is not an early warning. The 1.1 TiB in the
/// post-mortem is only legible in binary units.
/// What: binary units (1024-based), one decimal place above KiB.
/// Test: `human_bytes_renders_binary_units`.
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// The pure verdict for [`check_worktree_disk`], separated for hermetic tests.
///
/// Why: the branch logic is the part worth pinning, and pinning it must not
/// require a real repos root, a real `gh`, or a terabyte of fixtures.
/// What:
/// - no worktrees at all → `Ok`;
/// - every worktree's pull-request state indeterminate → `Unknown`, because a
///   survey that resolved nothing has not established the workspace is healthy;
///   it names `gh` so the operator can fix the cause;
/// - some bytes reclaimable → `Warn` with the total, the reclaimable share, and
///   the command that reclaims it;
/// - the walk was INCOMPLETE → `Unknown`. A 3-second budget against a walk that
///   exceeds 600 seconds on this repository means the figure is a floor, not a
///   total, so a healthy-looking verdict computed from it would be exactly
///   backwards: the fuller the disk, the less of it gets measured, and the more
///   confidently the probe would report `Ok`. A disk check that reads healthy on
///   a full disk inverts its own purpose;
/// - otherwise → `Ok` with the total, still reporting the number.
///
/// `Fail` is never returned: disk consumption is a pressure signal, not a
/// broken component, and a probe that fails the whole report for a large but
/// legitimate workspace would train operators to ignore it.
/// Test: `worktree_disk_check_warns_when_bytes_are_reclaimable`,
/// `worktree_disk_check_is_unknown_when_no_pr_state_resolved`,
/// `worktree_disk_check_is_unknown_when_the_walk_is_incomplete`,
/// `worktree_disk_check_is_ok_when_nothing_is_reclaimable`.
fn build_worktree_disk_check(survey: &ReclaimSurvey) -> DoctorCheck {
    let total = survey.candidates.len();
    if total == 0 {
        return DoctorCheck::new(
            "worktree_disk",
            CheckStatus::Ok,
            "no git-registered worktrees under the managed workspace root",
        );
    }
    let measured = format!(
        "{} across {total} worktree(s)",
        human_bytes(survey.total_bytes)
    );
    let undercount = if survey.unmeasured > 0 {
        format!(
            " — an UNDERCOUNT: {} worktree(s) went unmeasured inside this probe's {}s budget; \
             `tm session prune-worktrees --merged-prs` surveys with no deadline for exact figures",
            survey.unmeasured,
            SURVEY_TIMEOUT.as_secs()
        )
    } else {
        String::new()
    };
    // #2919 MEDIUM: this probe reads only the most recent
    // `PR_INDEX_LIMIT` pull requests, and this repository has thousands — so a
    // worktree whose PR is older than that window reads "state unknown" and can
    // never be classified here. Saying so is the difference between "nothing is
    // reclaimable" and "we did not look far enough back".
    // #2919: the two causes of an indeterminate pull-request state are reported
    // SEPARATELY. They read identically but need opposite actions — one says
    // "check `gh`", the other says "the probe ran out of time". Conflating them
    // cost a whole review round establishing that 169 unknowns against the real
    // store were unreachable repos and detached HEADs, not a bug.
    let lookup_unknown = survey.pr_state_unknown.saturating_sub(survey.not_inspected);
    let mut clauses = String::new();
    if survey.not_inspected > 0 {
        clauses.push_str(&format!(
            "; {} worktree(s) were NOT INSPECTED before this probe's {}s classify budget \
             expired — that is a budget limit, not a `gh` problem",
            survey.not_inspected,
            SURVEY_TIMEOUT.as_secs()
        ));
    }
    if lookup_unknown > 0 {
        clauses.push_str(&format!(
            "; {lookup_unknown} worktree(s) were inspected but their pull-request state did \
             not resolve — a detached HEAD, a repository this `gh` account cannot see, or a \
             branch older than the most recent {} pull requests this probe reads; \
             `tm session prune-worktrees --merged-prs` resolves that last case \
             branch-by-branch",
            crate::session_manager::worktree_reclaim::PR_INDEX_LIMIT
        ));
    }
    let window = clauses;
    if survey.pr_state_unknown == total {
        return DoctorCheck::new(
            "worktree_disk",
            CheckStatus::Unknown,
            format!(
                "{measured}{undercount}; pull-request state resolved for NONE of them, \
                 so nothing could be classified reclaimable — check that `gh` is \
                 installed and authenticated (`gh auth status`) (#2919)"
            ),
        );
    }
    if survey.reclaimable > 0 {
        return DoctorCheck::new(
            "worktree_disk",
            CheckStatus::Warn,
            format!(
                "{measured}{undercount}; {} across {} of {} worktree(s) measured sits on \
                 branches whose pull request already merged and holds no uncommitted or \
                 unpushed work — reclaim with `tm session prune-worktrees \
                 --merged-prs`{window} (#2919)",
                human_bytes(survey.reclaimable_bytes),
                survey.reclaimable_measured,
                survey.reclaimable
            ),
        );
    }
    if survey.unmeasured > 0 {
        // #2919 HIGH: never `Ok` on a partial walk. The measured figure is a
        // floor, and the bigger the store the smaller the fraction of it that
        // fits the budget — so `Ok` here would be most confident exactly when
        // it is least justified.
        return DoctorCheck::new(
            "worktree_disk",
            CheckStatus::Unknown,
            format!(
                "{measured}{undercount}{window}; total disk use is therefore UNDETERMINED (#2919)"
            ),
        );
    }
    DoctorCheck::new(
        "worktree_disk",
        CheckStatus::Ok,
        format!("{measured}{undercount}{window}; none currently reclaimable"),
    )
}

/// Probe worktree disk consumption and merged-PR reclaimability (#2919).
///
/// Why: see the module doc — the existing `worktrees` probe reports counts, not
/// bytes, and never noticed a terabyte.
/// What: runs the read-only survey on a blocking thread (it shells out and
/// walks the filesystem) under [`SURVEY_TIMEOUT`], then renders it with
/// [`build_worktree_disk_check`]. A missing or absent `repos_root` is `Ok` —
/// an operator who runs no managed sessions has no worktrees to account for. A
/// panicked or timed-out survey is `Unknown`, never `Ok`: a probe that learned
/// nothing must not read healthy.
/// Test: `worktree_disk_check_is_ok_without_a_repos_root`.
pub(super) async fn check_worktree_disk(
    repos_root: Option<&Path>,
    active_workspace_paths: &[PathBuf],
) -> DoctorCheck {
    let Some(root) = repos_root else {
        return DoctorCheck::new(
            "worktree_disk",
            CheckStatus::Ok,
            "no managed workspace root configured — worktree disk accounting skipped",
        );
    };
    if !root.exists() {
        return DoctorCheck::new(
            "worktree_disk",
            CheckStatus::Ok,
            format!(
                "{} does not exist — no worktrees to account for",
                root.display()
            ),
        );
    }
    let root = root.to_path_buf();
    let active: Vec<PathBuf> = active_workspace_paths.to_vec();
    // #2919: the deadline is passed INTO the blocking task, not wrapped around
    // it. `tokio::time::timeout` cannot cancel `spawn_blocking`, so an outer
    // timeout returns a verdict on schedule while the walk keeps running — and
    // runtime shutdown then waits for that thread. Measured during this change:
    // two doctor tests each left a walk running over the operator's real ~1 TiB
    // worktree store and the binary hung for over twenty minutes. The task
    // bounds itself; the outer timeout is only a backstop for the `git`/`gh`
    // subprocesses, which a deadline check cannot interrupt mid-call.
    let deadline = std::time::Instant::now() + SURVEY_TIMEOUT;
    let budget = SurveyBudget {
        measure: Some(SURVEY_TIMEOUT),
        classify: Some(deadline),
    };
    // #2919: `per_branch_fallback: false`. Resolving a truncated index costs one
    // network call PER unresolved branch, which cannot fit a 3-second budget —
    // the probe discloses the truncation instead, and the operator-invoked
    // reclaim path does the per-branch work.
    let joined = tokio::time::timeout(
        SURVEY_TIMEOUT + SURVEY_TIMEOUT + SURVEY_TIMEOUT_GRACE,
        // #5661: a REPORT-only probe with no delegation registry to consult, so
        // it says so — `Unknown` refuses, which keeps an agent-owned worktree
        // out of the reclaimable count `tm doctor` prints and out of the command
        // it advertises. Matching `worktree_reconcile::classify`, which has
        // never reported an agent tree reclaimable either.
        tokio::task::spawn_blocking(move || {
            survey(
                &root,
                &active,
                &|_| AgentDelegationState::Unknown,
                budget,
                false,
            )
        }),
    )
    .await;
    match joined {
        Ok(Ok(result)) => build_worktree_disk_check(&result),
        Ok(Err(e)) => {
            tracing::error!("doctor: worktree disk survey panicked: {e}");
            DoctorCheck::new(
                "worktree_disk",
                CheckStatus::Unknown,
                format!("worktree disk survey panicked ({e}) — disk state undetermined"),
            )
        }
        Err(_) => DoctorCheck::new(
            "worktree_disk",
            CheckStatus::Unknown,
            format!(
                "worktree disk survey exceeded its {}s ceiling ({}s classify + {}s \
                 measure + {}s subprocess grace) — disk state undetermined (#2919)",
                (SURVEY_TIMEOUT + SURVEY_TIMEOUT + SURVEY_TIMEOUT_GRACE).as_secs(),
                SURVEY_TIMEOUT.as_secs(),
                SURVEY_TIMEOUT.as_secs(),
                SURVEY_TIMEOUT_GRACE.as_secs()
            ),
        ),
    }
}

#[cfg(test)]
#[path = "doctor_worktree_disk_tests.rs"]
mod doctor_worktree_disk_tests;
