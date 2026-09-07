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
use std::time::Instant;

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
/// What: the four fact sources. `pr_state` is called once per worktree with the
/// scan record, so an implementation may batch per repository behind it.
pub(crate) struct DiskProbes<'a> {
    /// The pull-request state of one scanned worktree's branch.
    pub pr_state: &'a dyn Fn(&ScannedWorktree) -> BranchPrState,
    /// Every workspace claim live sessions hold, plus the caller's identity.
    pub claims: &'a LiveClaims,
    /// The delegation registry's answer for the agent a sentinel names.
    pub agent_state: AgentStateProbe<'a>,
    /// Whether a directory holds work a removal would destroy.
    pub dirt: &'a dyn Fn(&Path) -> Option<DirtyWorktree>,
}

/// Survey every worktree under `repos_root` for the Disk dashboard (#6927).
///
/// Why: this is the tool's whole body, and it is READ-ONLY by construction —
/// nothing here removes, prunes, or writes anything. DOC-73 §16.6 item 4 owns
/// the clear action.
/// What: scans git's own registry (ADR-0023: git decides existence, never a
/// directory walk), classifies each worktree, measures bytes through `index`
/// (never a second walker — see #6926), and groups rows under their managed
/// project. `deadline`, when set, stops CLASSIFICATION: remaining worktrees are
/// still listed, as `review` with an `unknown-branch-state` reason, never
/// omitted and never `stale`.
/// `project` filters to one managed project by name or by path prefix.
/// Test: `a_survey_groups_worktrees_under_their_project_and_measures_bytes`,
/// `a_survey_past_its_deadline_still_lists_every_worktree`,
/// `a_survey_serializes_the_documented_shape`.
pub(crate) fn run(
    repos_root: &Path,
    keep_list: &KeepList,
    keep_patterns: &[String],
    probes: &DiskProbes<'_>,
    index: &mut DirSizeIndex,
    deadline: Option<Instant>,
    project: Option<&str>,
) -> DiskSurvey {
    // Measured first so the root walk populates the directory cache every
    // project and worktree measurement below then revalidates at one stat per
    // directory instead of re-walking.
    let root_size = measure(index, repos_root);
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
        let row = if deadline.is_some_and(|d| Instant::now() >= d) {
            // Fail closed, exactly as the reclaim survey does: a worktree we
            // ran out of time to inspect is LISTED and never advertised as
            // clearable.
            not_inspected(&scanned)
        } else {
            inspect(&scanned, keep_list, probes, &probe_dirt, index)
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
        let size = measure(index, &path);
        projects.push(DiskProject {
            name: project_name(&path, repos_root),
            bytes: size.as_ref().map(|s| s.bytes),
            size: size.as_ref().map(note),
            path,
            worktrees,
        });
    }

    DiskSurvey {
        generated_at: Utc::now().to_rfc3339(),
        keep_list: KeepListReport {
            patterns: keep_patterns.to_vec(),
            invalid: keep_list.invalid().to_vec(),
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
fn inspect(
    scanned: &ScannedWorktree,
    keep_list: &KeepList,
    probes: &DiskProbes<'_>,
    probe_dirt: &dyn Fn(&Path) -> Option<DirtyWorktree>,
    index: &mut DirSizeIndex,
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
    let size = if classification.tier == WorktreeTier::Missing {
        None
    } else {
        measure(index, &scanned.path)
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

/// Ask the index for a directory's bytes, treating a refusal as "no figure".
///
/// Why: [`DirSizeIndex::measure`] refuses a forbidden root and a
/// non-directory. Both are reported as an absent figure rather than as a failed
/// survey — this view is read-only, and a missing number is strictly better
/// than no view. The refusal is logged so an operator whose workspace root sits
/// somewhere the index will not walk can find out why.
fn measure(index: &mut DirSizeIndex, path: &Path) -> Option<DirSize> {
    match index.measure(path) {
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
