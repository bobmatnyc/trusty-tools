//! Gathering the facts the Disk survey classifies (#6927).
//!
//! Why: split from [`super::survey`] so the DECISION stays pure and testable
//! and this file owns the I/O — the git scan, the pull-request lookup, the dirt
//! probe, and the byte index. It is also what keeps both files under the
//! 500-SLOC production cap.
//! What: [`run`] walks [`scan_registered_worktrees`], classifies each worktree
//! twice — once with `worktree_reclaim::classify` for the authoritative reclaim
//! verdict, once with [`classify_tier`] for the tier the dashboard renders —
//! measures bytes through the caller's [`DirSizeIndex`], and groups the rows by
//! managed project.
//! Test: `super::survey_tests`.
//!
//! # Every input is injected
//!
//! [`DiskProbes`] carries the pull-request lookup, the live-claim set, the
//! agent-liveness probe, and the dirt probe. None of them is resolved here, so
//! a test runs the whole survey over scratch git repositories in a tempdir with
//! no `gh`, no daemon, and no reads of the operator's real workspace.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use super::size_index::{DirSize, DirSizeIndex};
use super::survey::{
    Classification, DiskProject, DiskRoot, DiskSurvey, DiskWorktree, KeepListReport, PrRef, Reason,
    ReasonCode, SizeNote, TierCounts, WorktreeFacts, WorktreeTier, classify_tier,
};
use crate::session_manager::worktree_keep_list::KeepList;
use crate::session_manager::worktree_reclaim::{
    AgentStateProbe, BranchPrState, LiveClaims, ReclaimGate, ReclaimVerdict, classify,
};
use crate::session_manager::worktree_registry::{ScannedWorktree, scan_registered_worktrees};
use crate::session_manager::worktree_safety::DirtyWorktree;

/// The probes one survey run reads its facts through.
///
/// Why: see the module docs — injection is what makes the run hermetic. It is
/// also what lets the daemon hand in a `gh`-backed index while `tm doctor` or a
/// test hands in a fixed answer.
/// What: the five fact sources. `pr_state` is called once per worktree with
/// the scan record, so an implementation may batch per repository behind it.
/// `measure` is a probe rather than a borrowed
/// [`DirSizeIndex`](super::size_index::DirSizeIndex) for a reason #6927's
/// review found: the daemon's index is shared behind a mutex, and taking that
/// borrow for the whole pass held the lock across every `git` and `gh`
/// subprocess `classify` runs, serialising a second `disk_survey` (or the
/// #6926 refresher) behind minutes of network work. Measuring through a
/// closure lets the caller hold the lock for the measurement alone.
pub(crate) struct DiskProbes<'a> {
    /// The pull-request state of one scanned worktree's branch.
    pub pr_state: &'a dyn Fn(&ScannedWorktree) -> BranchPrState,
    /// Every workspace claim live sessions hold, plus the caller's identity.
    pub claims: &'a LiveClaims,
    /// The delegation registry's answer for the agent a sentinel names.
    pub agent_state: AgentStateProbe<'a>,
    /// Whether a directory holds work a removal would destroy.
    pub dirt: &'a dyn Fn(&Path) -> Option<DirtyWorktree>,
    /// Bytes under a directory, or `None` when no figure could be obtained.
    ///
    /// See the struct docs for why this is a probe. [`measure`] is the
    /// implementation every caller wraps.
    ///
    /// The second argument is the wall-clock budget this ONE measurement may
    /// spend: the time left before the survey's deadline, or `None` when the
    /// survey has no deadline and the index's own policy ceiling stands. It
    /// exists because the index's ceiling is fixed at 30 seconds, so a cold
    /// walk started late in a 30-second survey overran the survey and the
    /// console's stdio transport returned a 502 with no survey at all (#6929).
    pub measure: &'a dyn Fn(&Path, Option<Duration>) -> Option<DirSize>,
}

/// What one deadline-gated measurement produced.
///
/// Why this is not just `Option<DirSize>`: an absent figure has two causes that
/// must not be confused. The index having no number to give is ordinary and
/// leaves the row otherwise intact; the survey deadline having passed means the
/// row's tier was computed from facts we can no longer pair a byte figure with,
/// and the row must fall back to [`not_inspected`] whole (#6929).
enum Measured {
    /// The index answered — with a figure, or with `None` for no figure.
    Figure(Option<DirSize>),
    /// The deadline had already passed when the measurement was reached.
    DeadlineSpent,
}

impl Measured {
    /// The figure, for a caller that has no tier to invalidate.
    ///
    /// Projects and the root are aggregates, not classified rows: an unmeasured
    /// one reports `bytes: null` and says nothing that a byte figure could
    /// contradict, so a spent deadline and no-figure collapse to the same
    /// thing here.
    fn figure(self) -> Option<DirSize> {
        match self {
            Self::Figure(size) => size,
            Self::DeadlineSpent => None,
        }
    }
}

/// Survey every worktree under `repos_root` for the Disk dashboard (#6927).
///
/// Why: this is the tool's whole body, and it is READ-ONLY by construction —
/// nothing here removes, prunes, or writes anything. DOC-73 §16.6 item 4 owns
/// the clear action.
/// What: scans git's own registry (ADR-0023: git decides existence, never a
/// directory walk), classifies each worktree, measures bytes through `index`
/// (never a second walker — see #6926), and groups rows under their managed
/// project. `deadline`, when set, stops the WHOLE pass — classification and
/// byte measurement both. Remaining worktrees are still listed, as `review`
/// with an `unknown-branch-state` reason, never omitted and never `stale`; a
/// project or root the budget was reached before reports `bytes: null`, which
/// the console renders as unmeasured.
///
/// Measurement is deadline-gated because it is the expensive half, not the
/// cheap one (#6929). The workspace root used to be walked FIRST, to warm the
/// index's directory cache — but a cold root walk saturates the index's own
/// 30 s walk budget by itself, and the console's stdio MCP transport cuts the
/// whole call off at 30 s. Every `disk_survey` the console issued came back a
/// 502 with no survey at all, a call scoped to one project included, because
/// the root walk ran before the filter could narrow anything. Worktrees are
/// measured first now, then projects, then the root, so a budget-limited pass
/// spends what it has on the rows the view actually colours.
///
/// The deadline bounds each measurement as well as gating it: a walk is handed
/// the time actually left, never the index's fixed 30-second ceiling, so no one
/// measurement can outlive the survey (#6929).
/// `project` filters to one managed project by name or by path prefix.
/// Test: `a_survey_groups_worktrees_under_their_project_and_measures_bytes`,
/// `a_survey_past_its_deadline_still_lists_every_worktree`,
/// `a_deadline_that_crosses_mid_inspection_yields_a_not_inspected_row`,
/// `the_survey_hands_each_measurement_only_the_time_left`,
/// `a_survey_serializes_the_documented_shape`.
pub(crate) fn run(
    repos_root: &Path,
    keep_list: &KeepList,
    probes: &DiskProbes<'_>,
    deadline: Option<Instant>,
    project: Option<&str>,
) -> DiskSurvey {
    // #6929: no measurement is attempted once the budget is spent, and one that
    // IS attempted gets only the time actually left — a walk may not outlive
    // the survey that asked for it.
    let expired = || deadline.is_some_and(|d| Instant::now() >= d);
    let measure = |path: &Path| -> Measured {
        // ONE clock read decides both halves. Reading it twice — once to ask
        // whether to measure, once to work out the budget — is the same split
        // the review found between `inspect`'s two phases.
        let left = match deadline {
            None => None,
            Some(d) => match d.checked_duration_since(Instant::now()) {
                Some(left) if !left.is_zero() => Some(left),
                _ => return Measured::DeadlineSpent,
            },
        };
        Measured::Figure((probes.measure)(path, left))
    };
    let mut grouped: BTreeMap<PathBuf, Vec<DiskWorktree>> = BTreeMap::new();
    let cache: RefCell<HashMap<PathBuf, Option<DirtyWorktree>>> = RefCell::new(HashMap::new());
    // One dirt probe per worktree, shared by the reclaim classifier and the
    // tier classifier. Without the memo the same `git status` would run twice
    // per worktree, and it is the most expensive probe of the three.
    let probe_dirt = |path: &Path| -> Option<DirtyWorktree> {
        if let Some(hit) = cache.borrow().get(path) {
            return hit.clone();
        }
        let answer = (probes.dirt)(path);
        cache
            .borrow_mut()
            .insert(path.to_path_buf(), answer.clone());
        answer
    };

    for scanned in scan_registered_worktrees(repos_root) {
        if !selected(&scanned, repos_root, project) {
            continue;
        }
        let row = if expired() {
            // Fail closed, exactly as the reclaim survey does: a worktree we
            // ran out of time to inspect is LISTED and never advertised as
            // clearable.
            not_inspected(&scanned)
        } else {
            inspect(&scanned, keep_list, probes, &probe_dirt, &measure)
        };
        grouped
            .entry(scanned.project.clone())
            .or_default()
            .push(row);
    }

    let mut counts = TierCounts::default();
    let mut stale_bytes = 0u64;
    let mut stale_measured = 0usize;
    let mut projects = Vec::new();
    for (path, worktrees) in grouped {
        for wt in &worktrees {
            match wt.tier {
                WorktreeTier::Stale => {
                    counts.stale += 1;
                    if let Some(bytes) = wt.bytes {
                        stale_bytes = stale_bytes.saturating_add(bytes);
                        stale_measured += 1;
                    }
                }
                WorktreeTier::Review => counts.review += 1,
                WorktreeTier::Keep => counts.keep += 1,
                WorktreeTier::Missing => counts.missing += 1,
            }
        }
        let size = measure(&path).figure();
        projects.push(DiskProject {
            name: project_name(&path, repos_root),
            bytes: size.as_ref().map(|s| s.bytes),
            size: size.as_ref().map(note),
            path,
            worktrees,
        });
    }

    // Measured LAST: the root walk is the most expensive in the pass and the
    // least informative, so it gets whatever budget the worktrees left.
    let root_size = measure(repos_root).figure();
    DiskSurvey {
        generated_at: Utc::now().to_rfc3339(),
        keep_list: KeepListReport {
            patterns: keep_list.patterns().to_vec(),
            invalid: keep_list.invalid().to_vec(),
            error: keep_list.error().map(str::to_string),
        },
        root: DiskRoot {
            path: repos_root.to_path_buf(),
            bytes: root_size.as_ref().map(|s| s.bytes),
            size: root_size.as_ref().map(note),
            projects,
            counts,
            stale_bytes,
            stale_measured,
        },
    }
}

/// Classify and measure one scanned worktree.
///
/// Why: the two classifiers run over the SAME facts — one dirt probe, one claim
/// state, one pull-request answer — so the verdict the row reports and the tier
/// it renders can never have been computed from different observations.
///
/// Tier and bytes are atomic for the same reason (#6929 review). `classify` and
/// `classify_tier` shell out to `git` and `gh`, so the deadline can cross while
/// they run — after the loop admitted this worktree and before the measurement.
/// The row that produced was a full tier, `stale` and `reclaimable: true`
/// included, carrying `bytes: null`: the console would offer a clearable
/// worktree of unknown size, on the strength of a pass that had already run out
/// of time. A crossing found at the measurement therefore discards the tier too
/// and returns [`not_inspected`], which is what the loop's own check produces
/// when the clock crosses one instant earlier.
fn inspect(
    scanned: &ScannedWorktree,
    keep_list: &KeepList,
    probes: &DiskProbes<'_>,
    probe_dirt: &dyn Fn(&Path) -> Option<DirtyWorktree>,
    measure: &dyn Fn(&Path) -> Measured,
) -> DiskWorktree {
    let pr = (probes.pr_state)(scanned);
    let claim = probes.claims.claim_state(&scanned.path);
    let verdict = classify(
        &scanned.path,
        scanned.admission,
        &claim,
        &pr,
        probe_dirt,
        probes.agent_state,
        keep_list,
    );
    let facts = WorktreeFacts {
        path: &scanned.path,
        branch: scanned.branch.as_deref(),
        admission: scanned.admission,
        claim: &claim,
        pr: &pr,
        verdict: &verdict,
    };
    let classification = classify_tier(&facts, keep_list, probe_dirt);
    // A registration whose directory is gone has no bytes to measure, and
    // asking the index for one would only produce a `NotADirectory` refusal.
    // `missing` says that on its own, so it is not a deadline artifact and
    // keeps its tier.
    let size = if classification.tier == WorktreeTier::Missing {
        None
    } else {
        match measure(&scanned.path) {
            Measured::Figure(size) => size,
            Measured::DeadlineSpent => return not_inspected(scanned),
        }
    };
    row(scanned, classification, &verdict, &pr, &claim, size)
}

/// Assemble one row from the facts already gathered.
fn row(
    scanned: &ScannedWorktree,
    classification: Classification,
    verdict: &ReclaimVerdict,
    pr: &BranchPrState,
    claim: &crate::session_manager::worktree_reclaim_claim::ClaimState,
    size: Option<DirSize>,
) -> DiskWorktree {
    let (gate, reason) = match verdict {
        ReclaimVerdict::Reclaimable { .. } => (None, None),
        ReclaimVerdict::Blocked { gate, reason }
        | ReclaimVerdict::BlockedByAgent { gate, reason } => (Some(*gate), Some(reason.clone())),
    };
    DiskWorktree {
        id: scanned.path.to_string_lossy().into_owned(),
        path: scanned.path.clone(),
        branch: scanned.branch.clone(),
        tier: classification.tier,
        reasons: classification.reasons,
        gate,
        reason,
        reclaimable: verdict.is_reclaimable(),
        bytes: size.as_ref().map(|s| s.bytes),
        size: size.as_ref().map(note),
        pr: PrRef::from_state(pr),
        session: claiming_session(claim),
    }
}

/// The row for a worktree the classify deadline was reached before.
fn not_inspected(scanned: &ScannedWorktree) -> DiskWorktree {
    DiskWorktree {
        id: scanned.path.to_string_lossy().into_owned(),
        path: scanned.path.clone(),
        branch: scanned.branch.clone(),
        tier: WorktreeTier::Review,
        reasons: vec![Reason {
            code: ReasonCode::UnknownBranchState,
            detail: "the survey deadline was reached before this worktree was inspected"
                .to_string(),
        }],
        gate: Some(ReclaimGate::Deadline),
        reason: Some("survey deadline reached before inspection".to_string()),
        reclaimable: false,
        bytes: None,
        size: None,
        pr: None,
        session: None,
    }
}

/// The session id a claim carries, for the detail panel.
fn claiming_session(
    claim: &crate::session_manager::worktree_reclaim_claim::ClaimState,
) -> Option<String> {
    use crate::session_manager::worktree_reclaim_claim::ClaimState;
    match claim {
        ClaimState::Unclaimed => None,
        ClaimState::CallerNested { session }
        | ClaimState::CallerWorkspace { session }
        | ClaimState::Foreign { session, .. } => Some(session.clone()),
    }
}

/// Ask an index for a directory's bytes, treating a refusal as "no figure".
///
/// Why: [`DirSizeIndex::measure`] refuses a forbidden root and a
/// non-directory. Both are reported as an absent figure rather than as a failed
/// survey — this view is read-only, and a missing number is strictly better
/// than no view. The refusal is logged so an operator whose workspace root sits
/// somewhere the index will not walk can find out why.
/// What: the body every [`DiskProbes::measure`] implementation wraps. A caller
/// holding a shared index locks it around THIS call and nothing else — see the
/// [`DiskProbes`] docs for why the lock may not span the survey. `budget` is
/// the survey's remaining time, passed straight through so this one walk cannot
/// outlive the survey (#6929); `None` leaves the index's own ceiling standing.
/// Test: `a_survey_groups_worktrees_under_their_project_and_measures_bytes`,
/// `the_survey_hands_each_measurement_only_the_time_left`.
pub(crate) fn measure(
    index: &mut DirSizeIndex,
    path: &Path,
    budget: Option<Duration>,
) -> Option<DirSize> {
    match index.measure_within(path, budget) {
        Ok(size) => Some(size),
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "disk-survey: no byte figure (#6927)");
            None
        }
    }
}

/// Render a [`DirSize`]'s provenance for the payload.
fn note(size: &DirSize) -> SizeNote {
    SizeNote {
        from_cache: size.from_cache,
        truncated: size.truncated,
        measured_at: DateTime::<Utc>::from(size.measured_at).to_rfc3339(),
        unreadable: size.unreadable.len(),
    }
}

/// `<owner>/<repo>` for a managed project directory.
///
/// Why: the scan already establishes the managed project directory
/// (`<repos_root>/<owner>/<repo>`), and that IS the registered project root
/// DOC-73 §16.1 names. Deriving the label from it rather than joining against
/// the project registry keeps the survey answerable for a project the registry
/// has not been told about — which is every project on a host that registers
/// lazily.
fn project_name(project: &Path, repos_root: &Path) -> String {
    project
        .strip_prefix(repos_root)
        .unwrap_or(project)
        .to_string_lossy()
        .into_owned()
}

/// Whether the caller's `project` filter selects this worktree.
///
/// What: no filter selects everything; otherwise the filter matches the
/// project's `<owner>/<repo>` label, its bare repository name, or any prefix of
/// its absolute path — so an operator can pass either spelling.
fn selected(scanned: &ScannedWorktree, repos_root: &Path, project: Option<&str>) -> bool {
    let Some(filter) = project else {
        return true;
    };
    let name = project_name(&scanned.project, repos_root);
    name == filter
        || scanned
            .project
            .file_name()
            .is_some_and(|n| n.to_string_lossy() == filter)
        || scanned.project.starts_with(filter)
}
