//! The orphan sweep's candidate scan and its result types (#1840, #3649).
//!
//! Why: split out of `prune.rs` by #8782, which added the scoped scan and would
//! otherwise have pushed that file past the 500-SLOC production cap.
//! What: [`find_orphaned_worktrees_in`] and its [`OrphanCandidates`],
//! [`OrphanSweepOutcome`], and the one-line [`SweepClassification`] summary.
//! `prune.rs` re-exports all of them, so no call site moved.
//! Test: `find_orphaned_worktrees_discovers_worktree_at_unwalked_location`,
//! `sweep_summary_is_silent_without_candidates`.

use std::collections::BTreeMap;

use super::super::worktree_safety::DirtyWorktree;
use super::super::worktree_scope::WorktreeScope;

/// Enumerate orphaned worktree reclaim candidates under `repos_root` (#1840,
/// rebuilt git-native #4207 slice 1).
///
/// Why: this function used to WALK the filesystem, probing five hard-coded
/// location shapes under each `<repos_root>/<owner>/<repo>/`. Every shape was
/// added by a bug report about a location the previous shape list had missed
/// (#3649, #3971, and the #3971 follow-up), and the list could never be
/// complete because nothing stops a worktree from being registered anywhere on
/// disk. Worse, the walk found DIRECTORIES, not worktrees, so it had no idea
/// which checkout owned any of them — the grandparent guess that filled that
/// gap left fourteen worktrees physically inside `.base` but registered to the
/// parent repo permanently unreclaimable (#4207). Git maintains the registry;
/// deriving from it deletes the whole category of missed-location bug.
/// What: delegates discovery to
/// [`super::super::worktree_registry::enumerate_registered_worktrees_in`] — every
/// worktree git itself registers inside a managed project, wherever in that
/// project it lives — then removes any whose path is in `active_set`.
/// Candidates come back canonicalized (so the active-set comparison is
/// symlink-safe), sorted, and de-duplicated. A non-existent or unreadable
/// `repos_root` yields an empty vec.
///
/// BOUNDARY (#4224 review, HIGH): "wherever it lives" is bounded — a candidate
/// must be a strict descendant of the managed project directory whose registry
/// named it. Location is irrelevant WITHIN a project (that is the #4207 fix);
/// it is decisive at the project edge, so an operator checkout parked beside a
/// project, or a worktree the operator registered outside it, is never a
/// candidate. See
/// [`super::super::worktree_registry::enumerate_registered_worktrees_in`].
///
/// A candidate here is still only a CANDIDATE: `prune_orphaned_worktrees`
/// applies the #3649 ownership-sentinel gate and the #4091 dirty-tree gate
/// before anything is deleted, so a Claude-Code-created worktree (which never
/// carries trusty-mpm's sentinel) lands in `owner_unknown` and is reported,
/// never removed.
///
/// SCOPE NOTE (#4207): a directory that git does NOT register — a husk left
/// behind by a half-finished removal, for instance — is no longer enumerated.
/// It was already unreclaimable before this change, because
/// `git_worktree_list_agrees` refused every such path; the difference is that
/// it is now also absent from the report. Reclaiming unregistered husks is a
/// separate concern (#3715), deliberately not smuggled in here.
///
/// #8782: `scope` bounds the scan ([`WorktreeScope::all`] for every project),
/// and each candidate keeps the checkout whose registry named it, so the prune
/// preview can print its project without asking git again.
/// Test: `prune_orphaned_worktrees_spares_active`,
///       `reap_orphaned_worktrees_removes_orphan_preserves_live`,
///       `find_orphaned_worktrees_discovers_worktree_at_unwalked_location`
///       (#4207 — fails against the five-shape walk),
///       `find_orphaned_worktrees_ignores_plain_directory`,
///       `an_orphan_sweep_scoped_to_one_project_spares_another`.
pub(crate) fn find_orphaned_worktrees_in(
    repos_root: &std::path::Path,
    active_set: &std::collections::HashSet<std::path::PathBuf>,
    // #7357: the repos-root walk reaches a project only at
    // `<repos_root>/<owner>/<repo>`; the adopted anchors carry the rest. They
    // are INJECTED rather than resolved here — resolving them internally makes
    // this function's own unit tests read the developer's real
    // `~/.trusty-mpm/project-registry/worktrees.json` and survey whatever real
    // worktrees it names. Entry points resolve; scans take. `&[]` is the
    // pre-#7357 behaviour exactly.
    adopted: &[std::path::PathBuf],
    scope: &WorktreeScope,
) -> OrphanCandidates {
    let mut found = OrphanCandidates::default();
    let rows = super::super::worktree_registry::enumerate_registered_worktrees_in(
        repos_root, adopted, scope,
    );
    for row in rows.into_iter().filter(|r| !active_set.contains(&r.path)) {
        found
            .registry_roots
            .insert(row.path.clone(), row.registry_root);
        found.paths.push(row.path);
    }
    found
}

/// The orphan scan's candidates, in removal order, with their registries (#8782).
#[derive(Debug, Clone, Default)]
pub(crate) struct OrphanCandidates {
    /// Candidate paths, nested worktrees before their parents.
    pub(crate) paths: Vec<std::path::PathBuf>,
    /// The checkout whose `git worktree list` named each path.
    pub(crate) registry_roots: BTreeMap<std::path::PathBuf, std::path::PathBuf>,
}

/// Outcome of an orphaned-worktree sweep (#3649): which candidates were (or
/// would be, under `dry_run`) removed vs. skipped because ownership could not
/// be established.
///
/// Why: the #3649 safe default — an owner-unknown worktree is NEVER
/// auto-deleted — must not just silently vanish from the caller's view; the
/// daemon's orphan-GC log line and `tm session prune-worktrees` both need to
/// see this count so operators know legacy worktrees are being conservatively
/// left in place for `tm doctor` / `--dry-run` review, not merely "not found".
/// What: `removed` — paths actually removed (or that WOULD be removed under
/// `dry_run`); `owner_unknown` — paths whose ownership sentinel had no
/// resolvable owner (absent, empty/legacy, or unparsable content);
/// `skipped_dirty` (#4091) — paths whose owner WAS resolvable and provably
/// gone, but which still hold uncommitted or unpushed work (or whose
/// dirty-check could not complete), each with the reason and counts behind
/// the decision so no skip is ever silent.
/// Test: `prune_orphaned_worktrees_skips_owner_unknown`,
///       `prune_orphaned_worktrees_reclaims_terminal_owner`,
///       `prune_orphaned_worktrees_skips_modified_tracked_file` (#4091).
#[derive(Debug, Clone, Default)]
pub struct OrphanSweepOutcome {
    /// Paths actually removed (or that would be removed under `dry_run`).
    pub removed: Vec<std::path::PathBuf>,
    /// Paths skipped because their sentinel's owner could not be resolved —
    /// never auto-deleted; surfaced here for `tm doctor`/manual review.
    pub owner_unknown: Vec<std::path::PathBuf>,
    /// Paths skipped because they hold unsaved work (#4091) — never
    /// auto-deleted under the default [`DirtyWorktreePolicy::Skip`](super::super::DirtyWorktreePolicy::Skip).
    pub skipped_dirty: Vec<DirtyWorktree>,
    /// Paths owned by a dispatched agent (#4311) — reclaimed by that agent's
    /// exit, never by this sweep.
    ///
    /// Why this is reported rather than merely skipped: before #4311 these
    /// carried no sentinel and landed in `owner_unknown`, so they were
    /// unreclaimable but VISIBLE in `--dry-run`, the prune HTTP route, the MCP
    /// tool, and `tm doctor`. Attributing them must not cost an operator that
    /// view — a directory that vanishes from every report is worse than one
    /// reported as unreclaimable.
    /// Test: `prune_orphaned_worktrees_skips_an_agent_owned_worktree`.
    pub agent_owned: Vec<std::path::PathBuf>,
    /// The checkout whose registry named each candidate (#8782) — the prune
    /// preview's project column, carried from the scan so the route runs no
    /// `git` per row.
    pub registry_roots: BTreeMap<std::path::PathBuf, std::path::PathBuf>,
    /// Paths in `removed` whose unsaved work `--discard-dirty` discards (#8782).
    /// Always empty under [`DirtyWorktreePolicy::Skip`](super::super::DirtyWorktreePolicy::Skip),
    /// where such a tree lands in `skipped_dirty` instead.
    /// Test: `the_orphan_preview_names_unsaved_work_that_discard_dirty_destroys`.
    pub discarded_dirty: Vec<DirtyWorktree>,
    /// Candidates git failed on after deleting some or all of their content,
    /// each as `"<path>: <report>"` (#8782). Neither removed nor kept.
    pub partially_removed: Vec<String>,
}

/// Per-sweep classification counts for the orphan sweep's one log line (#4323).
///
/// Why: the classification loop logged one `info!` per candidate on every
/// candidate, on every 60s sweep. The backlog it reports is durable by design —
/// an owner-unknown worktree is never auto-deleted — so 193 of them wrote
/// 208,308 identical lines into a single day's log (61 MB). A count is the whole
/// signal an operator acts on; the paths themselves are a `debug!` away and are
/// reported structurally in [`OrphanSweepOutcome`] regardless. Kept as a pure
/// value with a pure renderer so the suppression rule is testable without a
/// tracing subscriber.
/// What: the six counts one classification pass produces. [`summary`](Self::summary)
/// renders the line, or `None` when there were no candidates at all — the idle
/// case, where even one line per minute is noise.
///
/// One knowing loss: `skipped_live` is a bare count, so the PATHS of
/// still-live-owner candidates are reachable at `debug` only. The other four
/// classifications keep their paths in [`OrphanSweepOutcome`], which the HTTP
/// route, the MCP tool and `tm doctor` all read; this arm has no such carrier
/// and is deliberately not given one — a live owner's worktree is not a backlog
/// an operator acts on, and adding a field to that public struct would churn
/// its literal constructions for a diagnostic that `RUST_LOG=debug` already
/// answers.
/// Test: `sweep_summary_is_silent_without_candidates`,
/// `sweep_summary_names_every_count`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct SweepClassification {
    /// Registered worktrees not claimed by any live session this pass.
    pub(super) candidates: usize,
    /// Candidates whose ownership sentinel named no resolvable owner (#3649).
    pub(super) owner_unknown: usize,
    /// Candidates attributed to a dispatched agent (#4311).
    pub(super) agent_owned: usize,
    /// Candidates whose owner is still live/resumable, or too young to rule out
    /// a creation race — the arm [`OrphanSweepOutcome`] does not report.
    pub(super) skipped_live: usize,
    /// Candidates holding uncommitted or unpushed work (#4091).
    pub(super) skipped_dirty: usize,
    /// Candidates that cleared every gate.
    pub(super) reclaimable: usize,
}

impl SweepClassification {
    /// Render the one-line sweep summary, or `None` when there was nothing to
    /// classify.
    ///
    /// Why: see [`SweepClassification`]. Returning `Option` rather than logging
    /// keeps the "idle sweep says nothing" rule assertable.
    /// What: `None` when `candidates == 0`; otherwise one line naming every
    /// count.
    /// Test: `sweep_summary_is_silent_without_candidates`,
    /// `sweep_summary_names_every_count`.
    pub(super) fn summary(&self) -> Option<String> {
        if self.candidates == 0 {
            return None;
        }
        Some(format!(
            "prune-worktrees: classified {} candidate(s) — {} reclaimable, \
             {} owner-unknown, {} agent-owned, {} owner-live, {} dirty \
             (per-path detail at debug; #4323)",
            self.candidates,
            self.reclaimable,
            self.owner_unknown,
            self.agent_owned,
            self.skipped_live,
            self.skipped_dirty,
        ))
    }
}

#[cfg(test)]
mod sweep_summary_tests {
    use super::SweepClassification;

    /// #4323: an idle daemon must log nothing here — a line per minute for a
    /// sweep that found nothing is the same accumulation in slower motion.
    #[test]
    fn sweep_summary_is_silent_without_candidates() {
        assert_eq!(SweepClassification::default().summary(), None);
    }

    /// #4323: every classification the loop makes must appear in the one line
    /// that replaced the per-path logs — including `skipped_live`, which
    /// `OrphanSweepOutcome` does not carry, so this line is its only report.
    #[test]
    fn sweep_summary_names_every_count() {
        let line = SweepClassification {
            candidates: 200,
            owner_unknown: 193,
            agent_owned: 3,
            skipped_live: 2,
            skipped_dirty: 1,
            reclaimable: 1,
        }
        .summary()
        .expect("a sweep with candidates reports");
        for (label, count) in [
            ("candidate(s)", "200"),
            ("owner-unknown", "193"),
            ("agent-owned", "3"),
            ("owner-live", "2"),
            ("dirty", "1"),
        ] {
            assert!(
                line.contains(count) && line.contains(label),
                "summary must name {label}={count}: {line}"
            );
        }
    }
}
